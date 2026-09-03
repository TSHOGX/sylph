//! Reading `claude`'s native store: one JSONL transcript per session under
//! `~/.claude/projects/`.
//!
//! `docs/agents.md` § `claude` governs. Two record mappings here are the ones a reader is
//! most likely to get wrong, because the wrong version raises no error and produces
//! plausible records: `defect_tool_use_id` and `defect_is_error` name the tests that pin
//! them, each asserting **the value that should have been found** rather than the absence
//! of a crash (`AGENTS.md` § Reading a vendor payload).
//!
//! # There is one projection, not two
//!
//! The vendor→Sylph mapping lives in [`crate::claude::translate`] and **this module does
//! not repeat it.** A native record is fed to the same [`Correlation`] a live turn drives,
//! after being reshaped into the payload the live stream would have carried. That is the
//! whole design decision here, and the alternative is what this rewrite exists to delete:
//! `AGENTS.md` § Compatibility records a separate per-vendor normalizer drifting from the
//! live one, and one vocabulary with two producers is how that started.
//!
//! It is also what makes the corrective half cheap. `translate::Correlation::tool_results`
//! already reads a result's id and error flag off the content block, so routing native
//! `user` records through it fixes both defects at once — a native `user` record and a
//! live `user` payload are the *same shape*, which is the fact that makes the reuse sound
//! rather than merely convenient.
//!
//! # What the store holds
//!
//! *Layout measured 2026-08-28 over `~/.claude/projects/`.* All four shapes occur in
//! quantity, and a parser has to handle each: a delegate and a workflow phase are ordinary
//! session files in a nested directory, not a different format.
//!
//! ```text
//! <project>/<session-uuid>.jsonl                                     the session
//! <project>/<session-uuid>/subagents/agent-<hash>.jsonl              a delegate
//! <project>/<session-uuid>/subagents/agent-<hash>.meta.json          its sidecar
//! <project>/<session-uuid>/workflows/wf_<id>.json                    workflow manifest
//! <project>/<session-uuid>/subagents/workflows/wf_<id>/agent-*.jsonl a workflow's delegate
//! <project>/<session-uuid>/subagents/workflows/wf_<id>/journal.jsonl phase outcomes
//! ```
//!
//! The root is hardcoded: a native store's location is a vendor fact with no configuration
//! key, and inventing one would offer a lever that cannot be right.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[cfg(test)]
use anyhow::Context;
use anyhow::{anyhow, Result};
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use serde_json::{json, Value};
use sylph_protocol::{
    normalize_workdir, Event, EventKind, EventStream, Fidelity, NativeBlob, NativeId, NativeRecord,
    NativeSummary, Phase, SessionMessage, StoreFilter,
};

use super::translate::Correlation;
#[cfg(test)]
use crate::native::for_each_json_line;
use crate::native::{candidates, epoch_millis_from_iso, SessionCandidate};

/// Where `claude` keeps its transcripts, `$HOME`-relative.
fn store_root() -> Option<PathBuf> {
    crate::native::home_relative(&[".claude", "projects"])
}

/// The fidelity a parsed `claude` session yields.
///
/// [`Fidelity::Blocks`] rather than [`Fidelity::Messages`], and it is measured rather
/// than assumed: the store flushes one record per content block, so an assistant message
/// with prose and two tool calls arrives as three records with three distinct timestamps
/// (`ordinary-turn-session-start.jsonl` holds exactly that, four records inside 662 ms
/// sharing one `message.id`). What is *not* recoverable is anything within a block, which
/// is what separates this from [`Fidelity::Stream`] — so a renderer may time a block and
/// must not time a token.
const NATIVE_FIDELITY: Fidelity = Fidelity::Blocks;

/// Parse everything the store already holds for one session, once.
///
/// The stream ends when the pass does.
pub fn adopt(id: &NativeId) -> Result<EventStream> {
    let session = locate(id)?;
    // Snapshot and follow share the same source inventory, cursor and projection state.
    // Keeping adoption on the bundle reader is important: a reload must not silently
    // traverse a different set of files than a live watcher.
    let mut reader = BundleFollowState::new(session, RootSource::File);
    let messages = reader.poll()?;
    Ok(Box::pin(futures::stream::iter(
        messages.into_iter().map(Ok),
    )))
}

/// Parse, then follow the complete native bundle as it grows.
pub fn watch(id: &NativeId) -> Result<EventStream> {
    let session = locate(id)?;
    Ok(follow(session, RootSource::File))
}

/// Follow a native bundle alongside Claude's live stdout.
///
/// # What the second source may repeat, and how that is decided
///
/// Built-in `Task` output is forwarded on stdout *and* written to a delegate file, so its
/// file copy would be a duplicate. `Agent` and `Workflow` forward nothing (`docs/agents.md`
/// § `--forward-subagent-text` forwards only for the built-in `Task`), so for them the file
/// is the **only** source and suppressing it loses the work this reader exists to recover.
///
/// The discriminant is therefore the **host call's tool name**, which is a fact recorded in
/// the root transcript before the delegate file exists. It is read from the records this
/// follower already parses for the workflow join, so no state is shared with the stdout pump
/// and there is nothing to race: an `Agent` cannot be mistaken for a `Task` by arriving in a
/// different order, because the order of two files is not what answers the question.
///
/// The rejected alternative was a set of live trajectory ids written by the stdout pump and
/// read here. It answered "has stdout mentioned this yet?", which is a question about
/// timing rather than about the vendor, so a delegate file polled before its notification
/// arrived would answer it wrongly — and it required sharing a mutex across the two, which
/// is what made the reader abort on a runtime thread.
pub fn watch_live(
    id: &NativeId,
    resumed: bool,
    root_correlation: std::sync::Arc<std::sync::Mutex<Correlation>>,
) -> Result<EventStream> {
    let session = locate(id)?;
    Ok(follow_from_with_correlation(
        session,
        RootSource::Stdout,
        if resumed {
            FollowBaseline::Current
        } else {
            FollowBaseline::Empty
        },
        Some(root_correlation),
    ))
}

/// Which native sessions this machine's store holds.
pub fn enumerate(filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
    let Some(root) = store_root() else {
        return Ok(Vec::new());
    };
    if !root.exists() {
        // A machine with the CLI installed and no sessions yet. An empty listing is the
        // truthful answer, not an error.
        return Ok(Vec::new());
    }

    let mut found = Vec::new();
    for candidate in candidates(&root, is_root_transcript) {
        let Some(summary) = summarize(&candidate) else {
            continue;
        };
        if let Some(wanted) = &filter.workdir {
            // **An exact path match after normalizing trailing slashes, never a
            // substring.** The store this replaces compiled the filter to
            // `LIKE '%' || ? || '%'`, so asking for `/w/app` also returned `/w/app-old`
            // and every subdirectory (`StoreFilter::workdir`).
            let Some(actual) = &summary.workdir else {
                continue;
            };
            if normalize_workdir(actual) != normalize_workdir(wanted) {
                continue;
            }
        }
        if let Some(since) = filter.since_ms {
            // The session's own last activity when it recorded one, and the file's mtime
            // otherwise — an absent end is not an ended session, so falling back to the
            // filesystem keeps a still-being-written session from being filtered out of
            // every listing that names a window.
            let last = summary.ended_at_ms.unwrap_or(candidate.modified_at_ms);
            if last < since {
                continue;
            }
        }
        found.push(summary);
    }

    // `candidates` already ordered by mtime; re-sorting on the session's own recorded end
    // would reorder by a field 9 of 999 sessions do not have.
    if let Some(limit) = filter.limit {
        found.truncate(limit);
    }
    Ok(found)
}

/// A trailing slash is not a different directory.
/// Whether this path is a session's own transcript, as opposed to a delegate's.
///
/// A delegate is **not** a session and must not be enumerated as one. That is measured
/// rather than stylistic: every one of 400 sampled `agent-<hash>.jsonl` files carries its
/// *parent's* `sessionId` on every record, so a delegate has no session id of its own and
/// `claude -r <id>` has nothing to resume it by. Listing one would produce a row that
/// cannot be adopted, cannot be resumed, and duplicates its parent's identity — the
/// two-id-spaces failure `sylph_protocol::NativeId` exists to prevent. Delegates reach a
/// reader as delegated trajectories inside their parent, which is what they are.
fn is_root_transcript(path: &Path) -> bool {
    if path.extension().is_none_or(|ext| ext != "jsonl") {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // `agent-*.jsonl` is a delegate and `journal.jsonl` is a workflow's phase log; both
    // are interior structure, read through their parent.
    if name.starts_with("agent-") || name == "journal.jsonl" {
        return false;
    }
    // A session's transcript sits directly in its project directory. Anything nested
    // deeper is interior structure whose name happens not to match the two rules above —
    // measured, one `…orphaned-<epoch>-<hash>.jsonl` exists at the top level and is a real
    // session, while nothing legitimate at depth 2 is.
    true
}

/// One session's files: its transcript, delegates, workflow manifests, and journals recorded
/// beside it.
#[derive(Debug, Clone)]
struct SessionFiles {
    transcript: PathBuf,
    /// `agent-<hash>.jsonl` under `subagents/`, including a workflow's, flattened.
    ///
    /// **Flat, because the store is flat.** Depth is expressed by `parentAgentId` inside
    /// each sidecar, **never by directory nesting**: reading one path component up to find a
    /// parent attributes every delegate to the root session and flattens a real depth-2 tree
    /// into one level.
    delegates: Vec<PathBuf>,
    /// `subagents/workflows/wf_<id>/journal.jsonl`.
    journals: Vec<PathBuf>,
    /// `workflows/wf_<id>.json`, the snapshot that joins a workflow run to its phase agents.
    manifests: Vec<PathBuf>,
}

/// Find one session's files by its native id.
fn locate(id: &NativeId) -> Result<SessionFiles> {
    let root = store_root()
        .ok_or_else(|| anyhow!("HOME is not set, so ~/.claude/projects cannot be located"))?;
    let wanted = format!("{}.jsonl", id.as_str());

    // The project directory is a mangled `cwd` and is **not derivable from the session
    // id**, so the store has to be walked. Measured over the whole store and well under the
    // per-session parse cost, which is why there is no index here to keep in sync.
    let transcript = candidates(&root, is_root_transcript)
        .into_iter()
        .find(|candidate| {
            candidate.path.file_name().and_then(|name| name.to_str()) == Some(wanted.as_str())
        })
        .map(|candidate| candidate.path)
        .ok_or_else(|| anyhow!("no `claude` session {} in {}", id.as_str(), root.display()))?;

    Ok(collect(transcript))
}

/// The delegates, workflow manifests, and journals sitting beside one transcript.
fn collect(transcript: PathBuf) -> SessionFiles {
    // `<session>.jsonl` has its delegates under `<session>/subagents/`.
    let interior = transcript.with_extension("");
    let mut delegates = Vec::new();
    let mut journals = Vec::new();
    let mut manifests = Vec::new();
    if interior.is_dir() {
        for candidate in candidates(&interior, |path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
        }) {
            let name = candidate
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name == "journal.jsonl" {
                journals.push(candidate.path);
            } else if name.starts_with("agent-") {
                delegates.push(candidate.path);
            }
        }
    }
    // A workflow manifest is a snapshot rewritten in place under `workflows/`, rather than
    // JSONL inside `subagents/`. Keep the directory boundary explicit so a nested sidecar
    // named `wf_*.json` cannot be mistaken for the run manifest.
    let workflow_dir = interior.join("workflows");
    if workflow_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&workflow_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_manifest = entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
                    && path.extension().is_some_and(|ext| ext == "json")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("wf_"));
                if is_manifest {
                    manifests.push(path);
                }
            }
        }
    }
    // Oldest first, so a delegate that spawned another is parsed before its child and the
    // trajectory tree is built in one pass rather than needing a second attachment pass.
    delegates.sort();
    journals.sort();
    manifests.sort();
    SessionFiles {
        transcript,
        delegates,
        journals,
        manifests,
    }
}

/// One listing row, from the head and tail of a transcript rather than a full parse.
///
/// # Why a window and not a parse
///
/// [`sylph_protocol::Adapter::enumerate`] must answer "what is on this machine" **without
/// parsing every session** — that is the requirement rather than an optimization. Measured
/// on this store: a full JSON parse of the 999 root transcripts (0.80 GB, 235694 records)
/// costs **3.0 s**, while a 32 KB head plus a 64 KB tail per file costs **163 ms**, about
/// 18× cheaper.
///
/// The window size is measured, not guessed. `ai-title` is written after the opening
/// exchange, so how much head is read decides how many titles are found: 16 KB finds
/// 71.8% of the titles present, **32 KB finds 94.7%**, 128 KB finds 97.2% and 512 KB finds
/// 99.0% for seven times the cost. 32 KB is where the curve bends, and the failure mode is
/// benign in the one direction that matters — a title this misses is `None`, which the
/// server's opening-ask fallback already handles, whereas a *wrong* title would be a lie
/// (`NativeSummary`: every field an adapter cannot answer is absent, not defaulted).
///
/// `None` when the file holds no `sessionId`, which is the one field with no fallback:
/// without it the row names no session and cannot be adopted.
fn summarize(candidate: &SessionCandidate) -> Option<NativeSummary> {
    /// Enough head for 94.7% of the titles the store holds. See above.
    const HEAD_BYTES: u64 = 32 * 1024;
    /// Enough tail to find the last timestamped record. Records run to a few KB.
    const TAIL_BYTES: u64 = 64 * 1024;

    let head = read_span(&candidate.path, 0, HEAD_BYTES)?;

    let mut native_id = None;
    let mut workdir = None;
    let mut title = None;
    let mut started_at_ms = None;
    let mut parent_native_id = None;

    for record in json_lines(&head, Edge::Head) {
        if native_id.is_none() {
            native_id = string_at(&record, "sessionId");
        }
        if workdir.is_none() {
            workdir = string_at(&record, "cwd");
        }
        if started_at_ms.is_none() {
            started_at_ms = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso);
        }
        // The vendor's own recorded title, in the three spellings it uses. Measured over
        // the whole store: `ai-title`/`aiTitle` on 934 of 999 root sessions,
        // `agent-name`/`agentName` on 51, `custom-title`/`customTitle` on 2 — so the first
        // is effectively the title and the other two exist. A user's explicit
        // `customTitle` outranks a generated one, which is why it is checked first.
        for (kind, field) in [
            ("custom-title", "customTitle"),
            ("ai-title", "aiTitle"),
            ("agent-name", "agentName"),
        ] {
            if record.get("type").and_then(Value::as_str) == Some(kind) {
                if let Some(text) = string_at(&record, field) {
                    title = Some(text);
                }
            }
        }
        // Session-level fork lineage. **`forkedFrom.sessionId` only** — see
        // [`fork_point_is_not_recoverable`] for why the sibling `messageUuid` is not a
        // branch point.
        if parent_native_id.is_none() {
            parent_native_id = record
                .get("forkedFrom")
                .and_then(|forked| string_at(forked, "sessionId"))
                .map(NativeId::new);
        }
    }

    let native_id = NativeId::new(native_id?);

    // The last record carrying one. Read from the tail because the alternative is reading
    // the whole file, and 15 of these transcripts are over 10 MB.
    let tail_from = candidate.size.saturating_sub(TAIL_BYTES);
    let tail = read_span(&candidate.path, tail_from, TAIL_BYTES).unwrap_or_default();
    let ended_at_ms = json_lines(
        &tail,
        if tail_from == 0 {
            Edge::Head
        } else {
            Edge::Tail
        },
    )
    .filter_map(|record| {
        record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_millis_from_iso)
    })
    .next_back();

    Some(NativeSummary {
        native_id,
        workdir,
        title,
        started_at_ms,
        // **Absent rather than the file's mtime.** An absent end is not an ended session
        // (`NativeSummary::ended_at_ms`), and mtime is a different fact: a store still
        // being written has one and no end stamp. `enumerate`'s `since` filter falls back
        // to mtime itself, where the substitution is local and visible.
        ended_at_ms,
        // Deliberately `None`: the honest answer costs a full parse, which is the 3.0 s
        // this function exists to avoid. `event_count`'s contract is "when the store can
        // say cheaply", and a record count is not an event count anyway — one assistant
        // record can hold several content blocks.
        event_count: None,
        parent_native_id,
        fidelity: NATIVE_FIDELITY,
    })
}

/// Which end of a file a byte span came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// Starts at a record boundary; the final line may be truncated.
    Head,
    /// Starts mid-record; the first line is a fragment and is dropped.
    Tail,
}

/// Read at most `len` bytes from `offset`.
fn read_span(path: &Path, offset: u64, len: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).ok()?;
    }
    let mut buffer = vec![0; len as usize];
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => break,
        }
    }
    buffer.truncate(filled);
    Some(buffer)
}

/// The whole JSON objects a byte span holds.
///
/// A span cut out of the middle of a file has a partial line at each end, and which end
/// depends on where the span started — so [`Edge`] says which fragment to drop rather than
/// this guessing. A line that does not parse is skipped for the same reason
/// [`for_each_json_line`] skips one: these are files the vendor is currently writing.
fn json_lines(bytes: &[u8], edge: Edge) -> impl DoubleEndedIterator<Item = Value> + use<'_> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if edge == Edge::Tail && !lines.is_empty() {
        // Began mid-record.
        lines.remove(0);
    }
    lines
        .into_iter()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect::<Vec<_>>()
        .into_iter()
}

/// The parsed JSON plus the exact line bytes that produced it. Raw retention uses the latter
/// so a future projection can recover lexical details serde_json normalizes away.
fn json_lines_with_raw(
    bytes: &[u8],
    edge: Edge,
) -> impl DoubleEndedIterator<Item = (Value, Vec<u8>)> + use<'_> {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if edge == Edge::Tail && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .into_iter()
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }
            let raw = line.to_vec();
            let trimmed = String::from_utf8_lossy(line);
            serde_json::from_str::<Value>(trimmed.trim())
                .ok()
                .map(|value| (value, raw))
        })
        .collect::<Vec<_>>()
        .into_iter()
}

/// A string field, treating `""` as absent.
///
/// Same rule as `translate::text_field`, for the same reason: the vendor sends `""` and
/// omits a field interchangeably, and every decision here wants one answer for both.
fn string_at(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// One session's whole recorded history, as the events a live turn would have emitted.
///
/// # The shape of the pass
///
/// The root transcript is parsed first, collecting workflow joins, then each delegate, then
/// the workflow journals. Each file gets its **own** [`Correlation`], because the correlation
/// state is per-stream: a
/// delegate's content-block indices are its own, and sharing one map across files would
/// reintroduce the index collision `translate::BlockKey` exists to prevent, here between
/// files rather than between concurrent delegates.
///
/// A delegate's events are then re-parented onto the trajectory that spawned it, which is
/// what [`delegate_spans`] resolves.
#[cfg(test)]
fn parse_session(session: &SessionFiles) -> Result<Vec<SessionMessage>> {
    let mut messages = Vec::new();

    let root = parse_transcript(&session.transcript, None)?;
    let spans = delegate_spans(session, &root.workflow_hosts);
    messages.extend(root.messages);

    for delegate in &session.delegates {
        let Some(agent) = agent_id_of(delegate) else {
            continue;
        };
        // **A delegate's trajectory id is its own `agentId`**, and the map answers the
        // different question of what *contains* it. Those must not be conflated: using the
        // containing call as the id makes `stream_id == span`, which is a node that is its
        // own ancestor — unreachable, so no attachment pass can root it and the whole
        // subtree vanishes with nothing reporting it missing (`Event::span`). It also
        // collapses every delegate spawned by one call onto a single node, which is the
        // multiplexing defect `translate::TrajectoryId` exists to prevent.
        //
        // `None` container when the store records no link — see [`delegate_spans`] for the
        // measured population. Its work still reaches a reader, at the root: an unknown
        // `span` renders at the root, because dropping loses work exactly where structure
        // is least reliable.
        let container = spans.get(&agent).cloned();
        let parsed = parse_transcript(delegate, Some(agent.clone()))?;

        // The delegate's own declaration, so a reader has a node to attach its work to even
        // when the parent's transcript never named it. Emitted before its contents, because
        // `started` is where a trajectory's identity lives.
        let mut opened = Event::new(
            EventKind::Subagent,
            Some(Phase::Started),
            parsed.started_at_ms,
        );
        opened.stream_id = Some(agent);
        opened.span = container;
        opened.name = delegate_name(delegate);
        messages.push(SessionMessage::Event(opened));
        if let Some(sidecar) = sidecar_record(delegate) {
            messages.push(SessionMessage::NativeRecord(sidecar));
        }

        messages.extend(parsed.messages);
    }

    for journal in &session.journals {
        messages.extend(parse_journal(journal)?);
    }

    Ok(messages)
}

/// One file's records, projected.
#[derive(Debug)]
#[cfg(test)]
struct ParsedFile {
    messages: Vec<SessionMessage>,
    /// The first timestamp seen, for the synthesized trajectory declaration above.
    started_at_ms: i64,
    /// Workflow run ids and their host `tool_use` ids found in the root transcript.
    /// `None` marks a conflicting duplicate, which is ambiguous and must not choose a host.
    workflow_hosts: HashMap<String, Option<String>>,
}

/// Project one JSONL file through the live translator.
///
/// `span` is the trajectory every event in this file belongs to — set for a delegate,
/// `None` for the session's own transcript.
#[cfg(test)]
fn parse_transcript(path: &Path, span: Option<String>) -> Result<ParsedFile> {
    let mut correlation = Correlation::new();
    let mut messages = Vec::new();
    let mut started_at_ms = 0;
    let mut workflow_hosts: HashMap<String, Option<String>> = HashMap::new();
    let mut version = 0u64;

    let stats = for_each_json_line(path, |record| {
        version += 1;
        messages.push(SessionMessage::NativeRecord(native_record(
            path, version, &record,
        )));
        let at_ms = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_millis_from_iso)
            .unwrap_or(started_at_ms);
        if started_at_ms == 0 {
            started_at_ms = at_ms;
        }

        // A root `toolUseResult` records the workflow run id while the nested
        // `tool_result` block records the host `tool_use_id`. Keep this join as metadata on
        // the same parse pass so `parse_session` does not scan the root transcript twice.
        if span.is_none() {
            if let Some((run_id, host)) = workflow_host(&record) {
                match workflow_hosts.get_mut(&run_id) {
                    // A conflicting duplicate is not a choice to make: keeping either host
                    // would turn a malformed native record into a plausible wrong tree. Keep
                    // the ambiguity latched so a later duplicate cannot silently revive it.
                    Some(existing) if existing.as_ref().is_some_and(|value| value != &host) => {
                        *existing = None;
                    }
                    Some(_) => {}
                    None => {
                        workflow_hosts.insert(run_id, Some(host));
                    }
                }
            }
        }

        for message in project_record(&mut correlation, &record, at_ms) {
            messages.push(message);
        }
        Ok(())
    })
    .with_context(|| format!("parsing {}", path.display()))?;

    // A file with records that all failed to parse is a different fact from an empty one,
    // and they look identical downstream (`crate::native::JsonLines`). This is the loud
    // failure `AGENTS.md` § Compatibility asks for in place of tolerating a shape no known
    // era emits.
    if stats.parsed == 0 && stats.skipped > 0 {
        return Err(anyhow!(
            "no record in {} parsed as JSON ({} lines skipped); this is not a shape any \
             known `claude` version writes",
            path.display(),
            stats.skipped
        ));
    }

    // Every event in a delegate's file belongs to that delegate's trajectory. Applied
    // after projection rather than inside it so the translator stays the live one, and
    // applied only where the event does not already name a *deeper* span — a delegate that
    // itself delegates has interior structure of its own, and overwriting it would flatten
    // the very tree this is reconstructing.
    if let Some(trajectory) = &span {
        for message in &mut messages {
            if let SessionMessage::Event(event) = message {
                if event.span.is_none() {
                    event.span = Some(trajectory.clone());
                }
            }
        }
    }

    Ok(ParsedFile {
        messages,
        started_at_ms,
        workflow_hosts,
    })
}

/// The root transcript's join key for a workflow manifest.
fn workflow_host(record: &Value) -> Option<(String, String)> {
    let run_id = record
        .get("toolUseResult")
        .and_then(|result| string_at(result, "runId"))?;
    let host = record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks.iter().find_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .then(|| string_at(block, "tool_use_id"))
                    .flatten()
            })
        })?;
    Some((run_id, host))
}

/// One native record, projected onto events.
///
/// # This is the corrective half of the port, and it works by *not* re-implementing
///
/// A native record and a live stdout payload are the same shape for the two record types
/// that carry content, so each is reshaped into the payload the live stream would have
/// carried and handed to [`Correlation`]. What that buys is both measured defects at once:
///
/// - **`defect_tool_use_id`** — a `user` record's tool result is read by
///   `Correlation::tool_results`, which takes the id off the `tool_result` **content
///   block**. Measured 2026-08-28, that is where it is on *every* such block, while the
///   plausible alternatives (`toolUseResult.tool_use_id`, `toolUseResult.toolUseId`, and
///   both top-level forms) occur **zero** times in the whole store. Read one of those and
///   every tool result carries a null call id.
/// - **`defect_is_error`** — read off that same block, which is the only place it appears:
///   on `user`-role blocks, and **never** on an assistant message. Read it on the assistant
///   branch and every real tool failure parses as a success — and `false` is
///   indistinguishable from absent-and-defaulted downstream, which is why the test asserts
///   `true` rather than merely asserting the field parses.
///
/// An `assistant` record is replayed as the `content_block_start` / `content_block_stop`
/// pair the live stream would have sent, which is what makes tool calls, thinking, and
/// prose all arrive with the right kind and a `Task` delegation declare its trajectory —
/// all of it through the live projection rather than a second copy of it.
fn project_record(
    correlation: &mut Correlation,
    record: &Value,
    at_ms: i64,
) -> Vec<SessionMessage> {
    project_record_inner(correlation, record, at_ms, true)
}

fn project_record_without_native_facts(
    correlation: &mut Correlation,
    record: &Value,
    at_ms: i64,
) -> Vec<SessionMessage> {
    project_record_inner(correlation, record, at_ms, false)
}

fn project_record_inner(
    correlation: &mut Correlation,
    record: &Value,
    at_ms: i64,
    include_native_facts: bool,
) -> Vec<SessionMessage> {
    let kind = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The delegating call whose output this is, when the record is a delegate's. Live, this
    // rides on the envelope as `parent_tool_use_id`; natively the store spells the same
    // fact `agentId`, and it is resolved to a trajectory by the caller rather than here.
    let mut messages = match kind {
        // `user` carries tool results, an interrupt, or something the person typed.
        "user" => {
            if let Some(interrupt) = user_interrupt(record, at_ms) {
                vec![SessionMessage::Event(interrupt)]
            } else if has_tool_result(record) {
                // Straight through: the live `user` payload and this record are the same
                // shape, which is the fact the whole reuse rests on.
                correlation.translate(record, at_ms).messages
            } else {
                // Something the person typed. Not projected as an event: the vocabulary has no
                // `user` kind, and the server owns a turn's `request_text`
                // (`docs/database-schema.md`). Inventing a kind is what
                // `crates/adapters/src/lib.rs` forbids an adapter from doing.
                vec![]
            }
        }
        "assistant" => assistant_blocks(correlation, record, at_ms),
        "system" => system_record(correlation, record, at_ms),
        // Everything else the store holds is state the vocabulary has no kind for:
        // `mode`, `permission-mode`, `file-history-snapshot`, `queue-operation`,
        // `attachment`, `ai-title`, `last-prompt`, `atis-latch`, `worktree-state`,
        // `relocated`, `agent-setting`, `cost-state`, `fork-context-ref`. Measured, they
        // are 30% of all records and none of them is a transcript event — several are read
        // by [`summarize`] for a listing row instead, which is where they belong.
        _ => vec![],
    };
    // Native-only completion facts and embedded task XML may appear in more than one
    // vendor record kind across Claude releases. The correlation's closed-trajectory set
    // makes this scan idempotent, while scanning every record avoids coupling correctness
    // to whether Claude chose `user`, `assistant`, or a bookkeeping envelope for the fact.
    if include_native_facts {
        messages.extend(project_native_facts(correlation, record, at_ms));
    }
    messages
}

/// Project only facts that are present in the native transcript and absent from the live
/// root stream. The root bundle reader uses this in `RootSource::Stdout`; the regular file
/// reader calls it after the ordinary record projection. Both paths share the same
/// `Correlation`, so a fact observed on stdout and then on disk closes exactly once.
fn project_native_facts(
    correlation: &mut Correlation,
    record: &Value,
    at_ms: i64,
) -> Vec<SessionMessage> {
    let mut messages = Vec::new();
    if let Some((agent, host, summary)) = synchronous_agent_result(record) {
        let mut emission = super::translate::Emission::default();
        correlation.native_agent_completed(&agent, host.as_deref(), summary, at_ms, &mut emission);
        messages.extend(emission.messages);
    }
    // Claude serializes the close for an Agent/Workflow inside the tool result's text as XML.
    // Feed the parsed payload back through the live task-notification handler so native and
    // stdout paths share one close projection.
    for text in string_values(record) {
        for notification in task_notifications(&text) {
            let emission = correlation.translate(&notification, at_ms);
            messages.extend(emission.messages);
        }
    }
    messages
}

/// Extract the native-only completion shape for a synchronous Agent invocation.
///
/// Claude writes asynchronous launches with `isAsync: true` and
/// `status: async_launched`; those are intentionally not closes.  The content block's
/// `tool_use_id` is the only stable route back to the host trajectory when the native
/// result does not repeat it in `toolUseResult`.
fn synchronous_agent_result(record: &Value) -> Option<(String, Option<String>, String)> {
    let result = record.get("toolUseResult")?.as_object()?;
    if result.get("isAsync")?.as_bool()? || result.get("status")?.as_str()? != "completed" {
        return None;
    }
    let result_value = Value::Object(result.clone());
    let agent = string_at(&result_value, "agentId")?;
    let content = record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array);
    let host = content.and_then(|blocks| {
        blocks.iter().find_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("tool_result"))
                .then(|| string_at(block, "tool_use_id"))
                .flatten()
        })
    });
    let summary = string_at(&result_value, "stdout")
        .or_else(|| string_at(&result_value, "summary"))
        .or_else(|| {
            content.and_then(|blocks| {
                blocks.iter().find_map(|block| {
                    (block.get("type").and_then(Value::as_str) == Some("tool_result"))
                        .then(|| block.get("content").map(Value::to_string))
                        .flatten()
                })
            })
        })
        .unwrap_or_default();
    Some((agent, host, summary))
}

/// Preserve the exact native JSON object and any large sidecar it references.
#[cfg(test)]
fn native_record(path: &Path, version: u64, record: &Value) -> NativeRecord {
    native_record_with_raw(path, version, record, None)
}

fn native_record_with_raw(
    path: &Path,
    version: u64,
    record: &Value,
    raw_json: Option<Vec<u8>>,
) -> NativeRecord {
    let mut blobs = Vec::new();
    if let Some(sidecar) = record
        .get("toolUseResult")
        .and_then(|result| string_at(result, "persistedOutputPath"))
    {
        blobs.push(read_native_blob(&sidecar));
    }
    NativeRecord {
        source: native_source(path),
        version,
        raw_json,
        payload: record.clone(),
        blobs,
    }
}

fn read_native_blob(name: &str) -> NativeBlob {
    let path = Path::new(name);
    let Some(root) = store_root().and_then(|root| std::fs::canonicalize(root).ok()) else {
        return NativeBlob {
            name: name.to_string(),
            size: 0,
            bytes: None,
            locator: None,
            error: Some("native store root is unavailable".to_string()),
        };
    };
    let canonical = match std::fs::canonicalize(path) {
        Ok(path) if path.starts_with(&root) => path,
        Ok(path) => {
            return NativeBlob {
                name: name.to_string(),
                size: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                bytes: None,
                locator: None,
                error: Some("persisted output path is outside the native store".to_string()),
            }
        }
        Err(error) => {
            return NativeBlob {
                name: name.to_string(),
                size: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                bytes: None,
                locator: None,
                error: Some(error.to_string()),
            }
        }
    };
    let size = std::fs::metadata(&canonical)
        .map(|meta| meta.len())
        .unwrap_or(0);
    NativeBlob {
        name: name.to_string(),
        size,
        bytes: None,
        locator: Some(canonical.to_string_lossy().to_string()),
        error: None,
    }
}

#[cfg(test)]
fn sidecar_record(path: &Path) -> Option<NativeRecord> {
    let (payload, raw) = read_sidecar_with_raw(path)?;
    Some(NativeRecord {
        source: native_source(&path.with_extension("meta.json")),
        version: revision_bytes(&raw),
        raw_json: Some(raw),
        payload,
        blobs: Vec::new(),
    })
}

fn revision_bytes(bytes: &[u8]) -> u64 {
    // Stable FNV-1a keeps snapshot identity independent of process lifetime and platform.
    // The database still checks payload equality on collision rather than silently replacing
    // a different revision.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn manifest_revision(path: &Path) -> Option<u64> {
    Some(revision_bytes(&std::fs::read(path).ok()?))
}

/// Stable logical source name, relative to the session bundle rather than the host's home.
fn native_source(path: &Path) -> String {
    let components: Vec<_> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    for marker in ["subagents", "workflows"] {
        if let Some(index) = components.iter().position(|component| *component == marker) {
            return components[index..].join("/");
        }
    }
    "root.jsonl".to_string()
}

/// Every string value nested in a native record.
///
/// Task notifications have appeared in tool-result content, assistant text, and a few
/// bookkeeping fields across Claude versions. Walking values rather than naming one path
/// keeps the reader tolerant of those envelope moves while the XML root remains the one
/// structural discriminator.
fn string_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(values) => values.iter().flat_map(string_values).collect(),
        Value::Object(values) => values.values().flat_map(string_values).collect(),
        _ => Vec::new(),
    }
}

/// Parse embedded `<task-notification>` XML into the payload shape used by the live path.
///
/// The XML is deliberately parsed as a small generic tree instead of deserializing a fixed
/// struct: Claude has added fields to this element over time, and preserving them in the
/// intermediate value lets the one normalizer decide which ones are meaningful. Scalar
/// numbers and booleans are recovered as JSON scalars so `usage` remains queryable.
fn task_notifications(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("<task-notification") {
        let start = search_from + start;
        let Some(end_rel) = text[start..].find("</task-notification>") else {
            break;
        };
        let end = start + end_rel + "</task-notification>".len();
        if let Some(value) = parse_task_notification_xml(&text[start..end]) {
            let mut payload = serde_json::Map::new();
            payload.insert("type".to_string(), Value::String("system".to_string()));
            payload.insert(
                "subtype".to_string(),
                Value::String("task_notification".to_string()),
            );
            if let Value::Object(fields) = value {
                payload.extend(fields);
            }
            out.push(Value::Object(payload));
        }
        search_from = end;
    }
    out
}

fn parse_task_notification_xml(xml: &str) -> Option<Value> {
    let mut reader = XmlReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<(String, serde_json::Map<String, Value>, String)> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Start(tag)) => {
                let name = xml_field_name(tag.local_name().as_ref());
                stack.push((name, serde_json::Map::new(), String::new()));
            }
            Ok(XmlEvent::Text(text)) => {
                let current = stack.last_mut()?;
                current.2.push_str(&text.decode().ok()?);
            }
            Ok(XmlEvent::CData(text)) => {
                let current = stack.last_mut()?;
                current.2.push_str(&String::from_utf8_lossy(&text));
            }
            Ok(XmlEvent::End(_)) => {
                let (name, fields, text) = stack.pop()?;
                let value = if fields.is_empty() {
                    xml_scalar(text.trim())
                } else {
                    let mut object = fields;
                    if !text.trim().is_empty() {
                        object.insert("text".to_string(), xml_scalar(text.trim()));
                    }
                    Value::Object(object)
                };
                if let Some(parent) = stack.last_mut() {
                    parent.1.insert(name, value);
                } else {
                    return Some(value);
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => return None,
            Ok(XmlEvent::Empty(tag)) => {
                let name = xml_field_name(tag.local_name().as_ref());
                if let Some(parent) = stack.last_mut() {
                    parent.1.insert(name, Value::String(String::new()));
                } else {
                    return None;
                }
            }
            Ok(XmlEvent::Comment(_))
            | Ok(XmlEvent::Decl(_))
            | Ok(XmlEvent::PI(_))
            | Ok(XmlEvent::DocType(_))
            | Ok(XmlEvent::GeneralRef(_)) => {}
        }
    }
}

fn xml_field_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).replace('-', "_")
}

fn xml_scalar(text: &str) -> Value {
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => text
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| text.parse::<f64>().map(Value::from))
            .unwrap_or_else(|_| Value::String(text.to_string())),
    }
}

/// Whether this `user` record carries a tool result.
fn has_tool_result(record: &Value) -> bool {
    record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

/// A user interrupt, as a status the timeline can show.
///
/// The vendor records this as a `user` record whose text is `[Request interrupted by user]`
/// plus a top-level `interruptedMessageId`. Measured at 133 records across the store.
/// Discriminated on the **id**, not on the sentence: the text is English prose the vendor
/// may reword, while the field's presence is structural.
///
/// A `status` rather than a failure: an interrupted turn is a completed exchange the user
/// chose to end, and `is_error` on it would report a deliberate stop as a broken turn
/// (`docs/session-protocol.md` § Outcomes — keep `abandoned` distinct from `failed`).
fn user_interrupt(record: &Value, at_ms: i64) -> Option<Event> {
    string_at(record, "interruptedMessageId")?;
    let mut event = Event::new(EventKind::Status, None, at_ms);
    event.text = "The user interrupted this turn".to_string();
    Some(event)
}

/// An `assistant` record's content blocks, replayed through the live block projection.
///
/// The store writes a *closed* block — the record only exists once the block finished — so
/// each one is replayed as the `content_block_start` / `content_block_stop` pair the live
/// stream would have sent, with the deltas that would have sat between them collapsed into
/// the start. That is what makes a `Task` declare its trajectory, a tool call carry its
/// parsed input, and thinking arrive as `thinking` rather than as prose.
///
/// **The block index is the vendor's own, per record.** `claude` numbers content blocks per
/// message, so two delegates both open index 0 — the key that keeps those apart is
/// `(host, index)` inside `translate::BlockKey`, and the host here is the delegate's
/// `agentId`, which is the store's spelling of the live `parent_tool_use_id`.
fn assistant_blocks(
    correlation: &mut Correlation,
    record: &Value,
    at_ms: i64,
) -> Vec<SessionMessage> {
    let mut messages = Vec::new();

    if let Some(failure) = api_error(record, at_ms) {
        // An API-level failure carries a synthetic assistant message whose text is the
        // error. Reported as a failed status rather than as the agent's own prose, which is
        // read none of the three fields that say so, and an API failure is
        // indistinguishable from ordinary assistant text.
        return vec![SessionMessage::Event(failure)];
    }

    let host = string_at(record, "agentId");
    let content = record
        .get("message")
        .and_then(|message| message.get("content"));
    let Some(blocks) = content.and_then(Value::as_array) else {
        return messages;
    };

    for (index, block) in blocks.iter().enumerate() {
        let index = index as u64;
        // `tool_use` input is a parsed object here, while live it arrives as streamed
        // partial JSON that `content_block_stop` parses. Re-serializing it is what lets one
        // projection read both: the translator parses the string back, so the input reaches
        // `data.input` as the object it is.
        let opening = match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let mut opening = block.clone();
                // The start carries no text live; the input arrives as deltas.
                if let Some(object) = opening.as_object_mut() {
                    object.remove("input");
                }
                opening
            }
            _ => block.clone(),
        };

        let mut start = json!({
            "type": "content_block_start",
            "index": index,
            "content_block": opening,
        });
        let mut stop = json!({
            "type": "content_block_stop",
            "index": index,
        });
        if let Some(agent) = &host {
            // The live envelope's `parent_tool_use_id`. Set on the *envelope* rather than
            // on the nested event, which is where it lives live and where
            // `Correlation::translate` reads it.
            for payload in [&mut start, &mut stop] {
                payload
                    .as_object_mut()
                    .expect("a JSON object was just constructed")
                    .insert("parent_tool_use_id".to_string(), json!(agent));
            }
        }

        // The deltas the live stream would have carried, collapsed into one. A `tool_use`'s
        // input and a text block's prose both arrive this way, so this is what puts the
        // text on the block before its stop reads it.
        let body = match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => block
                .get("input")
                .map(|input| json!({ "partial_json": input.to_string() })),
            Some("thinking") => block
                .get("thinking")
                .and_then(Value::as_str)
                .map(|text| json!({ "thinking": text })),
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .map(|text| json!({ "text": text })),
            _ => None,
        };

        messages.extend(envelope(correlation, start, at_ms));
        if let Some(delta) = body {
            let mut payload = json!({
                "type": "content_block_delta",
                "index": index,
                "delta": delta,
            });
            if let Some(agent) = &host {
                payload
                    .as_object_mut()
                    .expect("a JSON object was just constructed")
                    .insert("parent_tool_use_id".to_string(), json!(agent));
            }
            // **Dropped rather than emitted.** A delta is ephemeral by
            // `Event::derive_stability`, so it would never reach the journal anyway — and
            // emitting one here would claim an arrival time the store never recorded. It is
            // fed to the translator purely so the block's text is on it when its stop
            // reads it.
            let _ = envelope(correlation, payload, at_ms);
        }
        messages.extend(envelope(correlation, stop, at_ms));
    }

    messages
}

/// Feed one nested stream event to the translator, wrapped as the live envelope.
fn envelope(correlation: &mut Correlation, nested: Value, at_ms: i64) -> Vec<SessionMessage> {
    let host = nested
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut payload = json!({ "type": "stream_event", "event": nested });
    if let Some(agent) = host {
        payload
            .as_object_mut()
            .expect("a JSON object was just constructed")
            .insert("parent_tool_use_id".to_string(), json!(agent));
    }
    correlation.translate(&payload, at_ms).messages
}

/// An API-level failure, which the store records as a synthetic assistant message.
///
/// Three fields say so together and **all three are checked**: `message.model` is
/// `<synthetic>`, a top-level `error` names the class, and `isApiErrorMessage` is true.
/// Measured: 258 synthetic-model records, 196 with `isApiErrorMessage: true`, 341 with a
/// top-level `error` — so the counts differ and no single field is the discriminant. The
/// flag is required because a synthetic model also carries a *user interrupt*'s
/// acknowledgement (`user-interrupt.jsonl` holds exactly that, with
/// `isApiErrorMessage: false`), so keying on the model alone would report an interrupt as
/// an API failure.
fn api_error(record: &Value, at_ms: i64) -> Option<Event> {
    if record.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let mut event = Event::new(EventKind::Status, None, at_ms);
    event.is_error = true;
    // The vendor's own sentence, which is what a reader needs; the `error` class
    // (`server_error`) travels in `data` rather than replacing it.
    event.text = record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "The API call failed".to_string());
    if let Some(class) = string_at(record, "error") {
        event.data = Some(json!({ "error": class }));
    }
    Some(event)
}

/// A `system` record, mapped onto the kinds the vocabulary has.
///
/// `compact_boundary` is the one that becomes a first-class event; the rest are narration
/// or are dropped. **`compactMetadata` is camelCase natively** while the live payload spells
/// it `compact_metadata`, so the record is rewritten to the live spelling rather than
/// handled twice — a dual-spelling reader is what `AGENTS.md` § Compatibility forbids, and
/// the whole point of one projection is that the *reader* stays single.
fn system_record(correlation: &mut Correlation, record: &Value, at_ms: i64) -> Vec<SessionMessage> {
    match record.get("subtype").and_then(Value::as_str) {
        Some("compact_boundary") => {
            let mut payload = record.clone();
            let object = payload
                .as_object_mut()
                .expect("a `system` record is a JSON object");
            if let Some(metadata) = object.remove("compactMetadata") {
                object.insert("compact_metadata".to_string(), metadata);
            }
            // `logicalParentUuid` names the record this boundary logically follows, and
            // Carried in `data` beside the vendor's own metadata rather than promoted to a
            // field: nothing in the vocabulary means "the record before compaction", and
            // inventing a field for one vendor's bookkeeping is worse than keeping it
            // queryable.
            if let Some(logical) = string_at(record, "logicalParentUuid") {
                if let Some(metadata) = object
                    .get_mut("compact_metadata")
                    .and_then(Value::as_object_mut)
                {
                    metadata.insert("logicalParentUuid".to_string(), json!(logical));
                }
            }
            correlation.translate(&payload, at_ms).messages
        }
        // `away_summary` (952 records) is the agent's own account of what it did while the
        // user was away, and `scheduled_task_fire` (56) is a wakeup. Both are narration the
        // agent authored and a reader wants — `notice` is exactly that kind.
        Some("away_summary") | Some("scheduled_task_fire") | Some("informational") => {
            let Some(text) = string_at(record, "content") else {
                return vec![];
            };
            let mut event = Event::new(EventKind::Notice, None, at_ms);
            event.text = text;
            vec![SessionMessage::Event(event)]
        }
        Some("api_error") => {
            let mut event = Event::new(EventKind::Status, None, at_ms);
            event.is_error = true;
            event.text =
                string_at(record, "content").unwrap_or_else(|| "The API call failed".to_string());
            vec![SessionMessage::Event(event)]
        }
        // `turn_duration` (2665) is a measurement of a turn the server already owns,
        // `stop_hook_summary` (626) and `local_command` (113) are harness bookkeeping, and
        // `agents_killed` (28) reports on processes rather than on the conversation.
        // Dropped deliberately: none is a transcript event, and a `status` per hook run
        // would bury the transcript in narration nobody asked for.
        _ => vec![],
    }
}

/// A workflow's phase outcomes, from `subagents/workflows/wf_<id>/journal.jsonl`.
///
/// # This file is read rather than skipped, which is a decision
///
/// Skipping this file loses outcomes. Measured, its `started`/`result` records are the
/// *only* record of a workflow delegate's outcome: `--forward-subagent-text` forwards nothing
/// for `Workflow` (`docs/agents.md`), and a workflow delegate's own `.meta.json` carries
/// neither `toolUseId` nor `parentAgentId` on **any** file. So without this file a workflow's
/// delegates have no outcome at
/// all, and every one of them renders as permanently running.
///
/// The 26 `started` with no `result` are real: a workflow interrupted mid-phase. They stay
/// open rather than being closed with a guess, because the server's turn-settle sweep is
/// what closes an unreported trajectory, and it records that as `abandoned` — distinct from
/// `failed`, which is the distinction that keeps an interrupted turn from reading as a
/// broken one.
#[cfg(test)]
fn parse_journal(path: &Path) -> Result<Vec<SessionMessage>> {
    let mut messages = Vec::new();
    // `journal.jsonl` records carry **no timestamp** — measured, 1379 `started` and 1353
    // `result` records and not one `timestamp` field among them. So an outcome is ordered by
    // its position in the file and carries no time of its own. `0` rather than a synthesized
    // stamp: a fabricated timestamp is a lie a journal makes permanent, and the server
    // stamps arrival anyway.
    const NO_RECORDED_TIME: i64 = 0;

    for_each_json_line(path, |record| {
        let Some(agent) = string_at(&record, "agentId") else {
            return Ok(());
        };
        // The delegate this outcome belongs to. **Its own `agentId` is the trajectory**, which
        // is what its `agent-<hash>.jsonl` is named after and what `parse_session` declared —
        // so the outcome closes the same node the delegate's work is inside.
        //
        // Deliberately *not* the containing trajectory: closing the container would report one
        // delegate's outcome as the whole host's, and with a workflow fanning out to N
        // delegates under one host the last outcome to arrive would win while the other N-1
        // stayed open forever. That is the multiplexing collapse `translate::TrajectoryId`
        // exists to prevent, and a workflow is exactly the N>1 case.
        //
        // Only `result`. `started` is not emitted here: the delegate's own file already
        // declares the trajectory in `parse_session`, and a second `started` would be the
        // duplicate node that rule exists to prevent.
        if record.get("type").and_then(Value::as_str) == Some("result") {
            let mut event = Event::new(
                EventKind::Subagent,
                Some(Phase::Completed),
                NO_RECORDED_TIME,
            );
            event.stream_id = Some(agent);
            // The phase's own answer. A string for most, an object for a structured result —
            // serialized rather than dropped, because it is the delegate's whole output.
            event.text = match record.get("result") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            messages.push(SessionMessage::Event(event));
        }
        Ok(())
    })
    .with_context(|| format!("parsing workflow journal {}", path.display()))?;

    Ok(messages)
}

/// The `agentId` a delegate's filename encodes.
///
/// `agent-<hash>.jsonl` → `<hash>`. The filename is authoritative rather than the records
/// inside: measured, every record in a delegate's file carries its *parent's* `sessionId`,
/// so the file name is the only place the delegate's own identity appears unambiguously.
fn agent_id_of(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())?
        .strip_prefix("agent-")?
        .strip_suffix(".jsonl")
        .map(str::to_string)
}

/// What a delegate is called, from its sidecar.
///
/// **`None` when the sidecar does not name it**, which is most of them.
///
/// `agentType` is deliberately **not** a fallback even though every sidecar has one, because
/// it is the *kind* of agent (`general-purpose`, `Explore`, `workflow-subagent`) rather than
/// what this delegation is — and a timeline of nine rows reading "general-purpose" is the
/// unreadable card `translate::task_label` already refuses to produce. A guess written here
/// becomes permanent and is then indistinguishable from a name an agent chose
/// (`docs/session-protocol.md` § `name` is absent when unknown).
///
/// # `description` before `name`, so the two sources agree
///
/// Both are present or only `description` is — measured, no sidecar carries a `name` without
/// one — so this order never loses a label, and it is chosen to **match what stdout calls the
/// same delegation.** A live `task_started` reads `description` first, and this file's
/// declaration lands on the trajectory stdout already opened; because a later name overwrites
/// an earlier one, reading `name` here relabelled the card a few hundred milliseconds after it
/// appeared and left the slug as the durable label. Measured, that diverged for every one of
/// the 91 sidecars carrying a `name` and none of the others, and the slug is the less
/// informative half of the pair (`demo-b` against `后台演示任务B`). One fact with two spellings
/// is a card that changes under the reader; the brief is also what a person would recognise.
fn delegate_name(path: &Path) -> Option<String> {
    let sidecar = read_sidecar(path)?;
    string_at(&sidecar, "description").or_else(|| string_at(&sidecar, "name"))
}

/// A delegate's `agent-<hash>.meta.json`, when it has one.
fn read_sidecar(path: &Path) -> Option<Value> {
    read_sidecar_with_raw(path).map(|(payload, _)| payload)
}

fn read_sidecar_with_raw(path: &Path) -> Option<(Value, Vec<u8>)> {
    let sidecar = path.with_extension("meta.json");
    let raw = std::fs::read(sidecar).ok()?;
    let payload = serde_json::from_slice(&raw).ok()?;
    Some((payload, raw))
}

/// Which trajectory each delegate's work belongs inside, keyed by `agentId`.
///
/// # A recorded join, not directory nesting
///
/// The files sit **flat** in one `subagents/` directory however deep the delegation went, so
/// reading one path component up to find a parent attributes every delegate to the root
/// session and a depth-2 tree comes out one level deep. Depth is expressed by
/// `parentAgentId` inside the child's own sidecar — a minority of sidecars carry one, and
/// `fixtures/native/claude/delegation-task-fanout-nested.*` is a 3-way fan-out where each
/// depth-1 delegate spawns one depth-2 child.
///
/// A delegate's trajectory id **is its `agentId`**, so this map answers the other question:
/// which trajectory *contains* it. Four routes, in order of how directly the store states
/// it:
///
/// 1. `toolUseId` → the delegating `tool_use` in the parent's transcript, which promotes
///    that call in place (`docs/session-protocol.md` § Promotion).
/// 2. A workflow manifest's `runId` → the root `toolUseResult.runId` → its nested
///    `tool_result.tool_use_id`, the vendor's explicit join for `Workflow` delegates.
/// 3. `parentAgentId` → the parent delegate's trajectory. This is the fallback for a
///    depth-2 delegate whose sidecar names its parent but not the call inside it.
/// 4. `toolUseResult.agentId` in the parent's transcript is deliberately not consulted:
///    it overlaps the sidecar `toolUseId` route rather than extending it.
///
/// Delegates with no route are emitted at the **root**, because an unknown span renders at the
/// root and dropping them would lose work exactly where structure is least reliable. Before the
/// workflow join, this included every workflow delegate whose sidecar carried neither field.
///
/// The `wf_<id>` directory name is not the join key: the root transcript carries the same id
/// in `toolUseResult.runId`, and the manifest repeats it as `runId`. Pairing by directory
/// position would be a guess that is wrong whenever a session ran more than one workflow.
fn delegate_spans(
    session: &SessionFiles,
    root_workflow_hosts: &HashMap<String, Option<String>>,
) -> HashMap<String, String> {
    // `agentId` → the `tool_use` id that spawned it, when a sidecar names one.
    let mut by_tool_use: HashMap<String, String> = HashMap::new();
    // `agentId` → its parent delegate's `agentId`, when a sidecar names one.
    let mut by_parent: HashMap<String, String> = HashMap::new();
    // `agentId` → the root `Workflow` tool_use id joined through a manifest's run id.
    let mut by_workflow: HashMap<String, Option<String>> = HashMap::new();

    for manifest in &session.manifests {
        let Some(payload) = read_workflow_manifest(manifest) else {
            continue;
        };
        let Some(run_id) = string_at(&payload, "runId") else {
            continue;
        };
        let Some(host) = root_workflow_hosts.get(&run_id).and_then(Option::as_ref) else {
            continue;
        };
        let Some(progress) = payload.get("workflowProgress").and_then(Value::as_array) else {
            continue;
        };
        for entry in progress {
            if let Some(agent) = string_at(entry, "agentId") {
                match by_workflow.get_mut(&agent) {
                    Some(existing) if existing.as_ref().is_some_and(|value| value != host) => {
                        // A delegate appearing in two manifests with different hosts is
                        // ambiguous; leave it unresolved instead of attaching it wrongly, and
                        // keep the ambiguity latched across later snapshots.
                        *existing = None;
                    }
                    Some(_) => {}
                    None => {
                        by_workflow.insert(agent, Some(host.clone()));
                    }
                }
            }
        }
    }

    for delegate in &session.delegates {
        let Some(agent) = agent_id_of(delegate) else {
            continue;
        };
        let Some(sidecar) = read_sidecar(delegate) else {
            continue;
        };
        if let Some(parent) = string_at(&sidecar, "parentAgentId") {
            by_parent.insert(agent.clone(), parent);
        }
        if let Some(call) = string_at(&sidecar, "toolUseId") {
            by_tool_use.insert(agent, call);
        }
    }

    // What **contains** each delegate, which is what its events' `span` names. A delegate's
    // own trajectory id is its `agentId` and is not in this map.
    //
    // **Its own `toolUseId` is the route, and `parentAgentId` is only the fallback.** That
    // order is the fix rather than a preference: a depth-2 delegate's `toolUseId` names the
    // call *inside its parent's transcript*, so it lands inside the parent's node with no
    // reasoning about depth at all — while resolving through `parentAgentId` to the
    // *parent's* call puts the child beside its parent instead of inside it, collapsing the
    // two depths onto one node. Reversed, this fixture's three depth-2 children each
    // acquired their parent's container and the tree came out one level deep, which is the
    // same flattening the port set out to correct, arriving by a different route.
    //
    // Depth is therefore expressed by the chain rather than computed: a child names a call,
    // that call's own event carries its file's trajectory, and so on to the root. Nothing
    // here counts levels, which is what makes unbounded depth work.
    let mut spans = HashMap::new();
    for delegate in &session.delegates {
        let Some(agent) = agent_id_of(delegate) else {
            continue;
        };
        let container = by_tool_use
            .get(&agent)
            .cloned()
            .or_else(|| by_workflow.get(&agent).and_then(Clone::clone))
            // 10 of 2209 sidecars name a parent and no call. The parent's trajectory is its
            // `agentId`, so the child nests one level too shallow — inside its parent rather
            // than inside the call within it — which is the honest answer when the store
            // records no call, and strictly better than the root.
            .or_else(|| by_parent.get(&agent).cloned());
        if let Some(container) = container {
            spans.insert(agent, container);
        }
    }
    spans
}

/// The delegating tools whose interior Claude also forwards on stdout.
///
/// **A list, and deliberately a short one that fails safe.** `--forward-subagent-text`
/// forwards for the built-in `Task` alone (`docs/agents.md`), so anything absent here is read
/// from its file — which is the right default for a tool nobody has written yet, since an
/// unlisted tool loses nothing and a wrongly listed one loses its whole interior. This is the
/// inverse of `translate`'s promotion rule, where a *name* test could never be completed;
/// here the name test is answering a different question — not "did this delegate?" but "is
/// stdout already saying it?" — and that one the vendor's flag does settle.
const STDOUT_FORWARDED_TOOLS: &[&str] = &["Task"];

/// Record every `tool_use` id in this record against its tool name.
fn collect_host_tools(record: &Value, into: &mut HashMap<String, String>) {
    let Some(blocks) = record
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        if let (Some(id), Some(name)) = (string_at(block, "id"), string_at(block, "name")) {
            into.insert(id, name);
        }
    }
}

/// Read one workflow manifest snapshot.
fn read_workflow_manifest(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Parse, then follow the transcript as it grows.
///
/// # Why a poll and not a filesystem watch
///
/// A watch would need a per-platform notification API and would still have to re-read from
/// an offset on every event, because Claude appends JSONL files and creates delegate files
/// after a turn starts. The poll is the same read with a timer in front of it, and it also
/// handles files that do not exist yet. The root transcript is optional on the live path:
/// stdout already carries it at stream fidelity and reading it again would double every
/// root block.
///
/// The reader owns one cursor per source and keeps one `Correlation` per JSONL file. A
/// correlation cannot be shared between files because Claude numbers content blocks per
/// message, so two delegates both opening index zero would otherwise share block state.
/// Manifest files are different: they are snapshots rewritten in place, so their cursor is
/// a `(size, mtime)` fingerprint and a monotonically increasing version rather than a byte
/// offset. Re-scanning the directory on every pass is what discovers delegates and journals
/// created after the first pass.
fn follow(session: SessionFiles, root: RootSource) -> EventStream {
    follow_from(session, root, FollowBaseline::Empty)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowBaseline {
    /// Read every complete record currently in the bundle.
    Empty,
    /// Attach to the current end of every source and only publish later appends.
    Current,
}

fn follow_from(session: SessionFiles, root: RootSource, baseline: FollowBaseline) -> EventStream {
    follow_from_with_correlation(session, root, baseline, None)
}

fn follow_from_with_correlation(
    session: SessionFiles,
    root: RootSource,
    baseline: FollowBaseline,
    root_correlation: Option<std::sync::Arc<std::sync::Mutex<Correlation>>>,
) -> EventStream {
    /// Fast enough to feel live against a store that flushes per content block, slow enough
    /// that an idle session costs one `stat` per interval.
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);

    Box::pin(futures::stream::unfold(
        BundleFollowState::new_with_correlation(session, root, baseline, root_correlation),
        |mut state| async move {
            loop {
                if let Some(message) = state.pending.pop_front() {
                    return Some((Ok(message), state));
                }

                match state.poll() {
                    Ok(messages) if !messages.is_empty() => {
                        state.pending.extend(messages);
                        continue;
                    }
                    Ok(_) => tokio::time::sleep(POLL).await,
                    Err(error) => return Some((Err(error), state)),
                }
            }
        },
    ))
}

/// Where a follower's root-transcript records come from.
///
/// On [`RootSource::Stdout`] the process stream already carries the root at stream fidelity,
/// so projecting the file again would double every root block. The file is still *read* —
/// its records are retained, and the workflow join and host tool names are taken from it —
/// but nothing it holds is projected as an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    /// No process stream: this reader projects the root itself.
    File,
    /// A live session's stdout is projecting the root.
    Stdout,
}

/// Cursor for one append-only JSONL source.
struct JsonlCursor {
    offset: u64,
    version: u64,
    correlation: Correlation,
    opened: bool,
}

/// Cursor for a manifest snapshot rewritten in place.
struct ManifestCursor {
    fingerprint: Option<(u64, u128)>,
    version: u64,
}

struct JournalCursor {
    offset: u64,
    version: u64,
}

/// Cursor for a delegate's `.meta.json` sidecar.
///
/// Tracked independently from the delegate's JSONL cursor so that a sidecar appearing after
/// the delegate's first open is still discovered and ingested.
struct SidecarCursor {
    fingerprint: Option<(u64, u128)>,
    ingested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegateProjection {
    Project,
    Suppress,
    /// The sidecar is the source of truth for the host tool.  Do not consume the delegate
    /// cursor until it exists: otherwise a late Task sidecar would arrive after a duplicate
    /// prefix had already been published, and the protocol has no retract operation.
    Pending,
}

/// State for the dynamic multi-file bundle reader.
struct BundleFollowState {
    session: SessionFiles,
    root: JsonlCursor,
    delegates: HashMap<PathBuf, JsonlCursor>,
    journals: HashMap<PathBuf, JournalCursor>,
    manifests: HashMap<PathBuf, ManifestCursor>,
    /// Tracks `.meta.json` sidecars independently from their delegate JSONL files.
    sidecars: HashMap<PathBuf, SidecarCursor>,
    workflow_hosts: HashMap<String, Option<String>>,
    /// `tool_use` id → the tool's name, from the root transcript.
    ///
    /// This is what decides whether a delegate's interior is also on stdout, so it is read on
    /// **both** root sources — the live path does not project root records but still needs
    /// this fact from them.
    host_tools: HashMap<String, String>,
    pending: VecDeque<SessionMessage>,
    root_source: RootSource,
    root_correlation: Option<std::sync::Arc<std::sync::Mutex<Correlation>>>,
}

impl BundleFollowState {
    fn new(session: SessionFiles, root_source: RootSource) -> Self {
        Self::new_with_baseline(session, root_source, FollowBaseline::Empty)
    }

    fn new_with_baseline(
        session: SessionFiles,
        root_source: RootSource,
        baseline: FollowBaseline,
    ) -> Self {
        Self::new_with_correlation(session, root_source, baseline, None)
    }

    fn new_with_correlation(
        session: SessionFiles,
        root_source: RootSource,
        baseline: FollowBaseline,
        root_correlation: Option<std::sync::Arc<std::sync::Mutex<Correlation>>>,
    ) -> Self {
        let mut state = Self {
            session,
            // **One cursor over the root, whichever source projects it.** Two offsets over
            // one file is a state where advancing the wrong one silently skips records, and
            // the difference between the modes is only whether a record is *projected*.
            root: JsonlCursor {
                offset: 0,
                version: 0,
                correlation: Correlation::new(),
                opened: true,
            },
            delegates: HashMap::new(),
            journals: HashMap::new(),
            manifests: HashMap::new(),
            sidecars: HashMap::new(),
            workflow_hosts: HashMap::new(),
            host_tools: HashMap::new(),
            pending: VecDeque::new(),
            root_source,
            root_correlation,
        };
        if baseline == FollowBaseline::Current {
            state.seed_current_baseline();
        }
        state
    }

    /// Establish the live boundary for a resumed native session.
    ///
    /// A resumed process already has a complete native bundle. Starting every JSONL cursor
    /// at zero would project old delegate files again, while starting only the root cursor at
    /// the end would lose the host-tool metadata needed to suppress Task's duplicate file.
    /// The baseline therefore advances every existing source and mines its metadata without
    /// emitting records or events. Files created after this pass remain ordinary live sources.
    fn seed_current_baseline(&mut self) {
        let transcript = self.session.transcript.clone();
        if let Some((bytes, offset)) = read_appended(&transcript, 0) {
            let records: Vec<Value> = json_lines(&bytes, Edge::Head).collect();
            self.root.offset = offset;
            self.root.version = records.len() as u64;
            for record in records {
                self.remember_root_metadata(&record);
                self.hydrate_root(&record);
            }
        }

        for delegate in self.session.delegates.clone() {
            let Some((bytes, offset)) = read_appended(&delegate, 0) else {
                continue;
            };
            let records: Vec<Value> = json_lines(&bytes, Edge::Head).collect();
            let mut cursor = JsonlCursor {
                offset,
                version: records.len() as u64,
                correlation: Correlation::new(),
                opened: true,
            };
            // Keep host-tool discovery complete for nested delegates that are created after
            // the resume boundary.
            for record in &records {
                collect_host_tools(record, &mut self.host_tools);
                let _ = project_record_without_native_facts(&mut cursor.correlation, record, 0);
            }
            cursor.opened = true;
            self.delegates.insert(delegate, cursor);
        }

        for journal in self.session.journals.clone() {
            if let Some((bytes, offset)) = read_appended(&journal, 0) {
                let version = json_lines(&bytes, Edge::Head).count() as u64;
                self.journals
                    .insert(journal, JournalCursor { offset, version });
            }
        }

        for manifest in self.session.manifests.clone() {
            let Some(meta) = std::fs::metadata(&manifest).ok() else {
                continue;
            };
            self.manifests.insert(
                manifest.clone(),
                ManifestCursor {
                    fingerprint: Some((meta.len(), file_modified_nanos(&meta))),
                    version: manifest_revision(&manifest).unwrap_or(1),
                },
            );
            // `delegate_spans` reads the current snapshot on every pass, so no projection is
            // needed here; this cursor only prevents the baseline snapshot from being emitted.
        }

        for delegate in self.session.delegates.clone() {
            let sidecar = delegate.with_extension("meta.json");
            let Some(meta) = std::fs::metadata(&sidecar).ok() else {
                continue;
            };
            self.sidecars.insert(
                sidecar,
                SidecarCursor {
                    fingerprint: Some((meta.len(), file_modified_nanos(&meta))),
                    ingested: true,
                },
            );
        }
    }

    fn remember_root_metadata(&mut self, record: &Value) {
        if let Some((run_id, host)) = workflow_host(record) {
            self.workflow_hosts
                .entry(run_id)
                .and_modify(|existing| {
                    if existing.as_ref().is_some_and(|old| old != &host) {
                        *existing = None;
                    }
                })
                .or_insert(Some(host));
        }
        collect_host_tools(record, &mut self.host_tools);
    }

    fn hydrate_root(&mut self, record: &Value) {
        if let Some(correlation) = &self.root_correlation {
            if let Ok(mut correlation) = correlation.lock() {
                let _ = project_record(&mut correlation, record, 0);
            }
        } else {
            let _ = project_record(&mut self.root.correlation, record, 0);
        }
    }

    fn poll(&mut self) -> Result<Vec<SessionMessage>> {
        self.session = collect(self.session.transcript.clone());
        let mut messages = Vec::new();

        // Manifests are snapshots, not append-only logs. Re-read a changed file in full and
        // retain its version in the cursor so a later projection can distinguish revisions.
        let manifests = self.session.manifests.clone();
        for manifest in manifests {
            let Some(meta) = std::fs::metadata(&manifest).ok() else {
                continue;
            };
            let fingerprint = (meta.len(), file_modified_nanos(&meta));
            let changed = self
                .manifests
                .get(&manifest)
                .is_none_or(|cursor| cursor.fingerprint != Some(fingerprint));
            if !changed {
                continue;
            }
            let version = manifest_revision(&manifest).unwrap_or_else(|| {
                self.manifests
                    .get(&manifest)
                    .map_or(1, |cursor| cursor.version.saturating_add(1))
            });
            self.manifests.insert(
                manifest.clone(),
                ManifestCursor {
                    fingerprint: Some(fingerprint),
                    version,
                },
            );
            if let Some(payload) = read_workflow_manifest(&manifest) {
                messages.push(SessionMessage::NativeRecord(NativeRecord {
                    source: native_source(&manifest),
                    version,
                    raw_json: std::fs::read(&manifest).ok(),
                    payload,
                    blobs: Vec::new(),
                }));
            }
        }

        // **Two-phase poll to eliminate file-order dependency in suppression decisions.**
        //
        // The problem: a delegate's suppression depends on whether its host tool is in
        // `STDOUT_FORWARDED_TOOLS`. That name comes from the root transcript or from a parent
        // delegate. If delegates are processed in path-sorted order, a child may be reached
        // before its parent's tool calls are collected, causing the child to be wrongly
        // projected when it should have been suppressed.
        //
        // The fix: Phase 1 scans all new bytes in root and delegates to collect host tools,
        // without advancing cursors or projecting. Phase 2 then reads and projects with the
        // complete host_tools map available for suppression decisions.

        // Scan delegate sidecars independently. A `.meta.json` may appear after its delegate
        // JSONL was first opened, so this is polled every pass with its own fingerprint cursor.
        messages.extend(self.scan_sidecars());

        // Phase 1: Collect host tools from all new appended bytes
        self.collect_all_host_tools();

        // Resolve spans from the current manifest/sidecar view every pass. This is what
        // makes a manifest rewritten after watch() started visible without a restart.
        let spans = delegate_spans(&self.session, &self.workflow_hosts);
        let delegates = self.session.delegates.clone();
        // On the live path, delegate declarations are the only source of a trajectory for
        // Agent/Workflow calls. Process them before native-only root completion facts so a
        // synchronous close cannot land in the journal before its trajectory is opened.
        if self.root_source == RootSource::Stdout {
            for delegate in &delegates {
                let Some(agent) = agent_id_of(delegate) else {
                    continue;
                };
                messages.extend(self.read_delegate(delegate, spans.get(&agent).cloned())?);
            }
        }

        // Phase 2: Read and project with suppression decisions now stable
        messages.extend(self.read_root());

        if self.root_source == RootSource::File {
            for delegate in &delegates {
                let Some(agent) = agent_id_of(delegate) else {
                    continue;
                };
                messages.extend(self.read_delegate(delegate, spans.get(&agent).cloned())?);
            }
        }
        let journals = self.session.journals.clone();
        for journal in journals {
            messages.extend(self.read_journal(&journal)?);
        }
        Ok(messages)
    }

    /// Scan delegate sidecars independently from their JSONL files.
    ///
    /// A `.meta.json` may appear after its delegate JSONL was first opened, so we poll every
    /// delegate path for a sidecar every pass. This ensures late-arriving metadata is discovered.
    fn scan_sidecars(&mut self) -> Vec<SessionMessage> {
        let mut messages = Vec::new();
        let delegates = self.session.delegates.clone();

        for delegate in delegates {
            let sidecar_path = delegate.with_extension("meta.json");

            let Some(meta) = std::fs::metadata(&sidecar_path).ok() else {
                continue;
            };

            let fingerprint = (meta.len(), file_modified_nanos(&meta));
            let cursor = self
                .sidecars
                .entry(sidecar_path.clone())
                .or_insert(SidecarCursor {
                    fingerprint: None,
                    ingested: false,
                });

            // Check if this sidecar is new or changed
            let changed = cursor
                .fingerprint
                .is_none_or(|old_fp| old_fp != fingerprint);
            if !changed {
                continue;
            }

            // Ingest the sidecar
            if let Some((payload, raw)) = read_sidecar_with_raw(&delegate) {
                messages.push(SessionMessage::NativeRecord(NativeRecord {
                    source: native_source(&sidecar_path),
                    version: revision_bytes(&raw),
                    raw_json: Some(raw),
                    payload,
                    blobs: Vec::new(),
                }));
                cursor.fingerprint = Some(fingerprint);
                cursor.ingested = true;
            }
        }

        messages
    }

    /// Phase 1: Scan all new bytes in root and delegates for host tools without advancing cursors.
    ///
    /// This ensures that when Phase 2 makes suppression decisions, it has the complete picture
    /// of all host tools in this poll cycle, regardless of file-sort order.
    fn collect_all_host_tools(&mut self) {
        // Scan root for new host tools
        if let Some((bytes, _)) = read_appended(&self.session.transcript, self.root.offset) {
            for record in json_lines(&bytes, Edge::Head) {
                if let Some((run_id, host)) = workflow_host(&record) {
                    self.workflow_hosts
                        .entry(run_id)
                        .and_modify(|existing| {
                            if existing.as_ref().is_some_and(|old| old != &host) {
                                *existing = None;
                            }
                        })
                        .or_insert(Some(host));
                }
                collect_host_tools(&record, &mut self.host_tools);
            }
        }

        // Scan all delegates for nested host tools
        let delegates = self.session.delegates.clone();
        for delegate in delegates {
            let offset = self
                .delegates
                .get(&delegate)
                .map(|cursor| cursor.offset)
                .unwrap_or(0);

            if let Some((bytes, _)) = read_appended(&delegate, offset) {
                for record in json_lines(&bytes, Edge::Head) {
                    collect_host_tools(&record, &mut self.host_tools);
                }
            }
        }
    }

    /// Read the root transcript's appended records.
    ///
    /// Every record is **retained and mined** for the two facts the rest of the pass needs —
    /// the workflow run-id join and the host tool names — on both root sources. Only
    /// *projection* is conditional: on [`RootSource::Stdout`] the process stream is already
    /// emitting these blocks, and emitting them again here would double every one.
    ///
    /// Host tools are collected in Phase 1 (`collect_all_host_tools`), so this phase focuses
    /// on advancing the cursor, emitting native records, and conditionally projecting events.
    fn read_root(&mut self) -> Vec<SessionMessage> {
        let Some((bytes, next_offset)) = read_appended(&self.session.transcript, self.root.offset)
        else {
            return Vec::new();
        };
        if bytes.is_empty() {
            return Vec::new();
        }
        self.root.offset = next_offset;
        let mut messages = Vec::new();
        for (record, raw) in json_lines_with_raw(&bytes, Edge::Head) {
            self.root.version += 1;
            messages.push(SessionMessage::NativeRecord(native_record_with_raw(
                &self.session.transcript,
                self.root.version,
                &record,
                Some(raw),
            )));
            let at_ms = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso)
                .unwrap_or(0);
            // workflow_host and collect_host_tools were already done in Phase 1, but we still
            // need to update workflow_hosts here for consistency (it may have changed between
            // Phase 1 scan and Phase 2 projection if new data arrived). Actually no - Phase 1
            // scanned the same bytes we're now reading, so the data is identical. We can skip
            // the duplicate collection.
            //
            // Actually, let's keep it for safety: if collect_all_host_tools fails partway, at
            // least read_root still collects from what it processes. The cost is negligible.
            if let Some((run_id, host)) = workflow_host(&record) {
                self.workflow_hosts
                    .entry(run_id)
                    .and_modify(|existing| {
                        if existing.as_ref().is_some_and(|old| old != &host) {
                            *existing = None;
                        }
                    })
                    .or_insert(Some(host));
            }
            collect_host_tools(&record, &mut self.host_tools);
            if self.root_source == RootSource::File {
                messages.extend(project_record(&mut self.root.correlation, &record, at_ms));
            } else if let Some(correlation) = &self.root_correlation {
                if let Ok(mut correlation) = correlation.lock() {
                    messages.extend(project_native_facts(&mut correlation, &record, at_ms));
                }
            } else {
                messages.extend(project_native_facts(
                    &mut self.root.correlation,
                    &record,
                    at_ms,
                ));
            }
        }
        messages
    }

    fn read_delegate(&mut self, path: &Path, span: Option<String>) -> Result<Vec<SessionMessage>> {
        let Some(agent) = agent_id_of(path) else {
            return Ok(Vec::new());
        };
        // **Suppressed only when stdout is carrying this delegate's interior too**, which is
        // true for the built-in `Task` and false for every other delegating tool
        // (`docs/agents.md`). The host's tool name comes from the root transcript, so the
        // answer does not depend on which file this reader happened to poll first.
        //
        // A host classification is held until its sidecar exists. **The two errors are not
        // symmetric:** publishing before classification can create an unretractable duplicate,
        // while waiting one poll costs latency but preserves a deterministic tree. Once the
        // sidecar is present, an unknown or non-forwarded host is projected; only a known
        // stdout-forwarded tool is suppressed.
        //
        // **Re-asked every pass**, because a tool name can arrive after the file it describes.
        // The window is narrow — a delegate file is never written before its host call
        // (measured over 454 pairs: the delegate's first record follows its host's by a median
        // of 5 ms, and never precedes it), and `read_root` runs before this in the same pass —
        // but it is not empty, since a host record still half-written when the root was read is
        // correctly held back by `read_appended` until the next pass. The delegate cursor is
        // not advanced while classification is pending, so a late sidecar cannot leave a
        // projected prefix that the protocol has no retract operation for.
        //
        // What must *not* happen is deciding once and latching: suppression describes a file,
        // and a delegate grows for as long as its subagent runs.
        //
        // Decided before the cursor is borrowed, because the answer is read from `self`.
        let projection = self.delegate_projection(path);
        if projection == DelegateProjection::Pending {
            return Ok(Vec::new());
        }
        let suppress = projection == DelegateProjection::Suppress;

        let entry = self
            .delegates
            .entry(path.to_path_buf())
            .or_insert_with(|| JsonlCursor {
                offset: 0,
                version: 0,
                correlation: Correlation::new(),
                opened: false,
            });

        let mut messages = Vec::new();
        // **Suppression withholds events, and never records.** A delegate is written while its
        // subagent runs, so this has to hold for the whole file rather than for the bytes that
        // existed when it was first seen — a rule that fired once at first sight would suppress
        // one poll interval's worth and project the entire rest, leaving the prefix and the
        // suffix disagreeing about what the rule was. And the records are kept because they are
        // the only copy: projection is the half stdout duplicates (`AGENTS.md`
        // § Compatibility). This is the same split [`BundleFollowState::read_root`] makes.
        //
        // # Declaring is not forwarding, and the declaration is emitted either way
        //
        // Two independent facts, and the three delegating tools disagree about both. Measured
        // over the dev store, by whether a delegate's `agentId` is ever announced as a
        // `<task-id>`:
        //
        // | host | stdout declares it | stdout forwards its interior |
        // | --- | --- | --- |
        // | `Task` | yes | yes |
        // | `Agent` | yes — 369 of 857 flat delegates | no |
        // | `Workflow` | **no — 0 of 1379** | no |
        //
        // So for an `Agent` the declaration below duplicates one stdout already sent, and it is
        // emitted anyway. Withholding it would save a row that every consumer merges on
        // `stream_id`, and would risk the node itself: stdout announces a delegation once, at
        // its `task_started`, so a session Sylph attaches to mid-flight has no such frame to
        // merge with and the interior would arrive under a trajectory nothing ever opened.
        // Same asymmetry as above — a duplicate is merged, an absence is lost — and for every
        // workflow delegate this declaration is the *only* one there will ever be.
        if !entry.opened && !suppress {
            let Some(first) = read_first_json(path) else {
                return Ok(Vec::new());
            };
            let at_ms = first
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso)
                .unwrap_or(0);
            let mut opened = Event::new(EventKind::Subagent, Some(Phase::Started), at_ms);
            opened.stream_id = Some(agent.clone());
            opened.span = span.clone();
            opened.name = delegate_name(path);
            messages.push(SessionMessage::Event(opened));
            // Sidecar is now handled independently in `scan_sidecars()`, not here. This
            // ensures a `.meta.json` that arrives after the delegate JSONL is still discovered.
            entry.opened = true;
        }

        let Some((bytes, next_offset)) = read_appended(path, entry.offset) else {
            return Ok(messages);
        };
        if bytes.is_empty() {
            return Ok(messages);
        }
        entry.offset = next_offset;
        // A delegate's own calls are host calls too: measured, 139 of 612 sidecars naming a
        // `toolUseId` name a call that lives in *another delegate's* file rather than the
        // root, so a root-only view leaves every nested delegation's host unnamed.
        // Accumulated locally because `entry` is borrowed for the whole loop.
        let mut nested_hosts = HashMap::new();
        for (record, raw) in json_lines_with_raw(&bytes, Edge::Head) {
            entry.version += 1;
            messages.push(SessionMessage::NativeRecord(native_record_with_raw(
                path,
                entry.version,
                &record,
                Some(raw),
            )));
            let at_ms = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso)
                .unwrap_or(0);
            collect_host_tools(&record, &mut nested_hosts);
            // Still correlated when suppressed, so the cursor's state stays consistent with
            // the bytes it has consumed; only the resulting events are dropped. A correlation
            // fed a gap would mis-key whatever followed it if suppression ever lifted.
            for message in
                project_record_without_native_facts(&mut entry.correlation, &record, at_ms)
            {
                if suppress {
                    continue;
                }
                let message = match message {
                    SessionMessage::Event(mut event) => {
                        if event.span.is_none() {
                            event.span = Some(span.clone().unwrap_or_else(|| agent.clone()));
                        }
                        SessionMessage::Event(event)
                    }
                    other => other,
                };
                messages.push(message);
            }
        }
        self.host_tools.extend(nested_hosts);
        Ok(messages)
    }

    fn delegate_projection(&self, path: &Path) -> DelegateProjection {
        if self.root_source != RootSource::Stdout {
            return DelegateProjection::Project;
        }
        let sidecar = path.with_extension("meta.json");
        // Do not guess while the metadata file is still arriving.  The vendor writes a
        // delegate sidecar for every delegate shape we support; holding the cursor is the
        // only way to make late metadata deterministic without retracting published events.
        if !sidecar.is_file() {
            return DelegateProjection::Pending;
        }
        let suppress = delegate_tool_use_id(path)
            .and_then(|host| self.host_tools.get(&host))
            .is_some_and(|tool| STDOUT_FORWARDED_TOOLS.contains(&tool.as_str()));
        if suppress {
            DelegateProjection::Suppress
        } else {
            DelegateProjection::Project
        }
    }

    fn read_journal(&mut self, path: &Path) -> Result<Vec<SessionMessage>> {
        let cursor = self
            .journals
            .entry(path.to_path_buf())
            .or_insert(JournalCursor {
                offset: 0,
                version: 0,
            });
        let Some((bytes, next_offset)) = read_appended(path, cursor.offset) else {
            return Ok(Vec::new());
        };
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        cursor.offset = next_offset;
        let mut messages = Vec::new();
        for (record, raw) in json_lines_with_raw(&bytes, Edge::Head) {
            cursor.version += 1;
            messages.push(SessionMessage::NativeRecord(native_record_with_raw(
                path,
                cursor.version,
                &record,
                Some(raw),
            )));
            if record.get("type").and_then(Value::as_str) != Some("result") {
                continue;
            }
            let Some(agent) = string_at(&record, "agentId") else {
                continue;
            };
            let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), 0);
            event.stream_id = Some(agent);
            event.text = match record.get("result") {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            messages.push(SessionMessage::Event(event));
        }
        Ok(messages)
    }
}

fn file_modified_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

fn read_first_json(path: &Path) -> Option<Value> {
    let bytes = read_span(path, 0, 64 * 1024)?;
    let first = json_lines(&bytes, Edge::Head).next();
    first
}

fn delegate_tool_use_id(path: &Path) -> Option<String> {
    string_at(&read_sidecar(path)?, "toolUseId")
}

/// Whole lines appended since `offset`, and the offset past the last complete one.
///
/// `None` when the file does not exist or shrank — a shrink means the vendor rewrote it,
/// which `claude` does not do but which costs one comparison to not corrupt if it starts.
fn read_appended(path: &Path, offset: u64) -> Option<(Vec<u8>, u64)> {
    let size = std::fs::metadata(path).ok()?.len();
    if size < offset {
        return None;
    }
    if size == offset {
        return Some((Vec::new(), offset));
    }
    let bytes = read_span(path, offset, size - offset)?;
    // Only up to the last newline: anything after it is a record still being written, and
    // re-reading it next poll is correct where parsing half of it is not.
    let complete = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(at) => at + 1,
        None => return Some((Vec::new(), offset)),
    };
    Some((bytes[..complete].to_vec(), offset + complete as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture directory, from this file's location rather than the process cwd.
    ///
    /// `cargo test` runs with the *crate* as cwd while `cargo test --workspace` may not, so
    /// a relative fixture path is a test that passes in one invocation and not the other.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/native/claude")
            .join(name)
    }

    /// Project one fixture the way [`parse_session`] projects a transcript.
    fn project(name: &str) -> Vec<Event> {
        let parsed = parse_transcript(&fixture(name), None).expect("the fixture must parse");
        parsed
            .messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event.clone()),
                _ => None,
            })
            .collect()
    }

    fn of_kind(events: &[Event], kind: EventKind) -> Vec<&Event> {
        events.iter().filter(|event| event.kind == kind).collect()
    }

    /// **Defect 1: a tool result's call id is on the content block, nowhere else.**
    ///
    /// *Measured 2026-08-28 over `~/.claude/projects/`:* `tool_use_id` is on **every**
    /// `tool_result` block, and the plausible alternatives — `toolUseResult.tool_use_id`,
    /// `toolUseResult.toolUseId`, and both top-level spellings — occur **zero** times in the
    /// whole store. A parser reading those pairs nothing: every tool result gets a null call
    /// id, and the pairing `docs/session-protocol.md` § Keying rules depends on silently
    /// does not exist.
    ///
    /// Asserted on the **value that should have been found**, not on presence: the exact id
    /// from the payload, matched to the call that produced it. A presence check would pass
    /// against a synthesized id, and `AGENTS.md` is explicit that a clean parse is not
    /// evidence.
    #[test]
    fn defect_tool_use_id_is_read_from_the_content_block() {
        let events = project("tool-call-and-result.jsonl");
        let results = of_kind(&events, EventKind::ToolResult);
        assert_eq!(results.len(), 2, "the fixture holds two tool results");

        // The two ids the fixture's own blocks carry, verbatim.
        let found: Vec<&str> = results
            .iter()
            .map(|event| event.stream_id.as_deref().expect("a result must be keyed"))
            .collect();
        assert!(
            found.contains(&"tooluse_cggcT0HQ4Y0RBoWZUigwKS"),
            "the Read result's id, from its content block; found {found:?}"
        );
        assert!(
            found.contains(&"tooluse_HY1CcE9ChkjuLWWSqPsLz9"),
            "the Bash result's id, from its content block; found {found:?}"
        );

        // And the pairing works, which is the point of the id: each result's `stream_id` is
        // its call's. A null id — what the ported parser produced — pairs nothing, and the
        // failure is invisible because a missing key and an omitted field look identical.
        let calls: Vec<&str> = of_kind(&events, EventKind::ToolCall)
            .iter()
            .map(|event| event.stream_id.as_deref().expect("a call must be keyed"))
            .collect();
        for result in &found {
            assert!(
                calls.contains(result),
                "{result} must pair with a tool_call; calls were {calls:?}"
            );
        }

        // The plausible alternative keys, asserted absent from the real payload. This is
        // what makes the test evidence about the *store* rather than about this parser: if a
        // future vendor version starts writing one, this fails and the doc above is stale.
        let record: Value = serde_json::from_str(
            std::fs::read_to_string(fixture("tool-call-and-result.jsonl"))
                .unwrap()
                .lines()
                .find(|line| line.contains("toolUseResult"))
                .unwrap(),
        )
        .unwrap();
        for absent in ["tool_use_id", "toolUseId", "parent_tool_use_id"] {
            assert!(
                record.get(absent).is_none(),
                "{absent} must not be at the top level of a real record"
            );
        }
        let tur = record.get("toolUseResult").expect("the fixture has one");
        for absent in ["tool_use_id", "toolUseId"] {
            assert!(
                tur.get(absent).is_none(),
                "{absent} must not be on toolUseResult in a real record"
            );
        }
    }

    /// **Defect 2: `is_error` is only ever on a `user`-role block.**
    ///
    /// *Measured 2026-08-28: 46957 occurrences, **2131 of them true**, and never on an
    /// assistant message.* Read it on the assistant branch and every real tool failure parses
    /// as a **success** — and `false` is indistinguishable from absent-and-defaulted
    /// downstream, which is exactly why this asserts `true` on a payload that genuinely failed
    /// rather than asserting the field was seen.
    #[test]
    fn defect_is_error_is_read_from_the_user_role_block() {
        let events = project("tool-error-edit.jsonl");
        let results = of_kind(&events, EventKind::ToolResult);
        assert_eq!(results.len(), 1);

        let failure = results[0];
        assert!(
            failure.is_error,
            "a real Edit failure must report as one; the ported parser read this off the \
             assistant branch, where it never appears, so every failure parsed as a success"
        );
        // Paired to its call, so the failure escalates onto the right node.
        assert_eq!(
            failure.stream_id.as_deref(),
            Some("tooluse_3h534n2kUuKLBaI0fSqo2H")
        );
        // And the vendor's own message survives, which is what a reader needs to know *what*
        // failed.
        assert!(
            failure.text.contains("String to replace not found"),
            "the tool's own error text must survive; got {:?}",
            failure.text
        );

        // The other half of the measurement: the flag is on the `user` block and the
        // assistant record carries none. Asserted against the real payload so this test is
        // evidence about the store.
        let lines: Vec<Value> = std::fs::read_to_string(fixture("tool-error-edit.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let assistant = lines
            .iter()
            .find(|record| record["type"] == "assistant")
            .expect("the fixture has one");
        let assistant_blocks = assistant["message"]["content"].as_array().unwrap();
        for block in assistant_blocks {
            assert!(
                block.get("is_error").is_none(),
                "is_error never appears on an assistant block, which is why reading it \
                 there yields a silent false"
            );
        }
        let user = lines
            .iter()
            .find(|record| record["type"] == "user")
            .expect("the fixture has one");
        assert_eq!(
            user["message"]["content"][0]["is_error"],
            json!(true),
            "the flag lives here, on the user-role tool_result block"
        );
    }

    /// The dual-shape `toolUseResult`, which cannot be held by a fixed struct.
    ///
    /// *Measured: 69001 records carry it as an **object** (a success: `stdout`, `stderr`,
    /// `interrupted`, `isImage`, …) and 2174 as a bare **string** (a failure).* The pairing is
    /// near-total — every one of the 2134 string-valued records whose block carries the flag
    /// has `is_error: true` — so the shape is *correlated* with the outcome and is still not
    /// the discriminant, because 43 string-valued records carry no flag at all.
    ///
    /// So this parser reads neither shape for the outcome: the flag on the block decides, and
    /// `toolUseResult` is never deserialized into a typed struct. That is why the dual shape
    /// costs nothing here — it is not on the path that decides anything.
    #[test]
    fn the_two_shapes_of_tool_use_result_both_project() {
        // Object-shaped, a success.
        let success = project("tool-call-and-result.jsonl");
        let results = of_kind(&success, EventKind::ToolResult);
        assert!(!results.is_empty());
        assert!(results.iter().all(|event| !event.is_error));

        // String-shaped, a failure. Same code path, no shape test anywhere in it.
        let failure = project("tool-error-edit.jsonl");
        let failed = of_kind(&failure, EventKind::ToolResult);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].is_error);
    }

    /// Large tool output has **three** renderings of one value, and the block's is the one
    /// that travels.
    ///
    /// Measured on `tool-result-persisted-output-sidecar.jsonl`, a real 53.6 KB Bash output:
    ///
    /// | Where | What it holds |
    /// | --- | --- |
    /// | `message.content[].content` | a 2088-byte `<persisted-output>` wrapper with a 2 KB preview |
    /// | `toolUseResult.stdout` | a fuller copy, capped at 30000 chars |
    /// | `toolUseResult.persistedOutputPath` / `Size` | the untruncated 54836-byte sidecar on disk |
    ///
    /// The **block** is used, and the choice is deliberate. It is what the model itself read,
    /// so it is what the transcript should show — a reader looking at the timeline sees what
    /// the agent saw. The fuller `stdout` is not preferred because "more text" is not the same
    /// as "what happened", and the sidecar is not read at all: it is a separate file that may
    /// have been pruned, and reading megabytes off disk per tool result to fill a timeline row
    /// is the 3.0 s-per-listing mistake in another form.
    ///
    /// The path and size travel in `data` instead, so a client that wants the full output
    /// knows it exists and where — which is strictly better than silently showing a truncated
    /// value that looks complete.
    #[test]
    fn a_persisted_output_keeps_the_block_text_and_names_the_sidecar() {
        let events = project("tool-result-persisted-output-sidecar.jsonl");
        let results = of_kind(&events, EventKind::ToolResult);
        assert_eq!(results.len(), 1);
        let result = results[0];

        // The block's own rendering, wrapper and all: this is what the model read.
        assert!(
            result.text.starts_with("<persisted-output>"),
            "the block's text is what the model saw; got {:?}",
            &result.text.chars().take(40).collect::<String>()
        );
        assert!(
            result.text.contains("Output too large"),
            "the vendor's own truncation notice must survive, or a reader cannot tell that \
             what they are looking at is partial"
        );
        // Bounded by the block rather than by the 30000-char `stdout`.
        assert!(
            result.text.len() < 4096,
            "the block's preview is ~2 KB; got {} bytes, which means a fuller copy leaked in",
            result.text.len()
        );
    }

    /// An API failure is not ordinary assistant prose, and three fields say so together.
    ///
    /// Read none of them and an API failure is indistinguishable from the agent talking.
    /// Measured, more records carry a synthetic `model` than carry `isApiErrorMessage: true`,
    /// so **the flag is the discriminant** — a user interrupt's acknowledgement *also* uses a
    /// synthetic model.
    #[test]
    fn an_api_failure_is_a_failed_status_rather_than_assistant_text() {
        let events = project("api-error-server-error.jsonl");
        let failures: Vec<&Event> = events
            .iter()
            .filter(|event| event.kind == EventKind::Status && event.is_error)
            .collect();
        assert_eq!(failures.len(), 1, "the fixture holds one API failure");
        assert!(
            failures[0].text.contains("API Error"),
            "the vendor's own sentence must reach a reader; got {:?}",
            failures[0].text
        );
        // The error class travels, because "it failed" and "the server failed" are different
        // facts and only the second names a cause.
        assert_eq!(
            failures[0].data.as_ref().and_then(|data| data.get("error")),
            Some(&json!("server_error"))
        );
        // And it is not also published as the agent's own prose, which would show the error
        // twice and once under the wrong voice.
        assert!(
            of_kind(&events, EventKind::Assistant)
                .iter()
                .all(|event| !event.text.contains("API Error")),
            "an API failure must not also arrive as assistant prose"
        );
    }

    /// A synthetic model alone must not be read as an API failure.
    ///
    /// This is the half that makes the discriminant load-bearing: `user-interrupt.jsonl`'s
    /// assistant record carries `model: "<synthetic>"` **and** `isApiErrorMessage: false`, so
    /// keying on the model would report a deliberate stop as a server error.
    #[test]
    fn a_user_interrupt_is_not_reported_as_an_api_failure() {
        let events = project("user-interrupt.jsonl");
        assert!(
            events
                .iter()
                .all(|event| !(event.kind == EventKind::Status && event.is_error)),
            "an interrupt is not a failure: colouring it red reports an interrupted turn as \
             a broken one"
        );
        // It is still recorded, because a turn the user cut short is a fact about the turn.
        let statuses = of_kind(&events, EventKind::Status);
        assert_eq!(statuses.len(), 1);
        assert!(statuses[0].text.contains("interrupted"));
    }

    /// A compaction boundary survives with the vendor's metadata, under the live spelling.
    ///
    /// The native store writes `compactMetadata` while the live payload writes
    /// `compact_metadata`, and the record is rewritten to the live spelling rather than read
    /// twice — a dual-spelling reader is what `AGENTS.md` § Compatibility forbids. The
    /// assertion is on a *value inside* the metadata, because an empty object would satisfy
    /// a presence check while carrying nothing.
    #[test]
    fn a_compaction_boundary_carries_the_vendors_own_metadata() {
        let events = project("compaction-boundary.jsonl");
        let compacts = of_kind(&events, EventKind::Compact);
        assert_eq!(compacts.len(), 1);
        let data = compacts[0].data.as_ref().expect("the metadata must travel");
        assert_eq!(data.get("trigger"), Some(&json!("manual")));
        assert_eq!(data.get("preTokens"), Some(&json!(178564)));
        assert_eq!(data.get("postTokens"), Some(&json!(16758)));
        // `logicalParentUuid` names the record this boundary logically follows, and it is
        // the only thing that says where the pre-compaction conversation ended.
        assert_eq!(
            data.get("logicalParentUuid"),
            Some(&json!("e117dff3-3ce2-490c-b927-f02cd2c7f7d8"))
        );
    }

    /// **`forkedFrom.messageUuid` is not a branch point.**
    ///
    /// *Measured over the whole store: 873 records carry `forkedFrom`, and on **all 873** the
    /// `messageUuid` equals **that record's own `uuid`**.* It is a per-record stamp, not a
    /// pointer to where the fork happened. A hand-written fixture can satisfy the opposite
    /// reading — a distinct earlier message — which real data falsifies, so this is asserted
    /// against a payload from the store.
    ///
    /// `forkedFrom.sessionId` *is* stable, so session-level lineage is recoverable and a
    /// per-record branch point is not. This parser therefore reports the parent and omits the
    /// point, because a value that is always the record itself carries no information and
    /// storing it would assert one.
    #[test]
    fn fork_point_is_not_recoverable_but_the_parent_session_is() {
        let path = fixture("fork-replay-messageuuid-mirrors-own-uuid.jsonl");
        let records: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        let forked: Vec<&Value> = records
            .iter()
            .filter(|record| record.get("forkedFrom").is_some())
            .collect();
        assert!(forked.len() >= 8, "the fixture holds a replayed prefix");
        for record in &forked {
            let fork = &record["forkedFrom"];
            assert_eq!(
                fork["messageUuid"], record["uuid"],
                "measured on all 873 real records: messageUuid mirrors the record's own uuid, \
                 so it cannot be a branch point"
            );
            // The one part that is stable across the whole replayed prefix.
            assert_eq!(
                fork["sessionId"],
                json!("bba7f74d-801e-4a61-b814-3091cafeb904")
            );
        }

        // And the listing row reports the parent, which is the half that is real.
        let candidate = SessionCandidate::of(&path).expect("the fixture is a file");
        let summary = summarize(&candidate).expect("it has a sessionId");
        assert_eq!(
            summary.parent_native_id.as_ref().map(|id| id.as_str()),
            Some("bba7f74d-801e-4a61-b814-3091cafeb904")
        );
    }

    /// Build a session tree on disk in the store's own layout.
    ///
    /// The fixture is a flat file plus six sidecars; the store nests a session's delegates
    /// under `<session>/subagents/`. The layout **is** part of what is being tested, since that
    /// is where a flattening defect lives, so it is reproduced rather than stubbed.
    fn stage_fanout(tag: &str) -> SessionFiles {
        let root =
            std::env::temp_dir().join(format!("sylph-claude-native-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = "9f1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9";
        let subagents = root.join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();

        let transcript = root.join(format!("{session}.jsonl"));
        std::fs::copy(fixture("delegation-task-fanout-nested.jsonl"), &transcript).unwrap();

        // The six sidecars, named as the store names them.
        for entry in std::fs::read_dir(fixture("").parent().unwrap().join("claude")).unwrap() {
            let path = entry.unwrap().path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(rest) = name.strip_prefix("delegation-task-fanout-nested.") else {
                continue;
            };
            if !rest.ends_with(".meta.json") {
                continue;
            }
            std::fs::copy(&path, subagents.join(rest)).unwrap();
            // Every sidecar needs its transcript beside it, or `collect` finds no delegate to
            // attach it to. Minimal content: this test is about attribution, not projection.
            let agent = rest.trim_end_matches(".meta.json");
            std::fs::write(
                subagents.join(format!("{agent}.jsonl")),
                format!(
                    "{}\n",
                    json!({
                        "type": "assistant",
                        "uuid": format!("{agent}-1"),
                        "sessionId": session,
                        "isSidechain": true,
                        "agentId": agent.trim_start_matches("agent-"),
                        "timestamp": "2026-08-27T00:00:01.000Z",
                        "message": {
                            "role": "assistant",
                            "model": "claude-opus-5",
                            "content": [{"type": "text", "text": format!("work by {agent}")}],
                        },
                    })
                ),
            )
            .unwrap();
        }
        collect(transcript)
    }

    /// **Nesting is `parentAgentId`, not directory depth.**
    ///
    /// The files sit flat in one `subagents/` directory however deep the delegation went, so
    /// reading one path component up attributes every delegate to the root session. This
    /// fixture is a real 3-way fan-out where each depth-1 delegate spawns one depth-2 child;
    /// flattened, all six read as siblings of the root.
    ///
    /// Asserted on the **span each depth-2 child resolves to**, which is what a timeline nests
    /// by — not on a depth count, which a flattened tree could still get right by accident.
    #[test]
    fn a_depth_two_delegate_nests_inside_its_parents_call_and_not_at_the_root() {
        let session = stage_fanout("nested");
        assert_eq!(
            session.delegates.len(),
            6,
            "three depth-1 and three depth-2"
        );
        let parsed_root = parse_transcript(&session.transcript, None).unwrap();
        let spans = delegate_spans(&session, &parsed_root.workflow_hosts);

        // Every delegate is contained by the `Agent` call that spawned it — depth-1 by a call
        // in the root transcript, depth-2 by a call inside its parent's. The pairs come from
        // the six real sidecars.
        for (agent, call) in [
            // Depth 1: `spawnDepth: 1`, no `parentAgentId`.
            ("af9bb3b1475445e7d", "toolu_bdrk_01Nx7t66HbEff2gxYm8fkS8c"),
            ("a7ee4760914240f23", "toolu_bdrk_01KEQn5XcyFaadCoYPixk1bs"),
            ("a93b1a01d826a734b", "toolu_bdrk_017eb6uXzXdSSFymrWrJ9KM2"),
            // Depth 2: `spawnDepth: 2`, each naming a `parentAgentId` above.
            ("a9aa098597842ed09", "toolu_bdrk_01XmGjD3gDvCnUzZJGGmQmZY"),
            ("acf7507ea81426dd0", "toolu_bdrk_01UinshAXbFYkth2Dfn4yZtE"),
            ("afcf7613795f5a25b", "toolu_bdrk_01K8XkeA7MbjbnJJ7ivTDS5K"),
        ] {
            assert_eq!(
                spans.get(agent).map(String::as_str),
                Some(call),
                "{agent} must be contained by the call that spawned it"
            );
        }

        // The property a flattening destroys: a depth-2 child's container is a call *inside
        // its parent's transcript*, so it is distinct from its parent's own container. Read one
        // path component up instead and all six collapse onto the root, which is what this
        // inequality detects.
        assert_ne!(
            spans.get("a9aa098597842ed09"),
            spans.get("a7ee4760914240f23"),
            "a depth-2 child must not share its parent's container; that is the tree \
             coming out one level deep"
        );

        // And no delegate is its own container. A node that is its own ancestor is
        // unreachable, so no attachment pass roots it and the whole subtree vanishes with
        // nothing reporting it missing (`Event::span`).
        for (agent, container) in &spans {
            assert_ne!(
                agent, container,
                "{agent} must not contain itself: a cycle silently deletes both nodes"
            );
        }
    }

    /// A delegate's events land inside its trajectory rather than at the root.
    ///
    /// The span resolution above is only half the fix: it has to reach the events. A delegate
    /// whose events carry no `span` renders as the root agent's own work, which is the
    /// pre-`span` bug reappearing one layer down.
    #[test]
    fn a_delegates_own_work_carries_its_trajectory() {
        let session = stage_fanout("reparent");
        let messages = parse_session(&session).expect("the staged session must parse");
        let events: Vec<&Event> = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect();

        // Each delegate declares its own trajectory, keyed by its `agentId`, so a reader has a
        // node to attach work to even where the parent's transcript never named it. Six
        // delegates, six distinct trajectories — not one per spawning call, which is the
        // multiplexing collapse.
        let declared: Vec<&str> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::Subagent && event.phase == Some(Phase::Started)
            })
            .filter_map(|event| event.stream_id.as_deref())
            .collect();
        assert_eq!(
            declared.len(),
            6,
            "one trajectory per delegate; got {declared:?}"
        );
        assert!(declared.contains(&"a7ee4760914240f23"));
        assert!(
            declared.contains(&"a9aa098597842ed09"),
            "including the depth-2 children"
        );

        // Each one nests inside the call that spawned it.
        let opened: Vec<(&str, Option<&str>)> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::Subagent && event.phase == Some(Phase::Started)
            })
            .map(|event| (event.stream_id.as_deref().unwrap(), event.span.as_deref()))
            .collect();
        assert!(
            opened.contains(&(
                "a7ee4760914240f23",
                Some("toolu_bdrk_01KEQn5XcyFaadCoYPixk1bs")
            )),
            "a depth-1 delegate nests inside its root-transcript call; got {opened:?}"
        );
        assert!(
            opened.contains(&(
                "a9aa098597842ed09",
                Some("toolu_bdrk_01XmGjD3gDvCnUzZJGGmQmZY")
            )),
            "a depth-2 delegate nests inside the call within its parent; got {opened:?}"
        );

        // And the delegate's own prose sits inside its own trajectory rather than at the root,
        // which is the half that makes the span resolution reach the events.
        let inside: Vec<&Event> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::Assistant
                    && event.span.as_deref() == Some("a7ee4760914240f23")
            })
            .copied()
            .collect();
        assert!(
            !inside.is_empty(),
            "a delegate's own output must carry its trajectory, or it renders as the root \
             agent's work"
        );
        assert!(inside
            .iter()
            .any(|event| event.text.contains("work by agent-a7ee4760914240f23")));

        // Nothing a delegate produced may be left at the root: that is the flattening.
        assert!(
            !events.iter().any(|event| {
                event.kind == EventKind::Assistant
                    && event.span.is_none()
                    && event.text.contains("work by agent-")
            }),
            "a delegate's work must not appear as the root agent's own"
        );
    }

    /// A delegate with no recorded link is emitted at the **root**, not dropped.
    ///
    /// A sidecar can still have no attribution route when it carries neither `toolUseId` nor
    /// `parentAgentId`, and not every workflow manifest is guaranteed to be present. That is a
    /// real limit of the store. `docs/session-protocol.md`: an unknown span renders at the root,
    /// because dropping loses work exactly where structure is least reliable.
    #[test]
    fn an_unattributable_delegate_is_kept_at_the_root() {
        let root =
            std::env::temp_dir().join(format!("sylph-claude-native-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = "1a2b3c4d-5e6f-7081-92a3-b4c5d6e7f809";
        let subagents = root.join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            root.join(format!("{session}.jsonl")),
            format!(
                "{}\n",
                json!({
                    "type": "user", "uuid": "u1", "sessionId": session,
                    "timestamp": "2026-08-27T00:00:00.000Z", "cwd": "/tmp",
                    "message": {"role": "user", "content": "go"},
                })
            ),
        )
        .unwrap();
        // The measured shape: a sidecar naming only the kind of agent.
        std::fs::write(
            subagents.join("agent-aorphan123.meta.json"),
            json!({"agentType": "workflow-subagent"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-aorphan123.jsonl"),
            format!(
                "{}\n",
                json!({
                    "type": "assistant", "uuid": "a1", "sessionId": session,
                    "isSidechain": true, "agentId": "aorphan123",
                    "timestamp": "2026-08-27T00:00:01.000Z",
                    "message": {
                        "role": "assistant", "model": "claude-opus-5",
                        "content": [{"type": "text", "text": "orphaned work"}],
                    },
                })
            ),
        )
        .unwrap();

        let session_files = collect(root.join(format!("{session}.jsonl")));
        assert_eq!(session_files.delegates.len(), 1);
        assert!(
            delegate_spans(&session_files, &HashMap::new()).is_empty(),
            "this delegate genuinely has no recorded link"
        );

        let messages = parse_session(&session_files).unwrap();
        let kept = messages.iter().any(|message| match message {
            SessionMessage::Event(event) => event.text.contains("orphaned work"),
            _ => false,
        });
        assert!(
            kept,
            "an unattributable delegate's work must reach a reader at the root rather than \
             being dropped"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A workflow's phase outcomes come from `journal.jsonl`, and nothing else carries them.
    ///
    /// *Measured:* a workflow journal's `started`/`result` records are the only record of a
    /// workflow delegate's outcome — `--forward-subagent-text` forwards nothing for
    /// `Workflow`, and **no** workflow sidecar carries `toolUseId` or `parentAgentId`. Skip
    /// this file and every workflow delegate renders as permanently running.
    #[test]
    fn a_workflow_journal_supplies_the_outcomes_nothing_else_records() {
        let root = std::env::temp_dir().join(format!(
            "sylph-claude-native-workflow-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = "2b3c4d5e-6f70-8192-a3b4-c5d6e7f80912";
        let workflow = root
            .join(session)
            .join("subagents")
            .join("workflows")
            .join("wf_abc123def");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            root.join(format!("{session}.jsonl")),
            format!(
                "{}\n",
                json!({
                    "type": "user", "uuid": "u1", "sessionId": session,
                    "timestamp": "2026-08-27T00:00:00.000Z", "cwd": "/tmp",
                    "message": {"role": "user", "content": "run the workflow"},
                })
            ),
        )
        .unwrap();
        // The real journal shape: no timestamp on any record, which is why an outcome carries
        // no time of its own.
        std::fs::write(
            workflow.join("journal.jsonl"),
            [
                json!({"type": "started", "key": "v2:aaa", "agentId": "a21fcc237510c4670"}).to_string(),
                json!({"type": "result", "key": "v2:aaa", "agentId": "a21fcc237510c4670", "result": "scouted"}).to_string(),
                // A phase that started and never reported: a workflow interrupted mid-run.
                json!({"type": "started", "key": "v2:bbb", "agentId": "ab020b248dc2d843d"}).to_string(),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let session_files = collect(root.join(format!("{session}.jsonl")));
        assert_eq!(
            session_files.journals.len(),
            1,
            "the journal must be read, not skipped"
        );

        let outcomes = parse_journal(&session_files.journals[0]).unwrap();
        let closed: Vec<&Event> = outcomes
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(
            closed.len(),
            1,
            "one result, and the unreported phase stays open"
        );
        assert_eq!(closed[0].kind, EventKind::Subagent);
        assert_eq!(closed[0].phase, Some(Phase::Completed));
        assert_eq!(closed[0].stream_id.as_deref(), Some("a21fcc237510c4670"));
        assert_eq!(closed[0].text, "scouted");
        // The phase with no result is **not** closed with a guess: the server's turn-settle
        // sweep records that as `abandoned`, which stays distinguishable from `failed`.
        assert!(
            !closed
                .iter()
                .any(|event| event.stream_id.as_deref() == Some("ab020b248dc2d843d")),
            "an unreported phase must not be closed by this parser"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A workflow delegate is joined to its host by `runId`, not by the order of files.
    ///
    /// The vendor records the workflow run on the root `toolUseResult`, writes the matching
    /// manifest under `workflows/`, and puts each phase agent under the corresponding
    /// `subagents/workflows/` directory. Two workflows are deliberately staged in a different
    /// order from their manifest paths: pairing by position would attach both delegates to the
    /// wrong `Workflow` call while still producing plausible spans.
    #[test]
    fn workflow_delegates_join_to_their_host_by_run_id() {
        let root = std::env::temp_dir().join(format!(
            "sylph-claude-native-workflow-join-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = "3c4d5e6f-7081-92a3-b4c5-d6e7f8091234";
        let session_dir = root.join(session);
        let subagents = session_dir.join("subagents").join("workflows");
        let manifests = session_dir.join("workflows");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::create_dir_all(&manifests).unwrap();

        let host_one = "toolu_workflow_host_one";
        let host_two = "toolu_workflow_host_two";
        let run_one = "wf_run_one";
        let run_two = "wf_run_two";
        let root_records = [
            // The root observes run two first, so a directory-position pairing is wrong.
            json!({
                "type": "assistant", "uuid": "root-1", "sessionId": session,
                "timestamp": "2026-08-27T00:00:01.000Z",
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": host_two, "name": "Workflow", "input": {}}
                ]}
            }),
            json!({
                "type": "user", "uuid": "root-2", "sessionId": session,
                "timestamp": "2026-08-27T00:00:02.000Z",
                "toolUseResult": {"runId": run_two},
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": host_two, "content": "started"}
                ]}
            }),
            json!({
                "type": "assistant", "uuid": "root-3", "sessionId": session,
                "timestamp": "2026-08-27T00:00:03.000Z",
                "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": host_one, "name": "Workflow", "input": {}}
                ]}
            }),
            json!({
                "type": "user", "uuid": "root-4", "sessionId": session,
                "timestamp": "2026-08-27T00:00:04.000Z",
                "toolUseResult": {"runId": run_one},
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": host_one, "content": "started"}
                ]}
            }),
        ];
        let transcript = root.join(format!("{session}.jsonl"));
        std::fs::write(
            &transcript,
            root_records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();

        for (run_id, agent_id) in [
            (run_one, "workflow_agent_one"),
            (run_two, "workflow_agent_two"),
        ] {
            let workflow_dir = subagents.join(run_id);
            std::fs::create_dir_all(&workflow_dir).unwrap();
            std::fs::write(
                workflow_dir.join(format!("agent-{agent_id}.jsonl")),
                format!(
                    "{}\n",
                    json!({
                        "type": "assistant", "uuid": format!("{agent_id}-record"),
                        "sessionId": session, "isSidechain": true, "agentId": agent_id,
                        "timestamp": "2026-08-27T00:00:05.000Z",
                        "message": {"role": "assistant", "content": [
                            {"type": "text", "text": format!("work by {agent_id}")}
                        ]}
                    })
                ),
            )
            .unwrap();
            std::fs::write(
                manifests.join(format!("{run_id}.json")),
                json!({
                    "runId": run_id,
                    "workflowProgress": [
                        {"type": "workflow_phase", "index": 1},
                        {"type": "workflow_agent", "agentId": agent_id, "state": "done"}
                    ]
                })
                .to_string(),
            )
            .unwrap();
        }

        let session = collect(transcript);
        assert_eq!(
            session.manifests.len(),
            2,
            "both workflow manifests must be collected"
        );
        let parsed_root = parse_transcript(&session.transcript, None).unwrap();
        let spans = delegate_spans(&session, &parsed_root.workflow_hosts);
        assert_eq!(
            spans.get("workflow_agent_one").map(String::as_str),
            Some(host_one),
            "run one must resolve to the root tool_use carrying its runId"
        );
        assert_eq!(
            spans.get("workflow_agent_two").map(String::as_str),
            Some(host_two),
            "run two must resolve to the root tool_use carrying its runId"
        );

        let messages = parse_session(&session).expect("the staged workflow must parse");
        for (agent, host) in [
            ("workflow_agent_one", host_one),
            ("workflow_agent_two", host_two),
        ] {
            let opened = messages.iter().any(|message| match message {
                SessionMessage::Event(event) => {
                    event.kind == EventKind::Subagent
                        && event.phase == Some(Phase::Started)
                        && event.stream_id.as_deref() == Some(agent)
                        && event.span.as_deref() == Some(host)
                }
                _ => false,
            });
            assert!(
                opened,
                "{agent} must be nested under its Workflow host {host}"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A delegate is not a session, so it is never enumerated as one.
    ///
    /// Measured: all 400 sampled delegate files carry their *parent's* `sessionId` on every
    /// record, so a delegate has no id of its own and `claude -r` cannot resume it. Listing one
    /// produces a row that duplicates its parent's identity and cannot be adopted.
    #[test]
    fn only_a_sessions_own_transcript_is_a_listing_candidate() {
        assert!(is_root_transcript(Path::new("/p/9f1b2c3d.jsonl")));
        // A delegate, a workflow delegate, and a workflow journal are all interior structure.
        assert!(!is_root_transcript(Path::new(
            "/p/s/subagents/agent-af9bb.jsonl"
        )));
        assert!(!is_root_transcript(Path::new(
            "/p/s/subagents/workflows/wf_1/agent-af9bb.jsonl"
        )));
        assert!(!is_root_transcript(Path::new(
            "/p/s/subagents/workflows/wf_1/journal.jsonl"
        )));
        // A sidecar is not a transcript.
        assert!(!is_root_transcript(Path::new(
            "/p/s/subagents/agent-af9bb.meta.json"
        )));
    }

    /// A listing row is built from a window, and every field it claims is the vendor's.
    #[test]
    fn a_listing_row_reports_what_the_transcript_recorded() {
        let path = fixture("ordinary-turn-session-start.jsonl");
        let candidate = SessionCandidate::of(&path).expect("the fixture is a file");
        let summary = summarize(&candidate).expect("it records a sessionId");

        assert_eq!(
            summary.native_id.as_str(),
            "04905097-e558-4cd8-a563-3e03c46d42ee"
        );
        assert_eq!(summary.workdir.as_deref(), Some("/Users/hsw/.agents"));
        // The vendor's own `ai-title`, which it records for ~93% of sessions here.
        assert_eq!(
            summary.title.as_deref(),
            Some("审查并优化 discord-me 项目架构")
        );
        // Blocks rather than messages: the store flushes per content block, measured on this
        // very fixture — four assistant records inside 662 ms sharing one `message.id`.
        assert_eq!(summary.fidelity, Fidelity::Blocks);
        // Both stamps are the vendor's, and the end is after the start.
        let started = summary.started_at_ms.expect("the first record is stamped");
        let ended = summary.ended_at_ms.expect("the last record is stamped");
        assert!(ended >= started, "{ended} must not precede {started}");
        // Never synthesized: this field's contract is "when the store can say cheaply".
        assert_eq!(summary.event_count, None);
    }

    /// A title the window misses is absent rather than wrong.
    ///
    /// The 32 KB head finds 94.7% of the titles present, so ~5% resolve to `None` — which the
    /// server's opening-ask fallback handles. A *wrong* title would be a lie a journal makes
    /// permanent, so the failure direction is the safe one by construction.
    #[test]
    fn a_session_with_no_recorded_title_reports_none() {
        let path = fixture("usage-and-model-thinking-tokens.jsonl");
        let candidate = SessionCandidate::of(&path).unwrap();
        let summary = summarize(&candidate).expect("it records a sessionId");
        assert_eq!(summary.title, None, "absent beats invented");
        assert_eq!(
            summary.native_id.as_str(),
            "badddc0a-5946-486c-bda2-6779273679b6"
        );
    }

    /// The workdir filter is an exact match, never a substring.
    ///
    /// The store this replaces compiled it to `LIKE '%' || ? || '%'`, so asking for `/w/app`
    /// also returned `/w/app-old` and every subdirectory.
    #[test]
    fn the_workdir_filter_does_not_match_a_sibling_directory() {
        assert_eq!(normalize_workdir("/w/app/"), "/w/app");
        assert_eq!(normalize_workdir("/w/app"), "/w/app");
        // The failure the substring match produced, as a direct comparison.
        assert_ne!(normalize_workdir("/w/app-old"), normalize_workdir("/w/app"));
        assert_ne!(normalize_workdir("/w/app/sub"), normalize_workdir("/w/app"));
    }

    /// An assistant record's blocks arrive as their own kinds, with a tool call's input parsed.
    #[test]
    fn thinking_prose_and_tool_calls_are_projected_as_their_own_kinds() {
        let events = project("tool-call-and-result.jsonl");

        let thinking = of_kind(&events, EventKind::Thinking);
        assert!(
            !thinking.is_empty(),
            "the fixture opens with a thinking block"
        );
        assert!(thinking.iter().any(|event| event
            .text
            .contains("starting to explore the discord-me project")));

        assert!(of_kind(&events, EventKind::Assistant)
            .iter()
            .any(|event| event.text.contains("I'll start by exploring")));

        // A tool call's input reaches `data.input` as the **object** it is. The live stream
        // sends it as streamed partial JSON, so this is where the native shape and the live one
        // are reconciled — and asserting on a field inside it is what proves the round trip,
        // since a raw string would satisfy a presence check.
        // **On the `completed` phase, not `started`.** A `tool_call`'s `completed` means its
        // *arguments* finished streaming (`Phase::Completed`), and that is the only phase that
        // can carry a parsed input — live, the input arrives as deltas between the two, so at
        // `started` there is nothing to parse yet.
        let calls = of_kind(&events, EventKind::ToolCall);
        let read = calls
            .iter()
            .find(|event| {
                event.name.as_deref() == Some("Read") && event.phase == Some(Phase::Completed)
            })
            .expect("the fixture holds a Read");
        let input = read
            .data
            .as_ref()
            .and_then(|data| data.get("input"))
            .expect("a call's input must travel");
        assert_eq!(
            input.get("file_path"),
            Some(&json!("/Users/hsw/.agents/skills/discord-me/SKILL.md")),
            "the input must arrive parsed, not as a JSON string"
        );
    }

    /// A record type the vocabulary has no kind for is dropped, not invented into one.
    ///
    /// Measured, these are ~30% of all records: `mode`, `permission-mode`, `attachment`,
    /// `queue-operation`, `file-history-snapshot`, `ai-title`, `last-prompt`, and nine more.
    /// Several are read by [`summarize`] for a listing row, which is where they belong.
    /// Inventing an event kind is what `crates/adapters/src/lib.rs` forbids an adapter to do.
    #[test]
    fn store_bookkeeping_does_not_become_events() {
        let events = project("injected-boilerplate-slash-command.jsonl");
        // The fixture is a `/model` slash command plus its two queue operations and an
        // attachment — no transcript content at all except the local command's own output.
        for event in &events {
            assert!(
                !matches!(event.kind, EventKind::Assistant | EventKind::ToolCall),
                "{:?} is bookkeeping and must not become a transcript event",
                event.kind
            );
        }
    }

    /// A `system` subtype the agent authored reaches a reader; harness bookkeeping does not.
    #[test]
    fn an_away_summary_is_a_notice_and_a_stop_hook_is_not() {
        let away = project("system-subtype-away-summary.jsonl");
        let notices = of_kind(&away, EventKind::Notice);
        assert_eq!(notices.len(), 1, "the agent's own account of its work");
        assert!(notices[0].text.contains("Reviewed the discord-me project"));

        // A scheduled wakeup is the agent speaking too.
        let fired = project("system-subtype-scheduled-task-fire.jsonl");
        assert_eq!(of_kind(&fired, EventKind::Notice).len(), 1);

        // A hook summary is the harness, and 626 of them would bury a transcript.
        assert!(
            project("system-subtype-stop-hook-summary.jsonl").is_empty(),
            "harness bookkeeping must not reach the transcript"
        );
    }

    /// A file whose every line is unparseable **fails loudly** rather than reading as empty.
    ///
    /// The two are indistinguishable downstream and mean opposite things
    /// (`crate::native::JsonLines`). `AGENTS.md` § Compatibility: fail on a shape no known era
    /// emits rather than tolerating it.
    #[test]
    fn a_wholly_unparseable_transcript_is_an_error_and_not_an_empty_session() {
        let dir =
            std::env::temp_dir().join(format!("sylph-claude-native-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let broken = dir.join("broken.jsonl");
        std::fs::write(&broken, "not json\nalso not json\n").unwrap();
        let error = parse_transcript(&broken, None).expect_err("this must not read as empty");
        assert!(
            error.to_string().contains("no record in"),
            "the error must say what happened; got {error}"
        );

        // An genuinely empty file is a different, non-error fact: a session with no records yet.
        let empty = dir.join("empty.jsonl");
        std::fs::write(&empty, "").unwrap();
        let parsed = parse_transcript(&empty, None).expect("an empty session is not an error");
        assert!(parsed.messages.is_empty());

        // And a truncated final line — the normal state of a live session — costs nothing.
        let partial = dir.join("partial.jsonl");
        std::fs::write(
            &partial,
            format!(
                "{}\n{{\"type\":\"assist",
                json!({
                    "type": "assistant", "uuid": "a1", "timestamp": "2026-08-27T00:00:00.000Z",
                    "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]},
                })
            ),
        )
        .unwrap();
        let parsed = parse_transcript(&partial, None).expect("a half-written line is normal");
        assert!(
            parsed.messages.iter().any(
                |message| matches!(message, SessionMessage::Event(event) if event.text == "hi")
            ),
            "the records before a truncated line must survive"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A watcher only advances past a **complete** line, so a half-written record is re-read.
    #[test]
    fn a_follow_offset_never_advances_past_a_partial_record() {
        let dir =
            std::env::temp_dir().join(format!("sylph-claude-native-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("growing.jsonl");

        std::fs::write(&path, "{\"a\":1}\n{\"a\":2").unwrap();
        let (bytes, offset) = read_appended(&path, 0).expect("the file exists");
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "{\"a\":1}\n",
            "only the complete line is taken"
        );
        assert_eq!(offset, 8, "the offset stops at the newline");

        // The partial record completes; re-reading from the held offset yields it whole.
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        let (bytes, next) = read_appended(&path, offset).unwrap();
        assert_eq!(String::from_utf8_lossy(&bytes), "{\"a\":2}\n");
        assert_eq!(
            next, 16,
            "both complete lines, and nothing past the last newline"
        );

        // Nothing new is not an error and not an ending.
        assert_eq!(read_appended(&path, next), Some((Vec::new(), next)));
        // An absent file is not an absent session.
        assert_eq!(read_appended(&dir.join("nope.jsonl"), 0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A success and a failure must be distinguishable, which is the half a `false`-defaulting
    /// reader passes by accident.
    #[test]
    fn a_successful_tool_result_is_not_flagged() {
        let events = project("tool-call-and-result.jsonl");
        for result in of_kind(&events, EventKind::ToolResult) {
            assert!(
                !result.is_error,
                "{:?} succeeded and must not be flagged",
                result.stream_id
            );
        }
    }

    /// The bundle reader discovers a delegate after its first pass and then follows only
    /// the bytes appended after the cursor.  This is the regression that the old
    /// `watch()` could never catch: it collected delegates once, then tailed only root.
    #[test]
    fn a_bundle_reader_discovers_and_follows_a_delegate_created_late() {
        let dir = std::env::temp_dir().join(format!("sylph-claude-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("session.jsonl");
        std::fs::write(
            &transcript,
            r#"{"type":"mode","mode":"normal","sessionId":"bundle-session"}
"#,
        )
        .unwrap();
        let mut state = BundleFollowState::new(collect(transcript.clone()), RootSource::File);
        assert!(state
            .poll()
            .unwrap()
            .iter()
            .all(|message| matches!(message, SessionMessage::NativeRecord(_))));

        // Build the path from the transcript stem exactly as `collect` does.
        let subagents = transcript.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let delegate = subagents.join("agent-late.jsonl");
        std::fs::write(
            &delegate,
            r#"{"type":"assistant","timestamp":"2026-09-02T00:00:00.001Z","message":{"role":"assistant","content":[{"type":"text","text":"first"}]}}
"#,
        )
        .unwrap();

        let first = state.poll().unwrap();
        assert!(first.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event)
                if event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("late")
        )));
        assert!(first.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "first"
        )));
        assert!(first.iter().any(|message| matches!(
            message,
            SessionMessage::NativeRecord(record)
                if record.source == "subagents/agent-late.jsonl"
                    && record.payload["type"] == "assistant"
        )));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&delegate)
            .unwrap();
        use std::io::Write;
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2026-09-02T00:00:00.002Z","message":{{"role":"assistant","content":[{{"type":"text","text":"second"}}]}}}}"#
        )
        .unwrap();
        let second = state.poll().unwrap();
        assert!(second.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "second"
        )));
        assert!(second.iter().all(|message| !matches!(
            message,
            SessionMessage::Event(event) if event.text == "first"
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Manifest cursors are versioned snapshots: an in-place rewrite produces a new
    /// version instead of being mistaken for an append or silently replacing the previous
    /// observation.
    #[test]
    fn a_manifest_rewrite_advances_its_snapshot_version() {
        let dir =
            std::env::temp_dir().join(format!("sylph-claude-manifest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("session.jsonl");
        std::fs::write(
            &transcript,
            r#"{"type":"mode","sessionId":"bundle-session"}
"#,
        )
        .unwrap();
        let workflow_dir = transcript.with_extension("").join("workflows");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        let manifest = workflow_dir.join("wf_run.json");
        std::fs::write(
            &manifest,
            r#"{"runId":"wf_run","workflowProgress":[{"agentId":"a1","state":"start"}]}
"#,
        )
        .unwrap();
        let mut state = BundleFollowState::new(collect(transcript), RootSource::File);
        state.poll().unwrap();
        let first_revision = state.manifests.get(&manifest).unwrap().version;
        std::fs::write(
            &manifest,
            r#"{"runId":"wf_run","workflowProgress":[{"agentId":"a1","state":"done"}]}
"#,
        )
        .unwrap();
        state.poll().unwrap();
        let second_revision = state.manifests.get(&manifest).unwrap().version;
        assert_ne!(first_revision, second_revision);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Native Agent closes arrive as XML embedded in a tool result. The parser must feed
    /// the exact live notification shape to `Correlation`, including the numeric usage
    /// payload, so the close lands on the delegate rather than becoming assistant prose.
    #[test]
    fn an_embedded_task_notification_closes_the_agent_trajectory() {
        let text = r#"<task-notification>
<task-id>agent-42</task-id>
<tool-use-id>toolu_agent</tool-use-id>
<status>completed</status>
<summary>finished the investigation</summary>
<output-file>/tmp/agent-42.txt</output-file>
<usage><input_tokens>12</input_tokens><output_tokens>34</output_tokens></usage>
</task-notification>"#;
        let notifications = task_notifications(text);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0]["subtype"], "task_notification");
        assert_eq!(notifications[0]["task_id"], "agent-42");
        assert_eq!(notifications[0]["usage"]["output_tokens"], 34);

        let mut correlation = Correlation::new();
        // The host call is what makes the close a trajectory rather than a root notice.
        let host = json!({
            "type": "stream_event",
            "event": {"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_agent", "name": "Agent"}}
        });
        let _ = correlation.translate(&host, 1);
        let mut notification = notifications[0].clone();
        notification["task_type"] = json!("local_agent");
        let events = correlation.translate(&notification, 2).messages;
        assert!(events.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event)
                if event.kind == EventKind::Subagent
                    && event.phase == Some(Phase::Completed)
                    && event.stream_id.as_deref() == Some("agent-42")
                    && event.text == "finished the investigation"
                    && event.data.as_ref().and_then(|data| data.get("output_file"))
                        == Some(&json!("/tmp/agent-42.txt"))
                    && event
                        .data
                        .as_ref()
                        .and_then(|data| data.get("usage"))
                        .and_then(|usage| usage.get("output_tokens"))
                        == Some(&json!(34))
        )));
    }

    #[test]
    fn a_synchronous_native_agent_result_closes_once() {
        let mut correlation = Correlation::new();
        let host = json!({
            "type": "stream_event",
            "event": {"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_agent", "name": "Agent"}}
        });
        let _ = correlation.translate(&host, 1);
        let record = json!({
            "type": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_agent", "content": "done"
            }]},
            "toolUseResult": {"agentId": "agent-sync", "isAsync": false, "status": "completed", "stdout": "done"}
        });
        let messages = project_record(&mut correlation, &record, 2);
        let closes: Vec<&Event> = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event)
                    if event.kind == EventKind::Subagent
                        && event.phase == Some(Phase::Completed) =>
                {
                    Some(event)
                }
                _ => None,
            })
            .collect();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].stream_id.as_deref(), Some("agent-sync"));
        assert_eq!(closes[0].span.as_deref(), Some("toolu_agent"));

        // The async launch shape is explicitly not a close.
        let async_record = json!({
            "type": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_agent", "content": "launched"
            }]},
            "toolUseResult": {"agentId": "agent-async", "isAsync": true, "status": "async_launched"}
        });
        let messages = project_record(&mut correlation, &async_record, 3);
        assert!(!messages.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event)
                if event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("agent-async")
                    && event.phase == Some(Phase::Completed)
        )));
    }

    #[test]
    fn a_resumed_bundle_starts_after_the_existing_delegate_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "sylph-claude-resume-baseline-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = "8a8b8c8d-1e2f-43a4-b5c6-d7e8f9012345";
        let transcript = dir.join(format!("{session}.jsonl"));
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({"type":"assistant","sessionId":session,"message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_agent","name":"Agent","input":{}}]}})
            ),
        )
        .unwrap();
        let subagents = transcript.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let delegate = subagents.join("agent-old.jsonl");
        std::fs::write(
            &delegate,
            format!(
                "{}\n",
                json!({"type":"assistant","agentId":"old","message":{"role":"assistant","content":[{"type":"text","text":"old-history"}]}})
            ),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-old.meta.json"),
            r#"{"toolUseId":"toolu_agent"}"#,
        )
        .unwrap();

        let mut state = BundleFollowState::new_with_baseline(
            collect(transcript),
            RootSource::Stdout,
            FollowBaseline::Current,
        );
        let initial = state.poll().unwrap();
        assert!(!initial.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "old-history"
        )));

        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new().append(true).open(&delegate).unwrap(),
            "{}",
            json!({"type":"assistant","agentId":"old","message":{"role":"assistant","content":[{"type":"text","text":"new-live"}]}})
        )
        .unwrap();
        let appended = state.poll().unwrap();
        assert!(appended.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "new-live"
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn late_sidecar_classification_does_not_publish_a_duplicate_prefix() {
        let dir =
            std::env::temp_dir().join(format!("sylph-claude-late-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = "9a9b9c9d-1e2f-43a4-b5c6-d7e8f9012345";
        let transcript = dir.join(format!("{session}.jsonl"));
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({"type":"assistant","sessionId":session,"message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_task","name":"Task","input":{}}]}})
            ),
        )
        .unwrap();
        let subagents = transcript.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let delegate = subagents.join("agent-late.jsonl");
        std::fs::write(
            &delegate,
            format!(
                "{}\n",
                json!({"type":"assistant","agentId":"late","message":{"role":"assistant","content":[{"type":"text","text":"prefix"}]}})
            ),
        )
        .unwrap();
        let mut state = BundleFollowState::new(collect(transcript), RootSource::Stdout);
        let first = state.poll().unwrap();
        assert!(!first.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "prefix"
        )));
        std::fs::write(
            subagents.join("agent-late.meta.json"),
            r#"{"toolUseId":"toolu_task"}"#,
        )
        .unwrap();
        let second = state.poll().unwrap();
        assert!(!second.iter().any(|message| matches!(
            message,
            SessionMessage::Event(event) if event.text == "prefix"
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live bundle follower must survive a poll, and must suppress only what stdout
    /// already carries.
    ///
    /// Both halves are one test because they share a fixture and the first hides the second:
    /// the follower used to abort on its first poll, so the suppression rule below had never
    /// executed in a running session. An `Agent` interior exists *only* on disk
    /// (`docs/agents.md`: `--forward-subagent-text` forwards for the built-in `Task` alone),
    /// so suppressing it loses work; a `Task` interior arrives on stdout too, so emitting it
    /// from the file duplicates work.
    #[tokio::test]
    async fn a_live_bundle_follow_suppresses_only_what_stdout_carries() {
        use futures::StreamExt;

        let root =
            std::env::temp_dir().join(format!("sylph-claude-live-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = "5f6a7b8c-9d0e-41f2-a3b4-c5d6e7f80912";
        let subagents = root.join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();

        // Two hosts in the root transcript: one `Task`, one `Agent`.
        let transcript = root.join(format!("{session}.jsonl"));
        std::fs::write(
            &transcript,
            [
                json!({"type":"assistant","uuid":"r1","sessionId":session,
                       "timestamp":"2026-08-27T00:00:01.000Z",
                       "message":{"role":"assistant","content":[
                           {"type":"tool_use","id":"toolu_task_host","name":"Task","input":{}}]}}),
                json!({"type":"assistant","uuid":"r2","sessionId":session,
                       "timestamp":"2026-08-27T00:00:02.000Z",
                       "message":{"role":"assistant","content":[
                           {"type":"tool_use","id":"toolu_agent_host","name":"Agent","input":{}}]}}),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
                + "\n",
        )
        .unwrap();

        for (agent, host, text) in [
            ("task0delegate", "toolu_task_host", "TASK_INTERIOR"),
            ("agent0delegate", "toolu_agent_host", "AGENT_INTERIOR"),
        ] {
            std::fs::write(
                subagents.join(format!("agent-{agent}.jsonl")),
                format!(
                    "{}\n",
                    json!({"type":"assistant","uuid":format!("{agent}-1"),"sessionId":session,
                           "isSidechain":true,"agentId":agent,
                           "timestamp":"2026-08-27T00:00:03.000Z",
                           "message":{"role":"assistant","content":[
                               {"type":"text","text":text}]}})
                ),
            )
            .unwrap();
            std::fs::write(
                subagents.join(format!("agent-{agent}.meta.json")),
                json!({"toolUseId": host, "agentType": "general-purpose"}).to_string(),
            )
            .unwrap();
        }

        // `follow` rather than `watch_live`: the latter resolves through the real store
        // root, and a test must not depend on the developer's own `~/.claude`.
        let mut stream = follow(collect(transcript), RootSource::Stdout);

        // Drain one poll's worth. A panic inside the follower fails here rather than
        // surfacing three layers away as a dead observe facet.
        let mut text = String::new();
        for _ in 0..64 {
            match tokio::time::timeout(std::time::Duration::from_millis(400), stream.next()).await {
                Ok(Some(Ok(SessionMessage::Event(event)))) => text.push_str(&event.text),
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => panic!("the follower reported {error}"),
                Ok(None) | Err(_) => break,
            }
        }

        assert!(
            text.contains("AGENT_INTERIOR"),
            "an Agent interior exists only on disk and must be read from the file"
        );
        assert!(
            !text.contains("TASK_INTERIOR"),
            "a Task interior arrives on stdout, so the file copy must be suppressed"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A suppressed delegate stays suppressed as it grows, and its records are still kept.
    ///
    /// Suppression covers a *file*, not the bytes that happened to exist when it was first
    /// seen. A delegate is written while its subagent runs, so a rule that fires once at
    /// first sight suppresses whatever landed inside one poll interval and duplicates the
    /// whole remaining interior — which is worse than not suppressing at all, because the
    /// prefix and the suffix disagree about what the rule was.
    ///
    /// The raw records are the other half: projection is what stdout already carries, and a
    /// vendor record is the one thing that cannot be recovered later (`AGENTS.md`
    /// § Compatibility). So the file is always read losslessly and only its *events* are
    /// withheld — the same split `read_root` makes for the root transcript.
    #[test]
    fn a_suppressed_delegate_stays_suppressed_as_it_grows() {
        let root = std::env::temp_dir().join(format!(
            "sylph-claude-suppressed-growth-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = "7a8b9c0d-1e2f-43a4-b5c6-d7e8f9012345";
        let subagents = root.join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();

        let transcript = root.join(format!("{session}.jsonl"));
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({"type":"assistant","uuid":"r1","sessionId":session,
                       "timestamp":"2026-08-27T00:00:01.000Z",
                       "message":{"role":"assistant","content":[
                           {"type":"tool_use","id":"toolu_task","name":"Task","input":{}}]}})
            ),
        )
        .unwrap();

        let delegate = subagents.join("agent-grower.jsonl");
        std::fs::write(
            &delegate,
            format!(
                "{}\n",
                json!({"type":"assistant","uuid":"g-1","sessionId":session,"isSidechain":true,
                       "agentId":"grower","timestamp":"2026-08-27T00:00:02.000Z",
                       "message":{"role":"assistant","content":[
                           {"type":"text","text":"FIRST_PREFIX"}]}})
            ),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-grower.meta.json"),
            json!({"toolUseId":"toolu_task","agentType":"general-purpose"}).to_string(),
        )
        .unwrap();

        let mut state = BundleFollowState::new(collect(transcript), RootSource::Stdout);
        let first = state.poll().expect("the first pass must succeed");

        // The subagent keeps writing after that first poll.
        {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .open(&delegate)
                .unwrap();
            writeln!(
                handle,
                "{}",
                json!({"type":"assistant","uuid":"g-2","sessionId":session,"isSidechain":true,
                       "agentId":"grower","timestamp":"2026-08-27T00:00:03.000Z",
                       "message":{"role":"assistant","content":[
                           {"type":"text","text":"LATER_SUFFIX"}]}})
            )
            .unwrap();
        }
        let second = state.poll().expect("the second pass must succeed");

        let projected: String = first
            .iter()
            .chain(second.iter())
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            !projected.contains("FIRST_PREFIX") && !projected.contains("LATER_SUFFIX"),
            "stdout carries this Task interior, so no pass may project it; got {projected:?}"
        );

        let retained: Vec<String> = first
            .iter()
            .chain(second.iter())
            .filter_map(|message| match message {
                SessionMessage::NativeRecord(record) => Some(record.payload.to_string()),
                _ => None,
            })
            .collect();
        let retained = retained.join(" ");
        assert!(
            retained.contains("FIRST_PREFIX") && retained.contains("LATER_SUFFIX"),
            "a suppressed delegate's records are still the only copy and must be retained"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A delegate is called what stdout calls it, so the card does not relabel itself.
    ///
    /// A sidecar carrying both fields is the case that matters: `description` is the brief a
    /// live `task_started` also reads, and `name` is a slug only the file has. Reading the slug
    /// here meant the file's declaration overwrote stdout's a poll later, so the card changed
    /// under the reader and kept the less informative half.
    #[test]
    fn a_delegate_is_named_what_stdout_would_call_it() {
        let root =
            std::env::temp_dir().join(format!("sylph-claude-delegate-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let both = root.join("agent-both.jsonl");
        std::fs::write(&both, "").unwrap();
        std::fs::write(
            root.join("agent-both.meta.json"),
            json!({"name": "stage-a-slug", "description": "Stage A: remove IngestMode"})
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            delegate_name(&both).as_deref(),
            Some("Stage A: remove IngestMode"),
            "the brief is what stdout names the same delegation"
        );

        // No sidecar on this store carries a `name` without a `description`, but the fallback
        // is kept so such a shape is named rather than anonymous.
        let slug_only = root.join("agent-slug.jsonl");
        std::fs::write(&slug_only, "").unwrap();
        std::fs::write(
            root.join("agent-slug.meta.json"),
            json!({"name": "slug-only"}).to_string(),
        )
        .unwrap();
        assert_eq!(
            delegate_name(&slug_only).as_deref(),
            Some("slug-only"),
            "a slug is better than no name at all"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
