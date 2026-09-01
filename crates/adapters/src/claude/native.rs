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
//! <project>/<session-uuid>/subagents/workflows/wf_<id>/agent-*.jsonl a workflow's delegate
//! <project>/<session-uuid>/subagents/workflows/wf_<id>/journal.jsonl phase outcomes
//! ```
//!
//! The root is hardcoded: a native store's location is a vendor fact with no configuration
//! key, and inventing one would offer a lever that cannot be right.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sylph_protocol::{
    normalize_workdir, Event, EventKind, EventStream, Fidelity, NativeId, NativeSummary, Phase,
    SessionMessage, StoreFilter,
};

use super::translate::Correlation;
use crate::native::{candidates, epoch_millis_from_iso, for_each_json_line, SessionCandidate};

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
    let messages = parse_session(&session)?;
    Ok(Box::pin(futures::stream::iter(
        messages.into_iter().map(Ok),
    )))
}

/// Parse, then follow the transcript as it grows.
pub fn watch(id: &NativeId) -> Result<EventStream> {
    let session = locate(id)?;
    Ok(follow(session))
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

/// One session's files: its transcript, and the delegates recorded beside it.
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

/// The delegates and journals sitting beside one transcript.
fn collect(transcript: PathBuf) -> SessionFiles {
    // `<session>.jsonl` has its delegates under `<session>/subagents/`.
    let interior = transcript.with_extension("");
    let mut delegates = Vec::new();
    let mut journals = Vec::new();
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
    // Oldest first, so a delegate that spawned another is parsed before its child and the
    // trajectory tree is built in one pass rather than needing a second attachment pass.
    delegates.sort();
    journals.sort();
    SessionFiles {
        transcript,
        delegates,
        journals,
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
/// The transcript is parsed first, then each delegate, then the workflow journals. Each
/// file gets its **own** [`Correlation`], because the correlation state is per-stream: a
/// delegate's content-block indices are its own, and sharing one map across files would
/// reintroduce the index collision `translate::BlockKey` exists to prevent, here between
/// files rather than between concurrent delegates.
///
/// A delegate's events are then re-parented onto the trajectory that spawned it, which is
/// what [`delegate_spans`] resolves.
fn parse_session(session: &SessionFiles) -> Result<Vec<SessionMessage>> {
    let mut messages = Vec::new();
    let spans = delegate_spans(session);

    let root = parse_transcript(&session.transcript, None)?;
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

        messages.extend(parsed.messages);
    }

    for journal in &session.journals {
        messages.extend(parse_journal(journal)?);
    }

    Ok(messages)
}

/// One file's records, projected.
#[derive(Debug)]
struct ParsedFile {
    messages: Vec<SessionMessage>,
    /// The first timestamp seen, for the synthesized trajectory declaration above.
    started_at_ms: i64,
}

/// Project one JSONL file through the live translator.
///
/// `span` is the trajectory every event in this file belongs to — set for a delegate,
/// `None` for the session's own transcript.
fn parse_transcript(path: &Path, span: Option<String>) -> Result<ParsedFile> {
    let mut correlation = Correlation::new();
    let mut messages = Vec::new();
    let mut started_at_ms = 0;

    let stats = for_each_json_line(path, |record| {
        let at_ms = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_millis_from_iso)
            .unwrap_or(started_at_ms);
        if started_at_ms == 0 {
            started_at_ms = at_ms;
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
    })
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
    let kind = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The delegating call whose output this is, when the record is a delegate's. Live, this
    // rides on the envelope as `parent_tool_use_id`; natively the store spells the same
    // fact `agentId`, and it is resolved to a trajectory by the caller rather than here.
    match kind {
        // `user` carries tool results, an interrupt, or something the person typed.
        "user" => {
            if let Some(interrupt) = user_interrupt(record, at_ms) {
                return vec![SessionMessage::Event(interrupt)];
            }
            if has_tool_result(record) {
                // Straight through: the live `user` payload and this record are the same
                // shape, which is the fact the whole reuse rests on.
                return correlation.translate(record, at_ms).messages;
            }
            // Something the person typed. Not projected as an event: the vocabulary has no
            // `user` kind, and the server owns a turn's `request_text`
            // (`docs/database-schema.md`). Inventing a kind is what
            // `crates/adapters/src/lib.rs` forbids an adapter from doing.
            vec![]
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
/// **`None` when the sidecar does not name it**, which is 1919 of 2209 measured sidecars.
/// `name` is the agent's chosen label and `description` is its brief; `agentType` is
/// deliberately **not** used as a fallback even though every sidecar has one, because it is
/// the *kind* of agent (`general-purpose`, `Explore`, `workflow-subagent`) rather than what
/// this delegation is — and a timeline of nine rows reading "general-purpose" is the
/// unreadable card `translate::task_label` already refuses to produce. A guess written here
/// becomes permanent and is then indistinguishable from a name an agent chose
/// (`docs/session-protocol.md` § `name` is absent when unknown).
fn delegate_name(path: &Path) -> Option<String> {
    let sidecar = read_sidecar(path)?;
    string_at(&sidecar, "name").or_else(|| string_at(&sidecar, "description"))
}

/// A delegate's `agent-<hash>.meta.json`, when it has one.
fn read_sidecar(path: &Path) -> Option<Value> {
    let sidecar = path.with_extension("meta.json");
    let text = std::fs::read_to_string(sidecar).ok()?;
    serde_json::from_str(&text).ok()
}

/// Which trajectory each delegate's work belongs inside, keyed by `agentId`.
///
/// # `parentAgentId`, not directory nesting
///
/// The files sit **flat** in one `subagents/` directory however deep the delegation went, so
/// reading one path component up to find a parent attributes every delegate to the root
/// session and a depth-2 tree comes out one level deep. Depth is expressed by
/// `parentAgentId` inside the child's own sidecar — a minority of sidecars carry one, and
/// `fixtures/native/claude/delegation-task-fanout-nested.*` is a 3-way fan-out where each
/// depth-1 delegate spawns one depth-2 child.
///
/// A delegate's trajectory id **is its `agentId`**, so this map answers the other question:
/// which trajectory *contains* it. Three routes, in order of how directly the store states
/// it:
///
/// 1. `parentAgentId` → the parent delegate's trajectory. The only route that expresses
///    depth, and the one whose absence caused the flattening.
/// 2. `toolUseId` → the delegating `tool_use` in the parent's transcript, which promotes
///    that call in place (`docs/session-protocol.md` § Promotion). 599 of 2209 sidecars.
/// 3. `toolUseResult.agentId` in the parent's transcript, which pairs a delegate to the call
///    whose result announced it. 583 records, and it overlaps route 2 rather than extending
///    it — measured, it adds **zero** delegates route 2 misses, so it is *not* consulted.
///    Recorded here because its uselessness is the measurement: a reader would otherwise
///    reasonably add it.
///
/// **1613 of 2212 delegates have no route at all**, and that is a real limit rather than a
/// gap to paper over: 1379 are workflow delegates whose sidecars carry neither field, and
/// 234 are plain delegates whose sidecars carry only `agentType` or a `taskKind` of
/// `in_process_teammate`. Their events are emitted at the **root**, because an unknown span
/// renders at the root and dropping them would lose work exactly where structure is least
/// reliable.
///
/// A workflow delegate is deliberately **not** attached to its host `Workflow` call by
/// guessing. The `wf_<id>` directory name appears **nowhere** in the parent transcript
/// (measured: 0 of 23), and 11 of 20 workflow directories sit in sessions whose count of
/// `Workflow` calls differs from their count of `wf_` directories — so pairing them by
/// position would be a guess that is wrong whenever a session ran more than one workflow,
/// and wrong invisibly.
fn delegate_spans(session: &SessionFiles) -> HashMap<String, String> {
    // `agentId` → the `tool_use` id that spawned it, when a sidecar names one.
    let mut by_tool_use: HashMap<String, String> = HashMap::new();
    // `agentId` → its parent delegate's `agentId`, when a sidecar names one.
    let mut by_parent: HashMap<String, String> = HashMap::new();

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

/// Parse, then follow the transcript as it grows.
///
/// # Why a poll and not a filesystem watch
///
/// A watch would need a per-platform notification API and would still have to re-read from
/// an offset on every event, because the vendor appends rather than rewriting. The poll is
/// the same read with a timer in front of it, and it also handles the case a watch does not:
/// a file that does not exist yet. `claude` creates a transcript on the first record, so a
/// session opened and not yet spoken to has no file, and **an absent file is not an absent
/// session** (`sylph_protocol::Adapter::watch`).
///
/// The correlation state is held **across** polls, not rebuilt per poll. Rebuilding would
/// re-emit the whole history on every tick; holding it means each poll projects only what
/// arrived. The offset is in bytes and only advances past a **complete** line, so a poll that
/// catches a half-written record re-reads it next time rather than skipping it — which is the
/// mid-write hazard `crate::native::for_each_json_line` documents, handled here by not
/// committing the offset rather than by tolerating a broken record.
fn follow(session: SessionFiles) -> EventStream {
    /// Fast enough to feel live against a store that flushes per content block, slow enough
    /// that an idle session costs one `stat` per interval.
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);

    Box::pin(futures::stream::unfold(
        FollowState {
            session,
            offset: 0,
            correlation: Correlation::new(),
            replayed: false,
            pending: std::collections::VecDeque::new(),
        },
        |mut state| async move {
            loop {
                if let Some(message) = state.pending.pop_front() {
                    return Some((Ok(message), state));
                }

                // The first pass covers the delegates and journals too, so a watched session
                // starts from everything already recorded rather than from the next append.
                if !state.replayed {
                    state.replayed = true;
                    match parse_session(&state.session) {
                        Ok(messages) => {
                            state.offset = std::fs::metadata(&state.session.transcript)
                                .map(|meta| meta.len())
                                .unwrap_or(0);
                            state.pending.extend(messages);
                            continue;
                        }
                        Err(error) => return Some((Err(error), state)),
                    }
                }

                match read_appended(&state.session.transcript, state.offset) {
                    Some((bytes, next_offset)) if !bytes.is_empty() => {
                        state.offset = next_offset;
                        for record in json_lines(&bytes, Edge::Head) {
                            let at_ms = record
                                .get("timestamp")
                                .and_then(Value::as_str)
                                .and_then(epoch_millis_from_iso)
                                .unwrap_or(0);
                            let projected = project_record(&mut state.correlation, &record, at_ms);
                            state.pending.extend(projected);
                        }
                        continue;
                    }
                    // No file yet, or nothing new. Either way the session is not over: a
                    // watcher ends when its consumer drops it, not when the agent goes quiet.
                    _ => tokio::time::sleep(POLL).await,
                }
            }
        },
    ))
}

/// What one `watch` remembers between polls.
struct FollowState {
    session: SessionFiles,
    /// Bytes of the transcript already projected. Only advanced past a complete line.
    offset: u64,
    correlation: Correlation,
    replayed: bool,
    pending: std::collections::VecDeque<SessionMessage>,
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
        let spans = delegate_spans(&session);

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
    /// *Measured: 1613 of 2212 delegates have no attribution route at all* — 1379 workflow
    /// delegates whose sidecars carry neither `toolUseId` nor `parentAgentId`, and 234 plain
    /// ones carrying only `agentType` or an `in_process_teammate` `taskKind`. That is a real
    /// limit of the store. `docs/session-protocol.md`: an unknown span renders at the root,
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
            delegate_spans(&session_files).is_empty(),
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
}
