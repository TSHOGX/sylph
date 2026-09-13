//! The `codex` native store: `rollout-*.jsonl` under `~/.codex/sessions`.
//!
//! # Status
//!
//! [`adopt`], [`watch`], and [`enumerate`] are what `codex/mod.rs` calls, and their bodies
//! are the parser and generation-aware follower. What is already settled — and is the
//! expensive part —
//! is *what the store actually contains*, measured over every rollout on this
//! machine rather than read off `docs/agents.md`, which was written from a second-hand
//! description and is wrong in several places. Each measurement below is stated where
//! the code that acts on it will go, because each one is a branch a reader would
//! otherwise write from the document and each one fails silently.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sylph_protocol::{
    normalize_workdir, Event, EventKind, EventStream, Fidelity, InteriorState, NativeId,
    NativeRecord, NativeSummary, Phase, SessionMessage, StoreFilter,
};

use crate::native::{candidates, epoch_millis_from_iso, SessionCandidate};

/// Where `codex` keeps its rollouts, `$HOME`-relative.
///
/// The segments live in [`super::STORE_SEGMENTS`] rather than here, because the store root
/// is a third fact independent of both the agent name and the binary name — see that
/// constant for why it is not assembled from either.
fn store_root() -> Option<PathBuf> {
    crate::native::home_relative(super::STORE_SEGMENTS)
}

/// Stable source name for a rollout JSONL file.
///
/// The native store has one file per rollout (including descendants), so the filename is
/// already the vendor-relative identity we need. Keeping the absolute path out of the record
/// makes the same session portable between machines while still distinguishing child rollouts.
fn native_source(path: &Path) -> String {
    store_root()
        .and_then(|root| path.strip_prefix(root).ok().map(Path::to_path_buf))
        .or_else(|| path.file_name().map(PathBuf::from))
        .and_then(|relative| relative.to_str().map(str::to_string))
        .unwrap_or_else(|| "rollout.jsonl".to_string())
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn native_record(path: &Path, version: u64, value: &Value, raw: Vec<u8>) -> NativeRecord {
    NativeRecord {
        source: native_source(path),
        version: crate::native::record_revision(version, &raw),
        raw_json: Some(raw),
        payload: value.clone(),
        blobs: Vec::new(),
    }
}

/// The fidelity a parsed `codex` session yields.
///
/// [`Fidelity::Blocks`] rather than [`Fidelity::Messages`], and it holds for **both**
/// eras though for different reasons, which is worth stating because `fidelity` is a
/// property of the *record* rather than of the agent (`docs/architecture.md`), so a
/// per-agent constant has to be true of every file the agent writes.
///
/// - A `paginated` file carries per-item timestamps, so an item is separately timeable.
/// - A `legacy` file carries only each record's outer timestamp — but those are distinct
///   per content record, measured within one turn at `12:15:08.073Z`, `12:15:08.951Z`,
///   `12:15:26.110Z`. So the granularity is still the item, not the message.
///
/// What is *not* recoverable in either era is anything within an item, which is what
/// separates this from [`Fidelity::Stream`]. A renderer may time an item and must not
/// time a token.
pub const NATIVE_FIDELITY: Fidelity = Fidelity::Blocks;

/// **Reasoning text is mostly unrecoverable from a rollout, and a parser must expect
/// that rather than emit empty thinking events.**
///
/// Measured over a 400-file sample: every one of 12045 `response_item/reasoning` records
/// carries `encrypted_content`, and only **967** also carry a plaintext `summary`. The
/// `item_completed` side is emptier still — `Reasoning.summary_text` and
/// `Reasoning.raw_content` are present as fields and empty in practice.
///
/// So for roughly 92% of reasoning records the content exists only as vendor ciphertext.
/// The honest projection is to emit the summary where there is one and **omit the event
/// entirely** where there is not: an empty `thinking` event renders as a blank block,
/// which reads as a rendering bug rather than as an absent record, and a journal makes
/// that permanent (`docs/session-protocol.md` § "`name` is absent when unknown" states
/// the same rule for a different field).
pub const REASONING_IS_MOSTLY_ENCRYPTED: &str =
    "reasoning content is ciphertext on ~92% of records; emit the summary or nothing";

/// Which content vocabulary one rollout file speaks.
///
/// **The discriminant is `session_meta.payload.history_mode`, not `cli_version`.**
/// `docs/agents.md` says the vocabulary changed "around cli_version 0.147"; measured
/// over every rollout, 0.147.0 appears as `paginated` on some files and as `legacy` on
/// 21, and there is a 0.150.1 file — the newest version in the store — writing
/// `history_mode: "legacy"` with zero `item_completed`. A version comparison therefore
/// picks the wrong parser on a file the vendor is still producing today.
///
/// **The two content streams are mutually exclusive per file**, which is the half of
/// this that a document-driven reader gets backwards. "Both must be read" is true of
/// the *store* and false of a *record*: a paginated file carries `item_completed` and no
/// `event_msg/agent_message`, and reading both would double-count every assistant
/// message.
///
/// # Why there is no `Overlapping` variant
///
/// A small number of files do carry both streams, and an earlier version of this enum
/// declared a third variant for them. It was unreachable: measured, **every one of those
/// files has no `history_mode` at all** — they predate the field, being the oldest
/// versions in the store — so they arrive here as [`Self::Legacy`] and the variant could
/// never be constructed. That is the declared-only-variant trap this very module
/// documents about `SubAgentActivity.kind`, reproduced one file away from the correction.
///
/// Deleting it is also the right answer rather than merely the tidy one, because
/// `Legacy` is *correct* for those files: `event_msg/agent_message` carries their content
/// and `item_completed` is an addition beside it, so a legacy reading loses nothing. The
/// shape to rely on is that `history_mode == "paginated"` is what makes `item_completed`
/// the content carrier — not a count, which drifts as the store grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `event_msg/agent_message` carries assistant text. The large majority, and the
    /// right reading for every file without a `history_mode`.
    Legacy,
    /// `event_msg/item_completed` carries it, keyed by `item.type`.
    Paginated,
}

impl Era {
    /// Read the era off a `session_meta` payload's `history_mode`.
    ///
    /// Absent on most files, which are all pre-`history_mode` and legacy — so absence is
    /// [`Era::Legacy`] rather than an error, and that is measured rather than assumed.
    pub fn of(history_mode: Option<&str>) -> Self {
        match history_mode {
            Some("paginated") => Self::Paginated,
            _ => Self::Legacy,
        }
    }
}

/// **From 0.147 the `spawn_agent` brief is ciphertext**, so a delegation's name is not
/// recoverable from the parent's own record.
///
/// `docs/agents.md` does not mention this at all, and it is the most consequential fact
/// for a delegation card: `docs/session-protocol.md` § "`name` is absent when unknown"
/// requires the name to be **omitted** rather than guessed, because a journal makes
/// every guess permanent and a row written with a placeholder is afterwards
/// indistinguishable from one that carried a real name.
pub const BRIEF_IS_CIPHERTEXT: &str =
    "the spawn_agent brief is encrypted from 0.147; a delegation's name must be absent";

/// Which way a `SubAgentActivity` record's edge points.
///
/// **`agent_thread_id` is the *other party*, not the child.** In a parent's rollout it
/// names the child; in the child's own rollout a `kind: "interacted"` record names the
/// **parent** (`agent_path: "/root"`). So mapping it to `span` unconditionally inverts
/// the edge for every child-side record — which produces a tree that is not merely
/// incomplete but backwards, and nothing downstream can detect that.
///
/// **`kind` is `started` or `interacted` only.** `docs/agents.md` lists `interrupted`
/// and `completed`; both occur **zero** times across the whole store (measured: `started` 9,
/// `interacted` 33). That is the declared-only-event trap in its usual form, and the
/// consequence is concrete: **a codex delegation has no close record in the store**, so
/// a parsed delegation's end has to come from the turn that contains it rather than
/// from a record of its own.
///
/// `CollabAgentToolCall` is not lineage either, though the document presents it as
/// such: both real instances carry `receiver_thread_ids: []` and `agents_states: {}`
/// with `tool: "wait"`, and its `status: "completed"` is the *wait's* — the paired
/// output reads `{"message":"Wait timed out.","timed_out":true}`. It marks a barrier,
/// not a delegation.
pub fn delegation_edge(activity: &Value, own_thread: &str) -> Option<Edge> {
    delegation_edge_with_path(activity, own_thread, None)
}

/// Resolve a delegation edge with the reader's own team path when available.
///
/// The native record names the *other* party. Comparing only against a hard-coded `/root`
/// path misclassifies a nested delegate whose parent is `/root/research`; path ancestry is the
/// measured direction signal and still falls back to the historical root rule for old headers
/// that did not record the reader's path.
fn delegation_edge_with_path(
    activity: &Value,
    own_thread: &str,
    own_path: Option<&str>,
) -> Option<Edge> {
    let item = activity.get("item").unwrap_or(activity);
    if item.get("type").and_then(Value::as_str) != Some("SubAgentActivity") {
        return None;
    }
    let other = string_at(item, "agent_thread_id")?;
    if other == own_thread {
        // A record naming the reader itself. **Refused rather than returned**, because either
        // direction would be a self-edge: as a child it makes a trajectory its own ancestor,
        // which is unreachable so no attachment pass can root it and the whole subtree
        // vanishes with nothing reporting it missing (`sylph_protocol::Event::span`). Measured,
        // no real record is in this state — the guard costs one comparison to make the failure
        // impossible rather than rare.
        return None;
    }
    // The other party's own position in the team, which is the evidence. `/root` is the
    // primary agent and `/root/<task_name>` is a delegate; measured over 42 real records,
    // every one carries this field and it is never equal to the reader's own path.
    let other_path = string_at(item, "agent_path")?;

    // **The other path's ancestry names the direction.** A record naming an ancestor of the
    // reader is the child's view of its parent; a descendant is the parent's view of a child.
    // Older headers did not retain the reader path, so preserve the measured `/root` fallback
    // for that population. This is deliberately path-based rather than `kind`-based: both
    // `started` and `interacted` occur on both sides of the edge.
    let names_parent = if let Some(path) = own_path {
        if path_is_ancestor(&other_path, path) {
            Some(true)
        } else if path_is_ancestor(path, &other_path) {
            Some(false)
        } else {
            // A record naming a sibling (or an unrelated team) is not enough evidence to
            // attach a trajectory in this rollout.
            None
        }
    } else {
        Some(other_path == PRIMARY_AGENT_PATH)
    }?;
    if names_parent {
        // The reader is the delegate. Its parent is the named thread, and this record is
        // *not* an opening of a trajectory — it is an interaction with the thread above.
        // Reported as such rather than as a `span`, because a child's `span` is set by
        // whoever adopts the parent, and writing one here would name the parent as a
        // trajectory *inside* the child.
        return Some(Edge::Parent(other));
    }

    // The reader is the parent, so the named thread is genuinely a delegate of it. Reported
    // with the delegate's own thread id as the trajectory, never the record's `id`: that id
    // is the *tool call* that issued the spawn — measured, all 42 match a `spawn_agent`,
    // `send_message`, or `followup_task` `call_id` in the same file — and one host
    // multiplexes N delegates (32 `send_message` records share one host per conversation),
    // so keying a trajectory by it collapses siblings onto one node.
    Some(Edge::Child {
        thread: other,
        host: string_at(item, "id"),
        // The delegate's own name, from the path the vendor assigned it. **Not from the
        // spawn brief**, which is ciphertext ([`BRIEF_IS_CIPHERTEXT`]) — but the path's last
        // segment is plaintext and is measurably the `task_name` the agent chose: all 9
        // `spawn_agent` calls in the store carry a plaintext `task_name` equal to it. So a
        // codex delegation *can* be named, which is the one place this module is happier
        // than the ciphertext measurement suggests.
        name: other_path
            .rsplit('/')
            .next()
            .filter(|segment| !segment.is_empty())
            .map(str::to_string),
    })
}

/// The primary agent's path in a codex team. Everything deeper is a delegate.
pub const PRIMARY_AGENT_PATH: &str = "/root";

fn path_is_ancestor(candidate: &str, path: &str) -> bool {
    candidate == path
        || (candidate != "/"
            && path
                .strip_prefix(candidate)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// Which way a [`delegation_edge`] points.
///
/// **Two variants rather than an `Option<String>`, because the two directions are not the
/// same fact and a single string cannot say which was found.** The stub this replaces
/// returned one, and `docs/agents.md` describes `agent_thread_id` as "the child" — together
/// that is exactly the shape that produces an inverted tree with nothing able to detect it,
/// since a reversed pointer is still a well-formed pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edge {
    /// This record names a thread delegated *from* the file being read.
    Child {
        /// The delegate's own thread id, which is its trajectory id.
        thread: String,
        /// The tool call that issued the delegation, which the trajectory nests inside
        /// (`docs/session-protocol.md` § Promotion). `None` when the record names none.
        host: Option<String>,
        /// The delegate's name, from its `agent_path`'s last segment.
        name: Option<String>,
    },
    /// This record names the thread the file being read was delegated *from*.
    ///
    /// Carries no trajectory: a delegate's own events belong to a trajectory its **parent's**
    /// reader declares, so emitting a `span` here would make the parent a node inside the
    /// child. What this is good for is lineage on a listing row, and for knowing that such a
    /// record must not open anything.
    Parent(String),
}

/// A string field, treating `""` as absent.
///
/// Same rule the sibling parsers use, for the same reason: this vendor sends `""` and omits
/// a field interchangeably, and every decision here wants one answer for both.
fn string_at(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The stamp for a record that recorded none.
///
/// Zero rather than "now": a synthesized arrival time is a lie a journal makes permanent,
/// and a reader can recognize the epoch as "the store did not say" where it cannot recognize
/// a plausible recent instant. Same constant `claude`'s and `pi`'s parsers use.
const NO_RECORDED_TIME: i64 = 0;

/// Project one complete rollout record into the normalized event vocabulary.
///
/// # One pass, one era, and no correlation object
///
/// `claude`'s parser routes native records through the live `Correlation` because a native
/// record and a live stdout payload are the *same shape* there. **That is not true here**,
/// and re-using a translator across the two would be the second-normalizer mistake in
/// reverse: this vendor spells the same concepts four ways (`super` documents the table),
/// the rollout vocabulary is `type`/`payload` while the app-server's is slash-plus-camelCase,
/// and nothing in `super::appserver` names a rollout record type. So the projection here is
/// its own, and `super` states as an invariant that no name constant is shared between the
/// halves.
///
/// What replaces the correlation state is that **this store writes closed records only**.
/// Every content record — `agent_message`, an `item_completed` item, a `function_call` — is
/// written once, whole, after the thing it describes finished. There are no deltas to
/// accumulate, so there is no block map to key and therefore no index-collision hazard of the
/// kind `translate::BlockKey` exists to prevent.
///
/// # This is the only record-level projection, and there is no whole-file twin
///
/// Keeping this operation independent from file iteration is what lets adoption and the
/// generation-aware follower share exactly one projection state. A watcher must not re-run a
/// second, subtly different parser when a line arrives after its first poll: the era, call
/// names, delegation set, and duplicate guards all belong to the rollout's cursor, so the
/// only caller is [`RolloutProjector::apply`] and the only reader of *that* is
/// [`RolloutCursor::poll`]. A whole-file `parse_rollout` sitting beside them once
/// reimplemented the iteration half and was reached only from tests, which is the
/// two-derivations defect `AGENTS.md` § Rules That Keep Costing Time names: the tests
/// confirmed a projection production never ran.
///
/// Deleting it moved two bounds it held, and each has a named owner now rather than none:
///
/// - **"nothing in this file parsed as JSON."** Owned by [`read_complete_records`], which is
///   stricter than the tolerant count that check was built on — a *complete* malformed line
///   is an error there rather than a skipped line, and the cursor does not advance past it.
/// - **"this file has no `session_meta`."** Owned by [`header_of`], and enforced before a
///   projection starts: [`locate`] refuses an id no header claims, and
///   [`RolloutFollowState::discover`] drops a candidate whose header does not read, so a
///   headerless file is never followed rather than being followed and then rejected.
fn project_record(
    record: &Value,
    era: &mut Option<Era>,
    opened: &mut HashSet<String>,
    call_names: &mut HashMap<String, String>,
    parsed: &mut ParsedRollout,
) {
    // **The outer stamp, which is a string on every record and always UTC.** Measured over all
    // 343865 records in the store: every one ends in `Z`, so the offsetless hazard documented
    // for `pi` does not arise here — but the decoder is shared anyway, because a second
    // timestamp reader is how a unit defect gets a second chance.
    let at_ms = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(epoch_millis_from_iso);
    if let Some(stamp) = at_ms {
        if parsed.started_at_ms.is_none() {
            parsed.started_at_ms = Some(stamp);
        }
        parsed.ended_at_ms = Some(stamp);
    }
    let at_ms = at_ms.unwrap_or(NO_RECORDED_TIME);

    let kind = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(payload) = record.get("payload") else {
        // Every record in this store carries one. A record without it is a shape no known era
        // emits, and this counts rather than guesses.
        parsed.unknown_records += 1;
        return;
    };

    let before = parsed.messages.len();
    match kind {
        "session_meta" => {
            // **The first one is this file's own thread; a later one is the parent's, replayed.**
            // Measured: a minority of files carry two, and in every one the second is the
            // parent's header copied into a forked delegate. A reader that let the last one win
            // would report a delegate's id, cwd, and era as its parent's.
            if parsed.header.is_none() {
                let header = Header::read(payload);
                *era = Some(header.era);
                parsed.header = Some(header);
            }
        }
        "event_msg" => {
            let current_era = era.unwrap_or(Era::Legacy);
            event_msg(payload, current_era, at_ms, opened, parsed);
        }
        "response_item" => {
            let current_era = era.unwrap_or(Era::Legacy);
            response_item(payload, current_era, at_ms, call_names, parsed);
        }
        // A compaction's payload is the replacement history. The paired boundary announcement
        // emits the event, so this record contributes no second boundary.
        "compacted" => {}
        // `turn_context` restates model/sandbox/approval configuration, not transcript events.
        "turn_context" => {}
        // Collaboration-mode metadata carries no transcript content.
        "world_state" | "inter_agent_communication_metadata" => {}
        _ => parsed.unknown_records += 1,
    }
    let recorded_at_ms = if kind == "event_msg"
        && payload.get("type").and_then(Value::as_str) == Some("item_completed")
    {
        payload.get("completed_at_ms").and_then(Value::as_i64)
    } else {
        None
    }
    .or_else(|| (at_ms != NO_RECORDED_TIME).then_some(at_ms));
    if let Some(recorded_at_ms) = recorded_at_ms {
        for message in &mut parsed.messages[before..] {
            if let SessionMessage::Event(event) = message {
                event.recorded_at_ms = Some(recorded_at_ms);
            }
        }
    }
}

/// What one pass over a rollout produced.
#[derive(Debug, Default)]
struct ParsedRollout {
    header: Option<Header>,
    messages: Vec<SessionMessage>,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    /// Records whose top-level `type` this build does not model.
    ///
    /// Counted rather than ignored so a vendor addition is *observable* — the shape a
    /// document-driven reader gets wrong is the one nobody counted. Not an error: a new
    /// top-level type is additive here, and failing the session would make a whole store
    /// unreadable on the first release that adds one.
    unknown_records: usize,
    /// Call ids whose `tool_result` an `event_msg` already produced.
    ///
    /// **Only the legacy era populates this, and it exists to keep one call from yielding two
    /// results.** In that era the exit status is on an `event_msg/*_end` and the output text is
    /// on the `response_item` beside it, keyed by the same `call_id` — measured, 10139 of
    /// 55737 outputs are preceded by such an event. Two events under one `stream_id` do not
    /// render as two rows: a consolidating fold keeps the *last* phase's fields, so the visible
    /// symptom would be a failed command silently reporting success, because the statusless
    /// copy arrives second. That is the same class as `history::consolidate` losing a name given
    /// at open, and it is why this is a set rather than a count.
    emitted_results: std::collections::HashSet<String>,
    /// Delegate threads a record has reported as `errored`.
    ///
    /// **This exists because a later close would otherwise un-report the failure.** Measured
    /// over the whole store: `collab_waiting_end` is the *only* record that ever carries
    /// `errored` (3 occurrences), while `collab_close_end` carries `completed` on 75 of 76 and
    /// `errored` on none — and on all 3 real failures the `completed` close arrives *after*
    /// the errored wait. Since `history::consolidate` keeps the closing event's fields, the
    /// second event would silently overwrite the first and the delegation would render green.
    ///
    /// So a failure, once recorded, is carried onto every later close for that thread. That is
    /// not a guess: the vendor's close record simply does not restate an outcome it already
    /// gave, and reading its `completed` as evidence of success is reading a field that means
    /// "this close finished" as though it meant "the delegate succeeded".
    failed_delegations: std::collections::HashSet<String>,
    /// Reasoning summaries an `agent_reasoning` event already emitted, verbatim.
    ///
    /// **Legacy reasoning is duplicated across the two streams, and this is what keeps it from
    /// being emitted twice.** Measured over 1185 legacy files: `agent_reasoning`'s texts and
    /// `response_item/reasoning`'s plaintext summaries are the *same multiset* on 1082 of them,
    /// so a reader taking both doubles every thinking block in the store — the same defect the
    /// [`Era`] split prevents for assistant text, in a place the era split does not reach,
    /// because both records live in the legacy vocabulary.
    ///
    /// Neither stream is a superset, which is why this is a dedup rather than a choice of one:
    /// 2927 `agent_reasoning` texts have no matching summary, and **519 summaries have no
    /// matching `agent_reasoning`**. Dropping either side loses real reasoning; keeping both
    /// with this set keeps each block exactly once.
    ///
    /// Keyed on the **text**, deliberately, because there is no id to key on: measured, a
    /// legacy `response_item/reasoning` carries `id: null` and the `agent_reasoning` event
    /// carries no id at all. Two genuinely identical thinking blocks in one session therefore
    /// collapse to one — accepted, because the alternative is doubling every one of them.
    emitted_reasoning: std::collections::HashSet<String>,
}

/// Provenance of a rollout's parent pointer.
///
/// Codex uses the same native id field for delegated children and explicit forks. Keeping the
/// distinction in the native graph prevents a fork from being advertised as a live delegated
/// interior, while `NativeSummary::parent_native_id` can continue to expose the common parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineageKind {
    Delegation,
    Fork,
}

/// What one rollout's own `session_meta` says about it.
#[derive(Debug, Clone)]
struct Header {
    native_id: Option<NativeId>,
    workdir: Option<String>,
    era: Era,
    /// The thread this one was delegated from or forked out of.
    parent_native_id: Option<NativeId>,
    lineage_kind: Option<LineageKind>,
    /// The rollout's team path, when Codex records one. This keeps nested delegation
    /// direction independent of a fixed `/root` assumption.
    agent_path: Option<String>,
    /// When this file was opened, from the **record's** outer timestamp.
    ///
    /// Filled by [`header_of`] rather than by [`Header::read`], because the payload carries a
    /// timestamp of its own that means something different — see [`header_of`] for the
    /// measured difference between the two.
    started_at_ms: Option<i64>,
}

impl Header {
    /// Read one `session_meta` payload.
    fn read(payload: &Value) -> Self {
        Self {
            // **`id`, not `session_id`.** Measured over every rollout: `id` equals the
            // filename's uuid on every single file, while `session_id` is present on 152 and
            // on 25 of those names a *different* thread — the parent's. So reading
            // `session_id` would give 25 sessions their parent's identity, which is the
            // two-id-spaces failure `sylph_protocol::NativeId` exists to prevent, arriving
            // as a plausible value rather than as an error.
            native_id: string_at(payload, "id").map(NativeId::new),
            workdir: string_at(payload, "cwd"),
            era: Era::of(payload.get("history_mode").and_then(Value::as_str)),
            parent_native_id: Self::lineage(payload).map(NativeId::new),
            lineage_kind: Self::lineage_kind(payload),
            agent_path: Self::agent_path(payload),
            // A delegate's own path is available on its nested thread-spawn metadata. Root
            // rollouts omit it; activity records remain authoritative when this is absent.
            // Deliberately absent here: the payload's own `timestamp` is when the *thread* was
            // created, which on a resumed thread is up to 38 hours before this file's first
            // record. [`header_of`] fills this from the outer stamp.
            started_at_ms: None,
        }
    }

    fn agent_path(payload: &Value) -> Option<String> {
        string_at(payload, "agent_path").or_else(|| {
            payload
                .get("source")
                .and_then(|source| source.get("subagent"))
                .and_then(|subagent| subagent.get("thread_spawn"))
                .and_then(|spawn| string_at(spawn, "agent_path"))
        })
    }

    fn lineage_kind(payload: &Value) -> Option<LineageKind> {
        if payload
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .is_some()
            || payload
                .get("source")
                .and_then(|source| source.get("subagent"))
                .and_then(|subagent| subagent.get("thread_spawn"))
                .and_then(|spawn| spawn.get("parent_thread_id"))
                .and_then(Value::as_str)
                .is_some()
        {
            return Some(LineageKind::Delegation);
        }
        string_at(payload, "forked_from_id").map(|_| LineageKind::Fork)
    }

    /// The thread this one came from, from the three fields that record it.
    ///
    /// # Three spellings for one relationship, and they agree
    ///
    /// *Measured over the whole store:* `parent_thread_id` on 27 files, `forked_from_id` on 76, and
    /// `source.subagent.thread_spawn.parent_thread_id` on 202 — and **they never disagree**
    /// where two are present, which is what makes a fallback chain sound here rather than the
    /// dual-spelling tolerance `AGENTS.md` § Compatibility forbids. That rule is about
    /// absorbing a *rename*; these are three fields the vendor writes concurrently, and
    /// `source.subagent` is the one with the widest coverage, so reading only the top-level
    /// pair would miss 200 of 211 real delegates.
    ///
    /// The result covers **209 of the 214** threads that are delegates by any signal. The
    /// remaining 5 record `thread_source: "subagent"` and no pointer at all, and they stay
    /// absent rather than being guessed — a fabricated parent puts a session under the wrong
    /// row in every listing that shows lineage.
    ///
    /// `session_id` is deliberately **not** in this chain even though 25 files have it naming
    /// the parent, because on the other 127 it names the file's own thread. A field that means
    /// two different things by position is not a lineage pointer.
    fn lineage(payload: &Value) -> Option<String> {
        if let Some(parent) = string_at(payload, "parent_thread_id") {
            return Some(parent);
        }
        let spawn = payload
            .get("source")
            .and_then(|source| source.get("subagent"))
            .and_then(|subagent| subagent.get("thread_spawn"));
        if let Some(parent) = spawn.and_then(|spawn| string_at(spawn, "parent_thread_id")) {
            return Some(parent);
        }
        // A fork is a different relationship from a delegation and this collapses them,
        // deliberately: `NativeSummary::parent_native_id` is documented as "delegated from or
        // forked out of", one field for both. Measured, 65 of 76 files carrying
        // `forked_from_id` also record `thread_source: "subagent"`, so on this vendor a
        // collaboration delegate *is* implemented as a fork of its parent — separating them
        // here would report the same relationship two ways.
        string_at(payload, "forked_from_id")
    }
}

/// An `event_msg` payload, projected.
///
/// # The era decides which subtypes carry content, and reading both double-counts
///
/// This is the branch [`Era`] exists for. `agent_message` and `item_completed`/`AgentMessage`
/// are the *same* assistant text in two vocabularies, and the store is measurably exclusive
/// per file: of 1185 legacy files, 1132 carry `agent_message` and **zero** carry an
/// `AgentMessage` item; of 73 paginated files, 67 carry the item and **zero** carry
/// `agent_message`. So a reader that handles both unconditionally emits every assistant
/// message twice on any file that ever holds both — and the 53 legacy plus 6 paginated files
/// carrying neither are simply sessions whose agent never spoke.
///
/// The era is therefore checked on the *content* subtypes and deliberately **not** on the
/// structural ones: `task_started`, `task_complete`, `turn_aborted`, `token_count`, and
/// `error` occur in both eras with the same shape, so gating them on the era would be a
/// branch with no measured difference behind it.
fn event_msg(
    payload: &Value,
    era: Era,
    at_ms: i64,
    opened: &mut std::collections::HashSet<String>,
    parsed: &mut ParsedRollout,
) {
    let subtype = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match subtype {
        // ---- Legacy content vocabulary ----
        "agent_message" if era == Era::Legacy => {
            let Some(text) = string_at(payload, "message") else {
                return;
            };
            let mut event = Event::new(EventKind::Assistant, Some(Phase::Completed), at_ms);
            event.text = text;
            // The vendor's own `phase` — `commentary` for a progress note, `final_answer` for
            // the answer. Measured: 22203 `commentary` against 3842 `final_answer`, so most
            // assistant text in this store is *not* the answer, and a reader that renders
            // them identically shows a dozen equally-weighted paragraphs per turn. Carried in
            // `data` rather than promoted, because the vocabulary has no field for it and
            // inventing one for a single vendor is worse than keeping it queryable.
            if let Some(phase) = string_at(payload, "phase") {
                event.data = Some(json!({"phase": phase}));
            }
            parsed.messages.push(SessionMessage::Event(event));
        }
        "agent_reasoning" if era == Era::Legacy => {
            // **The plaintext reasoning summary, and this event is the primary source rather
            // than the `response_item` beside it.** Measured: 6416 `agent_reasoning` records
            // against 4008 `response_item/reasoning` records carrying a plaintext `summary`,
            // and 2927 of the former have no matching summary at all — so reading only the
            // response item loses 46% of the plaintext reasoning this store actually holds.
            // See [`reasoning_summary`] for the other direction.
            if let Some(text) = string_at(payload, "text") {
                parsed.emitted_reasoning.insert(text.clone());
                parsed
                    .messages
                    .push(SessionMessage::Event(thinking(text, at_ms)));
            }
        }
        // A legacy tool outcome, which is where this era records exit status. **The
        // `response_item` output beside it carries no status field at all** — measured, a
        // legacy `function_call_output` has exactly `call_id`/`output`/`type` — so without
        // these three subtypes every one of the 663 real command failures in this store
        // parses as a success.
        "exec_command_end" | "patch_apply_end" | "mcp_tool_call_end" if era == Era::Legacy => {
            if let Some(event) = legacy_tool_outcome(payload, subtype, at_ms) {
                // Remembered so the `response_item` output for this same `call_id` does not
                // emit a second, statusless result over it — see
                // [`ParsedRollout::emitted_results`], which is where the consequence is
                // recorded. The claim is made here, at the one place that produces such a
                // result, rather than at each consumer: three call sites each remembering is
                // three chances to forget.
                if let Some(call_id) = event.stream_id.clone() {
                    parsed.emitted_results.insert(call_id);
                }
                parsed.messages.push(SessionMessage::Event(event));
            }
        }
        // `web_search_end` (2050) and `view_image_tool_call` (57) report a tool that cannot
        // fail in a way this store records: neither carries a status, an exit code, or an
        // error. Their `response_item` pair already carries the call and its output, so a
        // third row would restate it.
        "web_search_end" | "view_image_tool_call" if era == Era::Legacy => {}
        // The legacy collaboration vocabulary. **Entirely different records from the
        // paginated era's `SubAgentActivity`, and the brief is plaintext here** — see
        // [`legacy_delegation`].
        "collab_agent_spawn_end" | "collab_agent_interaction_end" if era == Era::Legacy => {
            legacy_delegation(payload, subtype, at_ms, opened, parsed);
        }
        "collab_close_end" | "collab_waiting_end" if era == Era::Legacy => {
            legacy_delegation_close(payload, subtype, at_ms, parsed);
        }
        "context_compacted" if era == Era::Legacy => {
            parsed
                .messages
                .push(SessionMessage::Event(compaction(at_ms)));
        }

        // ---- Paginated content vocabulary ----
        "item_completed" if era == Era::Paginated => {
            item_completed(payload, at_ms, opened, parsed);
        }
        // Measured: 22 `item_completed` records occur in *legacy* files, all of them a `Plan`
        // item, which the paginated branch also drops. Named here so the exclusion is a
        // decision rather than a fall-through: a legacy file's assistant text is in
        // `agent_message`, and letting these through the paginated reader would be the
        // double-count this era split exists to prevent.
        "item_completed" => {}

        // ---- Both eras, same shape ----
        "task_started" => {
            // `started_at` is **whole seconds**, not milliseconds: measured against the
            // sibling outer ISO stamp over 3361 records, `started_at * 1000` sits a median of
            // 585 ms *before* it and never after — consistent with a truncated second, and
            // off by a factor of 1000 if read as millis. The outer stamp is used instead,
            // because it is the more precise of the two and the unit question then does not
            // arise. This is the unit half of the vendor-key family
            // (`native::timestamp`): the wrong divisor here would yield 1970 on every row.
            let mut event = Event::new(EventKind::Turn, Some(Phase::Started), at_ms);
            event.stream_id = string_at(payload, "turn_id");
            parsed.messages.push(SessionMessage::Event(event));
        }
        "task_complete" => {
            let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), at_ms);
            event.stream_id = string_at(payload, "turn_id");
            // The vendor's own duration, which it records on 2900 of 4317 closes. Not
            // recomputed from the stamps: the vendor measured the turn and this parser
            // measured record arrivals, and a derived number that disagrees with the
            // vendor's is worse than an absent one.
            event.data = Some(json!({
                "stopReason": "end_turn",
                "durationMs": payload.get("duration_ms").cloned().unwrap_or(Value::Null),
            }));
            // **`error` on a `task_complete` is a failed turn, and only the paginated era has
            // it** — measured, 20 of 191 paginated closes carry one and no legacy close does.
            // Read before the text, because a success and a failure are different shapes and
            // a reader for one swallows the other (`AGENTS.md`).
            if let Some(error) = payload.get("error").filter(|value| !value.is_null()) {
                event.is_error = true;
                if let Some(object) = event.data.as_mut().and_then(Value::as_object_mut) {
                    object.insert("stopReason".to_string(), json!("error"));
                    object.insert("error".to_string(), error.clone());
                }
            }
            parsed.messages.push(SessionMessage::Event(event));
        }
        "turn_aborted" => {
            // **A `status`, not a failed turn.** An interrupted turn is a completed exchange
            // the user chose to end, and flagging it as an error reports a deliberate stop as
            // a broken one (`docs/session-protocol.md` § Outcomes — keep `abandoned` distinct
            // from `failed`). Measured, the only `reason` in the store is `interrupted`.
            let mut event = Event::new(EventKind::Status, None, at_ms);
            event.text = match string_at(payload, "reason").as_deref() {
                Some("interrupted") => "The user interrupted this turn".to_string(),
                Some(other) => format!("This turn ended early: {other}"),
                None => "This turn ended early".to_string(),
            };
            event.stream_id = string_at(payload, "turn_id");
            parsed.messages.push(SessionMessage::Event(event));
        }
        "token_count" => {
            // `info` is **null on 5.6% of these records** and the outer object is otherwise
            // just rate limits, so a usage event built unconditionally would publish an empty
            // one. The rate limits are deliberately dropped: they are an account fact rather
            // than this session's, and journalling them per turn would record a moving global
            // number as though it belonged to the transcript.
            let Some(info) = payload.get("info").filter(|value| !value.is_null()) else {
                return;
            };
            let mut event = Event::new(EventKind::Usage, Some(Phase::Completed), at_ms);
            event.stream_id = Some("codex-usage".to_string());
            event.data = Some(info.clone());
            parsed.messages.push(SessionMessage::Event(event));
        }
        "error" => {
            let mut event = Event::new(EventKind::Status, None, at_ms);
            event.is_error = true;
            event.text = string_at(payload, "message")
                .unwrap_or_else(|| "The agent reported an error".to_string());
            // The vendor's own class (`usage_limit_exceeded`), because "it failed" and "you
            // are out of quota" are different facts and only the second names a cause.
            if let Some(class) = string_at(payload, "codex_error_info") {
                event.data = Some(json!({"error": class}));
            }
            parsed.messages.push(SessionMessage::Event(event));
        }
        "thread_name_updated" | "thread_goal_updated" | "thread_settings_applied" => {
            // Session-level state a listing reads, not transcript events. `thread_goal_updated`
            // fires 1549 times across 15 sessions — one per turn with the same objective — so
            // a row per record would repeat one sentence dozens of times.
        }
        "user_message" | "thread_rolled_back" | "entered_review_mode" | "exited_review_mode" => {
            // `user_message` (4770) duplicates the `response_item/message` the user typed, and
            // the vocabulary has no `user` kind — the server owns a turn's `request_text`
            // (`docs/database-schema.md`), and inventing a kind is what
            // `crates/adapters/src/lib.rs` forbids an adapter from doing.
        }
        _ => parsed.unknown_records += 1,
    }
}

/// A plaintext reasoning summary, as a `thinking` event.
///
/// One constructor so the two sources that can produce one — [`event_msg`]'s
/// `agent_reasoning` and [`reasoning_summary`]'s `response_item` — cannot disagree about the
/// kind or the phase.
fn thinking(text: String, at_ms: i64) -> Event {
    let mut event = Event::new(EventKind::Thinking, Some(Phase::Completed), at_ms);
    event.text = text;
    event
}

/// A compaction boundary.
///
/// Emitted from the *announcement* (`context_compacted` legacy, a `ContextCompaction` item
/// paginated) rather than from the `compacted` record carrying the replacement history,
/// because the two are paired one-to-one on both eras and drawing from both would put two
/// boundaries where context was condensed once.
fn compaction(at_ms: i64) -> Event {
    let mut event = Event::new(EventKind::Compact, Some(Phase::Completed), at_ms);
    event.text = "Context was compacted".to_string();
    event
}

/// A legacy tool outcome, from the three `*_end` subtypes that carry one.
///
/// # This era puts the outcome and the payload in two different records
///
/// A legacy `function_call_output` carries `call_id`, `output`, and nothing else — no status,
/// no exit code, no error flag. So the *failure* of a tool is recorded only here, on an
/// `event_msg`, and it is keyed by the same `call_id`. Measured: 12005 of 12075 of these pair
/// to a call in the same file, and 663 report a failure that the paired output cannot express.
///
/// The three subtypes say failure three different ways, and all three are read: an
/// `exec_command_end` has `exit_code`, a `patch_apply_end` has `success` plus `status`, and an
/// `mcp_tool_call_end` has neither and reports through `result`. A reader checking only
/// `exit_code` reports every failed patch as applied.
fn legacy_tool_outcome(payload: &Value, subtype: &str, at_ms: i64) -> Option<Event> {
    let call_id = string_at(payload, "call_id")?;
    let mut event = Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
    event.stream_id = Some(call_id);
    event.is_error = match subtype {
        // Zero is success and absent is *not* a failure: a missing key would otherwise report
        // every result as failed the moment the vendor renamed it.
        "exec_command_end" => payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0),
        "patch_apply_end" => payload.get("success").and_then(Value::as_bool) == Some(false),
        // An MCP result carries the JSON-RPC `isError` inside it. Measured, all 349 in this
        // store succeeded, so this branch is written from the shape rather than from an
        // observed failure — which is stated rather than hidden, because a branch that has
        // never fired is indistinguishable from one whose condition is rare (`AGENTS.md`).
        _ => payload
            .get("result")
            .and_then(|result| result.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    // The tool's own output, preferring the aggregated form the vendor assembled. `stdout`
    // alone loses a command's stderr, which is usually the whole content of a failure.
    event.text = string_at(payload, "aggregated_output")
        .or_else(|| string_at(payload, "formatted_output"))
        .or_else(|| string_at(payload, "stdout"))
        .unwrap_or_default();
    let mut detail = json!({});
    if let Some(object) = detail.as_object_mut() {
        for field in ["exit_code", "duration", "status", "success"] {
            if let Some(value) = payload.get(field).filter(|value| !value.is_null()) {
                object.insert(field.to_string(), value.clone());
            }
        }
    }
    event.data = Some(detail);
    Some(event)
}

/// A legacy collaboration spawn or follow-up, as a delegated trajectory.
///
/// # The plaintext brief lives here, and [`BRIEF_IS_CIPHERTEXT`] is about the other era
///
/// This is the measurement that turned out narrower than recorded. The brief is ciphertext
/// from 0.147 — true, and 0.147 is the *paginated* era. In the legacy era every one of the
/// 91 `collab_agent_spawn_end` records carries a **plaintext** `prompt`, and 86 of them also
/// carry a `new_agent_nickname` the vendor assigned (`Plato`, `Poincare`). So a legacy
/// delegation can be named honestly, and it is: the nickname when there is one, because it is
/// what the vendor calls this delegate, and never a placeholder when there is not
/// (`docs/session-protocol.md` § `name` is absent when unknown).
///
/// The trajectory id is the **delegate's thread id**, not the record's `call_id`. Measured,
/// all 91 spawn records' `call_id` names a `spawn_agent` call in the same file, and 12
/// `collab_agent_interaction_end` records name a `send_input` — so the call is the *host* the
/// trajectory nests inside, and keying by it would collapse every delegate one call touched
/// onto a single node.
fn legacy_delegation(
    payload: &Value,
    subtype: &str,
    at_ms: i64,
    opened: &mut std::collections::HashSet<String>,
    parsed: &mut ParsedRollout,
) {
    // `new_thread_id` on a spawn, `receiver_thread_id` on a follow-up. Two spellings for the
    // delegate because the vendor writes two; neither is optional, and 5 spawn records carry
    // neither a nickname nor a resolvable thread — those simply produce nothing rather than a
    // node with no identity.
    let Some(thread) =
        string_at(payload, "new_thread_id").or_else(|| string_at(payload, "receiver_thread_id"))
    else {
        return;
    };

    // `started` on first sighting only. A second `started` under one id opens a duplicate
    // node, and these records arrive repeatedly for one delegate — 110 `collab_waiting_end`
    // and 76 `collab_close_end` against 91 spawns.
    let phase = if opened.insert(thread.clone()) {
        Phase::Started
    } else {
        Phase::Delta
    };
    let mut event = Event::new(EventKind::Subagent, Some(phase), at_ms);
    event.stream_id = Some(thread);
    // The delegating call, which promotes it in place (`docs/session-protocol.md`
    // § Promotion).
    event.span = string_at(payload, "call_id");
    event.name = string_at(payload, "new_agent_nickname")
        .or_else(|| string_at(payload, "receiver_agent_nickname"));
    // The graph reader reports interior availability after reading the child's sources.
    // The brief. Plaintext in this era, and it is the delegate's whole assignment — a card
    // with a name and no brief says who ran and not what they were asked.
    event.text = string_at(payload, "prompt").unwrap_or_default();
    let mut detail = json!({"record": subtype});
    if let Some(object) = detail.as_object_mut() {
        for (field, key) in [("model", "model"), ("reasoning_effort", "reasoningEffort")] {
            if let Some(value) = string_at(payload, field) {
                object.insert(key.to_string(), json!(value));
            }
        }
        if let Some(role) = string_at(payload, "new_agent_role")
            .or_else(|| string_at(payload, "receiver_agent_role"))
        {
            object.insert("agentRole".to_string(), json!(role));
        }
    }
    event.data = Some(detail);
    parsed.messages.push(SessionMessage::Event(event));
}

/// A legacy delegation's close, from `collab_close_end` or `collab_waiting_end`.
///
/// # This era *does* record an outcome, and the paginated era does not
///
/// The recorded measurement — "a codex delegation has no close record" — is right about
/// `SubAgentActivity` and wrong about the store. Measured: 75 `collab_close_end` records carry
/// `status: {"completed": null}`, and 110 `collab_waiting_end` records carry a per-thread
/// `statuses` map whose value is either `{"completed": null}` or
/// `{"errored": "<vendor message>"}`. That map is the only place a delegate's *failure* is
/// recorded anywhere in either era.
///
/// **The status is an externally-tagged enum, so the outcome is the key rather than a value.**
/// A reader looking for a `status` string finds an object and reports nothing; a reader taking
/// the object's truthiness reports every failure as a success, because `{"errored": …}` is as
/// present as `{"completed": null}`. Both are silent.
fn legacy_delegation_close(payload: &Value, subtype: &str, at_ms: i64, parsed: &mut ParsedRollout) {
    // A close names one thread; a wait names N. `statuses` is keyed **by thread id**, which is
    // what makes a wait on three delegates close three trajectories rather than one — the
    // multiplexing rule `translate::TrajectoryId` exists for, in this vendor's spelling.
    let mut outcomes: Vec<(String, &Value)> = Vec::new();
    if let Some(map) = payload.get("statuses").and_then(Value::as_object) {
        for (thread, status) in map {
            outcomes.push((thread.clone(), status));
        }
    } else if let Some(thread) = string_at(payload, "receiver_thread_id") {
        if let Some(status) = payload.get("status") {
            outcomes.push((thread, status));
        }
    }

    for (thread, status) in outcomes {
        let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), at_ms);
        event.stream_id = Some(thread.clone());
        // The enum's tag. `errored` carries the vendor's own sentence as its value, which is
        // what a reader needs to know *what* failed.
        let (failed, message) = match status.as_object().and_then(|object| object.iter().next()) {
            Some((tag, value)) if tag == "errored" => (true, value.as_str().map(str::to_string)),
            Some((tag, _)) if tag == "completed" => (false, None),
            // A tag this build does not know. **Not treated as a failure**: an unrecognized
            // outcome is not evidence of one, and colouring it red would report an unknown
            // state as a broken delegation.
            other => (false, other.map(|(tag, _)| tag.clone())),
        };
        if failed {
            parsed.failed_delegations.insert(thread.clone());
        }
        // A failure already reported for this thread survives a later `completed` close. See
        // [`ParsedRollout::failed_delegations`] — this is the ordering that makes the flag
        // necessary rather than defensive.
        event.is_error = failed || parsed.failed_delegations.contains(&thread);
        if let Some(text) = message {
            event.text = text;
        }
        event.data = Some(json!({"record": subtype}));
        parsed.messages.push(SessionMessage::Event(event));
    }
}

/// One `item_completed` item, in the paginated vocabulary.
///
/// # The item stream and the `response_item` stream are two id spaces, not one
///
/// Measured over all 73 paginated files: an item's `id` overlaps the file's `call_id` set for
/// **`SubAgentActivity` and `CollabAgentToolCall` only** — every `CommandExecution`,
/// `FileChange`, `McpToolCall`, `Reasoning`, `AgentMessage`, `UserMessage`, `ImageView`, and
/// `ContextCompaction` id is disjoint from it (`exec-<uuid>`, `msg_…`, `rs_…` against
/// `call_…`). So an item cannot be keyed to a tool call by its own id, and the two collaboration
/// item types are the exception because their id genuinely *is* the issuing call's.
///
/// That is why a tool's outcome here rides on [`response_item`]'s output rather than on the
/// item: the pairing an item would need does not exist in the data. What the item is
/// nevertheless the only carrier of is the **exit status** — 360 of 3805 `CommandExecution`
/// items report `failed`, against a `custom_tool_call_output` whose text merely opens with
/// `Script failed` on 150 of them. Both facts are used, at their own records.
fn item_completed(
    payload: &Value,
    at_ms: i64,
    opened: &mut std::collections::HashSet<String>,
    parsed: &mut ParsedRollout,
) {
    let Some(item) = payload.get("item") else {
        parsed.unknown_records += 1;
        return;
    };
    // **The item's own stamps, which are epoch milliseconds** — verified against the sibling
    // outer ISO stamp over all 11231 items carrying them: `completed_at_ms` minus the outer
    // stamp has a median of 0 ms and a range of −25..0, so the units agree to the millisecond.
    // That is the comparison `AGENTS.md` requires in place of eyeballing a magnitude, and it is
    // what rules out the microsecond reading that decoded a sibling vendor's store to 1970.
    let completed_ms = payload
        .get("completed_at_ms")
        .and_then(Value::as_i64)
        .unwrap_or(at_ms);
    let started_ms = payload.get("started_at_ms").and_then(Value::as_i64);

    match item.get("type").and_then(Value::as_str).unwrap_or_default() {
        "AgentMessage" => {
            let text = flatten_content(item.get("content"));
            if text.is_empty() {
                return;
            }
            let mut event = Event::new(EventKind::Assistant, Some(Phase::Completed), completed_ms);
            event.stream_id = string_at(item, "id");
            event.text = text;
            // **`content[].type` is `Text` here and `output_text` on the sibling
            // `response_item`** — the same block, re-cased by the projection layer this era
            // added. Read by presence of `text` rather than by matching the tag, so a third
            // casing does not silently empty every message.
            if let Some(phase) = string_at(item, "phase") {
                event.data = Some(json!({"phase": phase}));
            }
            event.started_at_ms = started_ms;
            parsed.messages.push(SessionMessage::Event(event));
        }
        "Reasoning" => {
            // **Omitted, always, and that is the measurement rather than an omission.** All
            // 4895 `Reasoning` items in the store carry `summary_text: []` and
            // `raw_content: []` — the fields exist and are empty on every single one, while
            // the paired `response_item/reasoning` carries `encrypted_content` and, on 0 of
            // 4905, a plaintext summary. So this era's reasoning is *entirely* ciphertext, and
            // emitting an event would put a blank `thinking` block in every transcript, which
            // reads as a rendering bug rather than as an absent record
            // ([`REASONING_IS_MOSTLY_ENCRYPTED`]).
        }
        "CommandExecution" | "FileChange" | "McpToolCall" | "ImageView" => {
            if let Some(event) = paginated_tool_item(item, started_ms, completed_ms) {
                parsed.messages.push(SessionMessage::Event(event));
            }
        }
        "SubAgentActivity" => {
            paginated_delegation(payload, item, started_ms, completed_ms, opened, parsed);
        }
        "CollabAgentToolCall" => {
            // **A barrier, not lineage.** Both real instances carry `receiver_thread_ids: []`
            // and `agents_states: {}` with `tool: "wait"`, and the `status: "completed"` is
            // the *wait's* — its paired output reads
            // `{"message":"Wait timed out.","timed_out":true}`. Its `id` does name a
            // `wait_agent` call, which is exactly what makes treating it as a delegation
            // tempting and wrong: it would open a trajectory for the *waiting*, with no
            // delegate to put inside it.
        }
        "ContextCompaction" => {
            parsed
                .messages
                .push(SessionMessage::Event(compaction(completed_ms)));
        }
        "UserMessage" => {
            // The vocabulary has no `user` kind and the server owns a turn's `request_text`.
        }
        "Plan" => {
            // `docs/session-protocol.md` § Events reserves a `plan` kind and **no adapter
            // emits it and no timeline branch handles it** (`sylph_protocol::EventKind`), so
            // these 22 items would land as `Other("plan")` — a kind nothing renders. They
            // arrive when the producer and the renderer do, which is honest about what
            // happened rather than shipping a row that draws as nothing.
        }
        _ => parsed.unknown_records += 1,
    }
}

/// A paginated tool item, which carries the outcome the output record cannot.
fn paginated_tool_item(item: &Value, started_ms: Option<i64>, at_ms: i64) -> Option<Event> {
    let id = string_at(item, "id")?;
    let mut event = Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
    // The item's own id, which is **not** a `call_id` — see [`item_completed`] for the
    // measurement. So this result stands on its own rather than pairing to a call, which is
    // the honest projection: inventing a pairing the store does not record would attach a
    // command's outcome to whichever call happened to be nearby.
    event.stream_id = Some(id);
    event.name = Some(
        match item.get("type").and_then(Value::as_str) {
            Some("FileChange") => "apply_patch",
            Some("McpToolCall") => "mcp",
            Some("ImageView") => "view_image",
            _ => "exec",
        }
        .to_string(),
    );
    // **`status` is the discriminant and `exit_code` is not enough on its own.** Measured:
    // 360 `CommandExecution` items report `failed`, and every one of them also has a nonzero
    // exit code — but a `FileChange` and an `McpToolCall` carry no exit code at all, and 12 of
    // the 18 `McpToolCall` items are `failed`. Reading only the exit code reports all 12 as
    // successes.
    event.is_error = matches!(string_at(item, "status").as_deref(), Some("failed"));
    event.text = string_at(item, "aggregated_output")
        .or_else(|| string_at(item, "formatted_output"))
        .or_else(|| string_at(item, "stdout"))
        .unwrap_or_default();
    let mut detail = json!({});
    if let Some(object) = detail.as_object_mut() {
        for field in [
            "command",
            "cwd",
            "exit_code",
            "duration",
            "status",
            "changes",
        ] {
            if let Some(value) = item.get(field).filter(|value| !value.is_null()) {
                object.insert(field.to_string(), value.clone());
            }
        }
    }
    event.data = Some(detail);
    event.started_at_ms = started_ms;
    Some(event)
}

/// A `SubAgentActivity` item, whose edge direction is decided by [`delegation_edge`].
///
/// Only a [`Edge::Child`] opens anything. A [`Edge::Parent`] record — 31 of the 42 in the
/// store, every one of them in a *delegate's* rollout — is the child's view of the thread
/// above it, and mapping it to a `span` would name the parent as a trajectory inside the
/// child. That inversion is undetectable downstream: a reversed pointer is still a
/// well-formed pointer, and the tree it builds is wrong rather than broken.
fn paginated_delegation(
    payload: &Value,
    item: &Value,
    started_ms: Option<i64>,
    at_ms: i64,
    opened: &mut std::collections::HashSet<String>,
    parsed: &mut ParsedRollout,
) {
    // The reader's own thread, which is what makes the direction decidable. Passed through
    // rather than re-read inside `delegation_edge`, so the decision function stays pure and
    // assertable from a fixture.
    let own = payload
        .get("thread_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let own_path = parsed
        .header
        .as_ref()
        .and_then(|header| header.agent_path.as_deref());
    let Some(Edge::Child { thread, host, name }) = delegation_edge_with_path(item, own, own_path)
    else {
        return;
    };

    let phase = if opened.insert(thread.clone()) {
        Phase::Started
    } else {
        // Measured: 32 `send_message` records name the same handful of delegates, so a
        // second `started` under one id is the common case rather than the rare one.
        Phase::Delta
    };
    let mut event = Event::new(EventKind::Subagent, Some(phase), at_ms);
    event.stream_id = Some(thread);
    event.span = host;
    // **The name comes from `agent_path`, never from the brief.** The brief is ciphertext in
    // this era ([`BRIEF_IS_CIPHERTEXT`]) and a placeholder written in its place is permanent
    // and afterwards indistinguishable from a real name.
    event.name = name;
    // A complete parent declaration does not establish whether the child is readable.
    // **No text, deliberately.** The delegate's assignment is the `spawn_agent` call's
    // `message`, which is a Fernet token in this era — putting it on the card would render
    // 3 KB of base64 as a delegation's description. `text` is empty rather than holding a
    // notice, because the empty string is what a consolidating fold treats as "this phase
    // said nothing".
    event.data = Some(json!({
        "kind": item.get("kind").cloned().unwrap_or(Value::Null),
        "agentPath": item.get("agent_path").cloned().unwrap_or(Value::Null),
    }));
    event.started_at_ms = started_ms;
    parsed.messages.push(SessionMessage::Event(event));
}

/// A `response_item` payload, projected.
///
/// # This stream is the model's own transcript, and it overlaps the era stream
///
/// A `response_item` is what codex sends to the model, so the same assistant text appears here
/// *and* in whichever era stream the file speaks. Measured: 22236 legacy `message`/assistant
/// records against 26326 `agent_message` events, with the era stream a strict superset on
/// 1107 of 1185 files; and 1179 paginated assistant records against 1162 `AgentMessage` items,
/// identical on 64 of 73.
///
/// **So the era stream is the authority for assistant text and this one contributes none.**
/// That decision is what the mutual-exclusion measurement is *for*: reading assistant text
/// from both would double every message in the store, not just in the overlapping era. The
/// 4124 legacy messages present only in the era stream are mostly `commentary` (4122 of them),
/// which is the vendor filtering progress notes out of what it sends the model — so the era
/// stream is also the more complete one, which settles which of the two to keep.
///
/// What this stream *is* the only carrier of, and therefore what is read here:
///
/// - **Tool calls.** Neither era's `event_msg` records a call's arguments; both record only
///   its outcome. 55740 legacy plus 5704 paginated calls live here and nowhere else.
/// - **A tool's output.** The paginated era has no `*_end` event at all — measured, zero — so
///   the 5704 outputs here are that era's only tool text.
/// - **Reasoning that the legacy era's `agent_reasoning` missed**, which is 519 summaries.
fn response_item(
    payload: &Value,
    era: Era,
    at_ms: i64,
    call_names: &mut std::collections::HashMap<String, String>,
    parsed: &mut ParsedRollout,
) {
    match payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "function_call" | "custom_tool_call" => {
            let Some(call_id) = string_at(payload, "call_id") else {
                return;
            };
            let name = string_at(payload, "name");
            if let Some(name) = &name {
                call_names.insert(call_id.clone(), name.clone());
            }
            // **`completed` means its *arguments* finished streaming**, never that the tool
            // finished (`sylph_protocol::Phase::Completed`). It is the only phase this store
            // can support: a record exists only once the call was whole, so a `started` phase
            // would be an ephemeral projection the journal never keeps and the call itself
            // would vanish on reload.
            let mut event = Event::new(EventKind::ToolCall, Some(Phase::Completed), at_ms);
            event.stream_id = Some(call_id);
            event.name = name;
            // **`arguments` on a `function_call`, `input` on a `custom_tool_call`, and the two
            // are not interchangeable.** Measured, a `function_call` never carries `input` and
            // a `custom_tool_call` never carries `arguments`; reading one key would empty
            // 5489 of the paginated era's 5704 calls, which is 96% of them.
            let raw = string_at(payload, "arguments").or_else(|| string_at(payload, "input"));
            if let Some(raw) = raw {
                // A `function_call`'s arguments are a JSON *string*, so they are parsed back
                // into an object for `data.input`; a `custom_tool_call`'s `input` is
                // JavaScript source and is not JSON at all, which is why a parse failure here
                // keeps the raw text rather than dropping the call.
                event.data = match serde_json::from_str::<Value>(&raw) {
                    Ok(parsed_input) => Some(json!({"input": parsed_input})),
                    Err(_) => Some(json!({"input": raw})),
                };
                event.text = raw;
            }
            parsed.messages.push(SessionMessage::Event(event));
        }
        "function_call_output" | "custom_tool_call_output" => {
            let Some(call_id) = string_at(payload, "call_id") else {
                return;
            };
            // **The legacy era already emitted this result from its `*_end` event, which is
            // the only record carrying the exit status.** Emitting again here would produce
            // two `tool_result` rows under one `stream_id` — which a consolidating fold keeps
            // as one block, so the visible symptom would be the *outcome* silently reverting
            // to the statusless copy rather than a duplicate row. Measured, 10139 of the
            // 55737 legacy outputs are preceded by such an event; the other 45599 have none,
            // and those are read here.
            if era == Era::Legacy && parsed.emitted_results.contains(&call_id) {
                return;
            }
            let mut event = Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
            event.stream_id = Some(call_id.clone());
            event.name = call_names.get(&call_id).cloned();
            // **`output` is a string on the legacy era and an array of blocks on the
            // paginated one** — measured, 51764 strings against 5364 arrays — so both shapes
            // go through one flattener. A reader for one silently yields `""` for the other,
            // which is an empty tool result rather than an error.
            event.text = match payload.get("output") {
                Some(Value::String(text)) => text.clone(),
                other => flatten_content(other),
            };
            // **No `is_error` here, in either era.** Measured: an output record carries
            // `call_id`, `output`, `id`, and a passthrough object — no status, no flag, no
            // exit code. The paginated era's text opens with `Script failed` on 150 records,
            // and matching that sentence would be a prose test against a string the vendor
            // may reword; the item's own `status` is the structural fact and it is read there
            // ([`paginated_tool_item`]). So a result from this record reports no outcome
            // rather than a guessed one.
            parsed.messages.push(SessionMessage::Event(event));
        }
        "reasoning" => {
            let Some(event) = reasoning_summary(payload, at_ms) else {
                return;
            };
            // The dedup, and it applies to the legacy era only because the paginated era's
            // `agent_reasoning` count is zero — see [`ParsedRollout::emitted_reasoning`] for
            // why both streams are read at all rather than one being preferred.
            if era == Era::Legacy && !parsed.emitted_reasoning.insert(event.text.clone()) {
                return;
            }
            parsed.messages.push(SessionMessage::Event(event));
        }
        // The model-facing copy of the conversation, including the injected `developer` and
        // `user` envelopes. Not projected: assistant text comes from the era stream (see this
        // function's own note), and **the first `user` record is usually not what the user
        // typed** — measured here as `<environment_context>`, `# AGENTS.md instructions`, and
        // a 2.6 KB collaboration preamble — which is the rule `NativeSummary::title` assigns
        // to the server precisely so two implementations cannot disagree about it.
        "message" => {}
        // The paginated era's inter-agent message copy, 49 records with an `author`/`recipient`
        // pair. The delegation itself is projected from `SubAgentActivity`, and its payload is
        // the same ciphertext the brief is.
        "agent_message" => {}
        // `web_search_call` (2495) and `tool_search_call`/`tool_search_output` (39) are calls
        // whose own `function_call` record is absent — they are the vendor's built-in search,
        // and they carry no `call_id` to pair anything to.
        "web_search_call" | "tool_search_call" | "tool_search_output" => {}
        _ => parsed.unknown_records += 1,
    }
}

/// The plaintext part of a `response_item/reasoning`, when there is one.
///
/// # ~92% of reasoning in this store is unrecoverable, and the honest projection is silence
///
/// *Re-measured over the whole store:* every reasoning record carries
/// `encrypted_content`, and only **4008** also carrying a plaintext `summary` — 12.1%, and all
/// 4008 in the legacy era. The paginated era's 4905 records carry a summary **zero** times, and
/// its `Reasoning` items carry `summary_text: []` and `raw_content: []` on all 4895.
///
/// So this returns `None` far more often than it returns an event, and that is correct rather
/// than a gap: an empty `thinking` event renders as a blank block, which reads as a rendering
/// bug rather than as an absent record, and a journal makes it permanent
/// ([`REASONING_IS_MOSTLY_ENCRYPTED`]).
///
/// The ciphertext is deliberately **not** carried in `data` either. It is unreadable by
/// construction, it is 2–4 KB per record, and 33211 of them would make the journal mostly
/// undecryptable base64 — storing it would be paying for the whole cost of reasoning while
/// getting none of it.
fn reasoning_summary(payload: &Value, at_ms: i64) -> Option<Event> {
    // `summary` is an array of `{type: "summary_text", text}`. The `content` sibling is null on
    // all 33211 records, which is why it is not consulted.
    let text = payload
        .get("summary")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return None;
    }
    let mut event = thinking(text, at_ms);
    event.stream_id = string_at(payload, "id");
    Some(event)
}

/// The text of a content array, whatever the era spells the blocks.
///
/// **Read by the presence of `text` rather than by matching the block's `type`.** The same
/// block is tagged `input_text` in a `message`, `output_text` in an assistant `message`, `Text`
/// in an `AgentMessage` item, and `text` in a `custom_tool_call_output` — four casings for one
/// shape, measured, and a reader matching the tag empties three of the four. A bare string is
/// also accepted, because a legacy `function_call_output`'s `output` is one.
fn flatten_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                _ => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Stateful projector shared by adoption and follow.
#[derive(Default)]
struct RolloutProjector {
    era: Option<Era>,
    opened: HashSet<String>,
    call_names: HashMap<String, String>,
    parsed: ParsedRollout,
}

impl RolloutProjector {
    fn apply(&mut self, record: &Value) -> Vec<SessionMessage> {
        let owner = self
            .parsed
            .header
            .as_ref()
            .and_then(|header| header.native_id.as_ref())
            .map(NativeId::as_str)
            .map(str::to_string);
        let record_thread = record
            .get("payload")
            .and_then(|payload| payload.get("thread_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        project_record(
            record,
            &mut self.era,
            &mut self.opened,
            &mut self.call_names,
            &mut self.parsed,
        );
        let mut messages = std::mem::take(&mut self.parsed.messages);
        // Codex can forward a child's item records into its parent's rollout. Preserve that
        // source-session provenance so a graph reader can deduplicate the forwarded copy when
        // the child's own rollout is discovered later.
        if let (Some(owner), Some(record_thread)) = (owner, record_thread) {
            if owner != record_thread {
                for message in &mut messages {
                    if let SessionMessage::Event(event) = message {
                        if event.span.is_none() {
                            event.span = Some(record_thread.clone());
                        }
                    }
                }
            }
        }
        messages
    }
}

/// Re-project one retained Codex envelope without opening its original rollout file.
/// Callers that need cross-record correlation should retain and reuse the projector.
pub(crate) fn reproject_native_record(
    projector: &mut RolloutProjector,
    record: &NativeRecord,
) -> Vec<SessionMessage> {
    projector.apply(&record.payload)
}

/// A stable identity for one filesystem incarnation of a rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn file_identity(metadata: &std::fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    #[cfg(not(unix))]
    {
        // Windows has no portable inode pair. A size/mtime token still catches the rewrite
        // patterns this adapter can observe and keeps the type available on all targets.
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_nanos() as u64)
            .unwrap_or(0);
        FileIdentity {
            device: modified,
            inode: metadata.len(),
        }
    }
}

#[derive(Clone)]
struct HeaderCacheEntry {
    stamp: (FileIdentity, u64, u128),
    header: Header,
}

fn file_stamp(path: &Path) -> Option<(FileIdentity, u64, u128)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| time.as_nanos())
        .unwrap_or(0);
    Some((file_identity(&metadata), metadata.len(), modified))
}

fn bytes_fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// One append-only rollout cursor. `generation` advances whenever the file incarnation or
/// immutable prefix changes; offsets are never committed past a complete, parseable line.
struct RolloutCursor {
    path: PathBuf,
    identity: Option<FileIdentity>,
    prefix_fingerprint: Option<u64>,
    prefix_len: u64,
    generation: u64,
    offset: u64,
    records: u64,
    projector: RolloutProjector,
    /// Complete-line fingerprints from the prior generation, keyed by byte offset. They let a
    /// truncate-and-rewrite replay its unchanged prefix for state reconstruction without
    /// publishing duplicate normalized events.
    previous_lines: HashMap<u64, u64>,
    line_fingerprints: HashMap<u64, u64>,
    last_line: Option<(u64, usize, u64)>,
}

impl RolloutCursor {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            identity: None,
            prefix_fingerprint: None,
            prefix_len: 0,
            generation: 0,
            offset: 0,
            records: 0,
            projector: RolloutProjector::default(),
            previous_lines: HashMap::new(),
            line_fingerprints: HashMap::new(),
            last_line: None,
        }
    }

    #[allow(dead_code)]
    fn generation(&self) -> u64 {
        self.generation
    }

    fn reset_generation(&mut self, identity: FileIdentity, prefix: Option<u64>, prefix_len: u64) {
        self.generation = self.generation.saturating_add(1);
        self.offset = 0;
        self.records = 0;
        self.projector = RolloutProjector::default();
        self.previous_lines = std::mem::take(&mut self.line_fingerprints);
        self.last_line = None;
        self.identity = Some(identity);
        self.prefix_fingerprint = prefix;
        self.prefix_len = prefix_len;
    }

    fn poll(&mut self) -> Result<Vec<SessionMessage>> {
        let Some(metadata) = std::fs::metadata(&self.path).ok() else {
            return Ok(Vec::new());
        };
        let identity = file_identity(&metadata);
        // Hash only a prefix that was already present on the previous poll. Hashing the whole
        // file when it is shorter than 4 KiB would make every ordinary append look like a
        // replacement and reset a healthy cursor on each item flush.
        let prefix_len = if self.prefix_len == 0 {
            metadata.len().min(4096)
        } else {
            self.prefix_len.min(metadata.len())
        };
        let prefix = read_span(&self.path, 0, prefix_len).map(|bytes| bytes_fingerprint(&bytes));

        let last_line_changed = self
            .last_line
            .is_some_and(|(line_offset, length, previous)| {
                read_span(&self.path, line_offset, length as u64)
                    .map(|bytes| bytes_fingerprint(&bytes) != previous)
                    .unwrap_or(true)
            });
        let changed = self.identity.is_some()
            && (self.identity != Some(identity)
                || metadata.len() < self.offset
                || (self.offset > 0 && self.prefix_fingerprint != prefix)
                || last_line_changed);
        if changed {
            self.reset_generation(identity, prefix, prefix_len);
        } else {
            self.identity = Some(identity);
            self.prefix_fingerprint = prefix;
            self.prefix_len = prefix_len;
        }

        let Some((records, next_offset)) = read_complete_records(&self.path, self.offset)? else {
            return Ok(Vec::new());
        };
        let mut messages = Vec::new();
        for record in records {
            let fingerprint = bytes_fingerprint(&record.raw);
            let replayed = self
                .previous_lines
                .get(&record.offset)
                .is_some_and(|previous| *previous == fingerprint);
            let projected = self.projector.apply(&record.value);
            if !replayed {
                // Retain the complete vendor envelope before its normalized projection. The
                // server can then re-project Codex history after the event vocabulary evolves,
                // with lexical bytes preserved for JSONL round-trips.
                let version = self.records + 1;
                let raw_json = record
                    .raw
                    .strip_suffix(&[b'\n'][..])
                    .unwrap_or(&record.raw)
                    .to_vec();
                messages.push(SessionMessage::NativeRecord(native_record(
                    &self.path,
                    version,
                    &record.value,
                    raw_json,
                )));
                messages.extend(projected);
            }
            self.line_fingerprints.insert(record.offset, fingerprint);
            self.last_line = Some((record.offset, record.raw.len(), fingerprint));
            self.records = self.records.saturating_add(1);
        }
        self.offset = next_offset;
        // The prior-generation map is needed only while the rewritten prefix is consumed. Once
        // this pass reaches the current end, future appends belong solely to this generation.
        self.previous_lines.clear();
        Ok(messages)
    }
}

struct CompleteRecord {
    offset: u64,
    raw: Vec<u8>,
    value: Value,
}

/// Read complete JSONL records after `offset`.
///
/// A final unterminated line is intentionally left unread. A complete malformed line is an
/// error and leaves the cursor untouched, so a producer that was mid-write can repair it and be
/// retried without silently losing a record.
fn read_complete_records(path: &Path, offset: u64) -> Result<Option<(Vec<CompleteRecord>, u64)>> {
    let Some(metadata) = std::fs::metadata(path).ok() else {
        return Ok(None);
    };
    if metadata.len() < offset {
        return Ok(Some((Vec::new(), offset)));
    }
    if metadata.len() == offset {
        return Ok(Some((Vec::new(), offset)));
    }
    let bytes = read_span(path, offset, metadata.len() - offset)
        .ok_or_else(|| anyhow!("reading codex rollout {}", path.display()))?;
    let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(Some((Vec::new(), offset)));
    };
    let complete_len = last_newline + 1;
    let complete = &bytes[..complete_len];
    let mut records = Vec::new();
    let mut line_offset = offset;
    for raw_line in complete.split_inclusive(|byte| *byte == b'\n') {
        let end = line_offset.saturating_add(raw_line.len() as u64);
        if raw_line.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let value = serde_json::from_slice::<Value>(raw_line).map_err(|error| {
                anyhow!(
                    "malformed complete JSON record in {} at byte {}: {}",
                    path.display(),
                    line_offset,
                    error
                )
            })?;
            records.push(CompleteRecord {
                offset: line_offset,
                raw: raw_line.to_vec(),
                value,
            });
        }
        line_offset = end;
    }
    Ok(Some((records, offset + complete_len as u64)))
}

/// One child edge in a watched rollout graph.
#[derive(Debug, Clone)]
struct ChildLink {
    relation: LineageKind,
    host: Option<String>,
    name: Option<String>,
}

fn event_key(event: &Event) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    event.kind.hash(&mut hasher);
    event.phase.hash(&mut hasher);
    event.stream_id.hash(&mut hasher);
    event.span.hash(&mut hasher);
    event.name.hash(&mut hasher);
    event.text.hash(&mut hasher);
    event.is_error.hash(&mut hasher);
    if let Some(data) = &event.data {
        // Data is not itself `Hash`; serialize only for the transient digest and retain the
        // fixed-size key, rather than copying megabyte-scale tool output into a watcher's set.
        if let Ok(bytes) = serde_json::to_vec(data) {
            bytes.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Dynamic Codex rollout graph. The root is fixed by the requested native id; descendants are
/// rediscovered from headers every poll so files created after `watch()` do not require restart.
struct RolloutFollowState {
    root_id: NativeId,
    root_path: PathBuf,
    store_root: PathBuf,
    header_cache: HashMap<PathBuf, HeaderCacheEntry>,
    sources: HashMap<PathBuf, RolloutCursor>,
    links: HashMap<String, ChildLink>,
    declared: HashSet<String>,
    forwarded_events: HashSet<u64>,
    child_events: HashSet<u64>,
    forwarded_sources: HashSet<String>,
    forwarded_closes: HashSet<String>,
    interior_states: HashMap<String, InteriorState>,
    pending: VecDeque<SessionMessage>,
    finalizing: bool,
    stopped: bool,
    retry_after_error: bool,
}

/// Native-only envelopes that the app-server never forwards, using the same graph reader
/// as adoption. Stdout remains the sole producer of normalized live events.
pub(super) struct LiveRecords {
    state: Option<RolloutFollowState>,
}

impl LiveRecords {
    pub(super) fn new() -> Self {
        Self { state: None }
    }

    pub(super) fn poll(&mut self, id: &NativeId) -> Result<Vec<SessionMessage>> {
        if self.state.is_none() {
            let Some(path) = store_root().and_then(|root| {
                candidates(&root, is_rollout).into_iter().find_map(|candidate| {
                    header_of(&candidate).filter(|header| header.native_id.as_ref() == Some(id)).map(|_| candidate.path)
                })
            }) else {
                return Ok(Vec::new());
            };
            self.state = Some(RolloutFollowState::new(id.clone(), path, true));
        }
        let mut messages = self.state.as_mut().expect("native source found").poll()?;
        messages.retain(|message| matches!(message, SessionMessage::NativeRecord(_)));
        Ok(messages)
    }
}

impl RolloutFollowState {
    fn new(root_id: NativeId, root_path: PathBuf, finalizing: bool) -> Self {
        let store_root = store_root().unwrap_or_else(|| {
            root_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });
        Self::new_with_root(root_id, root_path, store_root, finalizing)
    }

    fn new_with_root(
        root_id: NativeId,
        root_path: PathBuf,
        store_root: PathBuf,
        finalizing: bool,
    ) -> Self {
        let mut sources = HashMap::new();
        sources.insert(root_path.clone(), RolloutCursor::new(root_path.clone()));
        Self {
            root_id,
            root_path,
            store_root,
            header_cache: HashMap::new(),
            sources,
            links: HashMap::new(),
            declared: HashSet::new(),
            forwarded_events: HashSet::new(),
            child_events: HashSet::new(),
            forwarded_sources: HashSet::new(),
            forwarded_closes: HashSet::new(),
            interior_states: HashMap::new(),
            pending: VecDeque::new(),
            finalizing,
            stopped: false,
            retry_after_error: false,
        }
    }

    fn poll(&mut self) -> Result<Vec<SessionMessage>> {
        let inventory = self.discover()?;
        if let Some((path, _)) = inventory.iter().find(|(_, header)| {
            header
                .native_id
                .as_ref()
                .is_some_and(|id| id.as_str() == self.root_id.as_str())
        }) {
            if path != &self.root_path {
                if let Some(mut cursor) = self.sources.remove(&self.root_path) {
                    cursor.path = path.clone();
                    self.sources.insert(path.clone(), cursor);
                } else {
                    self.sources
                        .insert(path.clone(), RolloutCursor::new(path.clone()));
                }
                self.root_path = path.clone();
            }
        }
        // A rollout can be atomically moved to a new date directory while retaining its native
        // id. Transfer the existing cursor by header identity so a path move is handled like a
        // generation change instead of replaying the whole child history from byte zero.
        for (path, header) in &inventory {
            if self.sources.contains_key(path) {
                continue;
            }
            let Some(id) = header.native_id.as_ref().map(NativeId::as_str) else {
                continue;
            };
            let old_path = self.sources.iter().find_map(|(old_path, cursor)| {
                (old_path != path
                    && cursor
                        .projector
                        .parsed
                        .header
                        .as_ref()
                        .and_then(|old| old.native_id.as_ref())
                        .map(NativeId::as_str)
                        == Some(id))
                .then(|| old_path.clone())
            });
            if let Some(old_path) = old_path {
                if let Some(mut cursor) = self.sources.remove(&old_path) {
                    cursor.path = path.clone();
                    self.sources.insert(path.clone(), cursor);
                }
            }
        }
        let mut paths: Vec<PathBuf> = inventory.iter().map(|(path, _)| path.clone()).collect();
        // `discover` returns breadth-first order, so every parent is visited before its
        // descendants. Only force the requested root to the front when a stale inventory order
        // comes from a reload.
        if let Some(root_at) = paths.iter().position(|path| path == &self.root_path) {
            let root = paths.remove(root_at);
            paths.insert(0, root);
        }

        let mut messages = Vec::new();
        for path in paths {
            let header = inventory
                .iter()
                .find(|(candidate, _)| candidate == &path)
                .map(|(_, header)| header.clone());
            let Some(header) = header else { continue };
            let child = header
                .native_id
                .as_ref()
                .filter(|id| id.as_str() != self.root_id.as_str())
                .map(|id| id.as_str().to_string());

            // A delegation child can be emitted safely only once its parent-side activity has
            // supplied a host call. Hold its cursor at the boundary until that edge is known;
            // otherwise a later parent record could not repair an already-rooted event.
            if let Some(child_id) = &child {
                if header.lineage_kind == Some(LineageKind::Delegation)
                    && self
                        .links
                        .get(child_id)
                        .is_none_or(|link| link.host.is_none())
                    && !self.finalizing
                {
                    continue;
                }
            }

            let cursor = self
                .sources
                .entry(path.clone())
                .or_insert_with(|| RolloutCursor::new(path.clone()));
            let mut projected = cursor.poll()?;
            self.remember_projected_links(&mut projected);
            self.rebuild_links(&inventory);

            if let Some(child_id) = child.clone() {
                if let Some(link) = self.links.get(&child_id).cloned() {
                    if !self.declared.contains(&child_id) {
                        let mut declaration = Event::new(
                            EventKind::Subagent,
                            Some(Phase::Started),
                            header.started_at_ms.unwrap_or(NO_RECORDED_TIME),
                        );
                        declaration.stream_id = Some(child_id.clone());
                        declaration.span = link.host.clone();
                        declaration.name = link.name.clone();
                        declaration.interior_state = Some(if self.finalizing {
                            InteriorState::Full
                        } else {
                            InteriorState::Partial
                        });
                        declaration.data = Some(json!({
                            "relation": match link.relation {
                                LineageKind::Delegation => "delegation",
                                LineageKind::Fork => "fork",
                            },
                            "parentNativeId": header
                                .parent_native_id
                                .as_ref()
                                .map(NativeId::as_str),
                        }));
                        messages.push(SessionMessage::Event(declaration));
                        self.declared.insert(child_id.clone());
                    }
                    for message in &mut projected {
                        if let SessionMessage::Event(event) = message {
                            // The child parser intentionally does not open a trajectory for its
                            // own parent-side interaction. Attach every ordinary child event to
                            // the parent-declared trajectory when it has no explicit span.
                            if event.kind != EventKind::Subagent && event.span.is_none() {
                                event.span = Some(child_id.clone());
                            }
                        }
                    }
                }
            }
            let source_is_child = child.is_some();
            let mut retained = Vec::with_capacity(projected.len());
            for message in projected {
                let SessionMessage::Event(event) = &message else {
                    retained.push(message);
                    continue;
                };
                let key = event_key(event);
                if source_is_child {
                    if self.forwarded_events.contains(&key) {
                        continue;
                    }
                    self.child_events.insert(key);
                } else if event.span.is_some() {
                    if let Some(span) = &event.span {
                        self.forwarded_sources.insert(span.clone());
                        if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed) {
                            self.forwarded_closes.insert(span.clone());
                        }
                    }
                    if self.child_events.contains(&key) {
                        continue;
                    }
                    self.forwarded_events.insert(key);
                }
                retained.push(message);
            }
            messages.extend(retained);
        }
        self.update_interiors(&mut messages);
        Ok(messages)
    }

    fn update_interiors(&mut self, messages: &mut Vec<SessionMessage>) {
        for (child, link) in &self.links {
            let source = self.sources.values().find(|cursor| {
                cursor.projector.parsed.header.as_ref().and_then(|header| header.native_id.as_ref()).map(NativeId::as_str) == Some(child.as_str())
            });
            let readable = source.filter(|cursor| cursor.offset > 0 && cursor.path.is_file());
            let complete = readable.is_some_and(|cursor| {
                std::fs::metadata(&cursor.path).is_ok_and(|meta| meta.len() == cursor.offset)
            });
            let state = if self.forwarded_closes.contains(child) || (self.finalizing && complete) {
                InteriorState::Full
            } else if readable.is_some() || self.forwarded_sources.contains(child) {
                InteriorState::Partial
            } else if self.finalizing {
                InteriorState::Unreadable
            } else {
                InteriorState::Pending
            };
            let mut reported = false;
            for message in messages.iter_mut() {
                if let SessionMessage::Event(event) = message {
                    if event.kind == EventKind::Subagent && event.stream_id.as_deref() == Some(child.as_str()) {
                        event.interior_state = Some(state);
                        reported = true;
                    }
                }
            }
            if !reported && self.interior_states.get(child).is_some_and(|old| *old != state) {
                let mut event = Event::new(EventKind::Subagent, Some(Phase::Delta), 0);
                event.stream_id = Some(child.clone());
                event.span = link.host.clone();
                event.interior_state = Some(state);
                messages.push(SessionMessage::Event(event));
            }
            self.interior_states.insert(child.clone(), state);
        }
    }

    fn discover(&mut self) -> Result<Vec<(PathBuf, Header)>> {
        let mut all = Vec::new();
        let mut present = HashSet::new();
        for candidate in candidates(&self.store_root, is_rollout) {
            present.insert(candidate.path.clone());
            let stamp = file_stamp(&candidate.path);
            let header = stamp
                .as_ref()
                .and_then(|stamp| {
                    self.header_cache
                        .get(&candidate.path)
                        .and_then(|cached| (cached.stamp == *stamp).then(|| cached.header.clone()))
                })
                .or_else(|| header_of(&candidate));
            if let (Some(header), Some(stamp)) = (header, stamp) {
                self.header_cache.insert(
                    candidate.path.clone(),
                    HeaderCacheEntry {
                        stamp,
                        header: header.clone(),
                    },
                );
                all.push((candidate.path, header));
            }
        }
        self.header_cache.retain(|path, _| present.contains(path));
        let mut by_id: HashMap<String, (PathBuf, Header)> = HashMap::new();
        for (path, header) in all {
            let Some(id) = header.native_id.as_ref() else {
                continue;
            };
            // Keep the newest path when a copied/replayed rollout carries the same id.
            by_id
                .entry(id.as_str().to_string())
                .or_insert((path, header));
        }
        let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
        for (id, (_path, header)) in &by_id {
            if let Some(parent) = header.parent_native_id.as_ref() {
                children_by_parent
                    .entry(parent.as_str().to_string())
                    .or_default()
                    .push(id.clone());
            }
        }
        for children in children_by_parent.values_mut() {
            children.sort();
        }
        let mut selected = Vec::new();
        let mut queue = VecDeque::from([self.root_id.as_str().to_string()]);
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some((path, header)) = by_id.get(&id).cloned() else {
                continue;
            };
            selected.push((path, header));
            if let Some(children) = children_by_parent.get(&id) {
                queue.extend(children.iter().cloned());
            }
        }
        Ok(selected)
    }

    fn rebuild_links(&mut self, inventory: &[(PathBuf, Header)]) {
        let mut links = std::mem::take(&mut self.links);
        // Header lineage keeps a child visible even when its parent activity was compacted or
        // not yet flushed. It does not invent a host call; delegation remains pending until one
        // appears, while forks are independent graph edges.
        for (_path, header) in inventory {
            let Some(id) = header.native_id.as_ref() else {
                continue;
            };
            if header.parent_native_id.is_none() {
                continue;
            }
            links
                .entry(id.as_str().to_string())
                .or_insert_with(|| ChildLink {
                    relation: header.lineage_kind.unwrap_or(LineageKind::Fork),
                    host: None,
                    name: header
                        .agent_path
                        .as_deref()
                        .and_then(|path| path.rsplit('/').next())
                        .filter(|name| !name.is_empty())
                        .map(str::to_string),
                });
        }
        self.links = links;
    }

    fn remember_projected_links(&mut self, projected: &mut [SessionMessage]) {
        for message in projected {
            let SessionMessage::Event(event) = message else {
                continue;
            };
            if event.kind != EventKind::Subagent || event.phase != Some(Phase::Started) {
                continue;
            }
            let Some(child_id) = event.stream_id.as_ref() else {
                continue;
            };
            // The parent rollout's declaration is already the canonical trajectory-open event;
            // a later child-file discovery or generation replay must not emit a second
            // `started` phase for it.
            let already_declared = !self.declared.insert(child_id.clone());
            if already_declared {
                event.phase = Some(Phase::Delta);
            }
            self.links.insert(
                child_id.clone(),
                ChildLink {
                    relation: LineageKind::Delegation,
                    host: event.span.clone(),
                    name: event.name.clone(),
                },
            );
        }
    }
}

/// Follow a rollout until the caller drops the stream. A bounded poll keeps idle sessions cheap
/// while still observing Codex's item-level flush cadence.
fn follow_rollout(id: NativeId, path: PathBuf, finalizing: bool) -> EventStream {
    const POLL: std::time::Duration = std::time::Duration::from_millis(250);
    Box::pin(futures::stream::unfold(
        RolloutFollowState::new(id, path, finalizing),
        |mut state| async move {
            loop {
                if state.stopped {
                    return None;
                }
                if state.retry_after_error {
                    state.retry_after_error = false;
                    tokio::time::sleep(POLL).await;
                }
                if let Some(message) = state.pending.pop_front() {
                    return Some((Ok(message), state));
                }
                match state.poll() {
                    Ok(messages) if !messages.is_empty() => {
                        state.pending.extend(messages);
                        continue;
                    }
                    Ok(_) if state.finalizing => return None,
                    Ok(_) => tokio::time::sleep(POLL).await,
                    Err(error) => {
                        // A finite adoption reports a malformed record once and then closes;
                        // otherwise the ingest drain would retry the same permanent error
                        // forever. A live watch keeps retrying after its caller asks for the
                        // next item, allowing a producer to repair a mid-write record.
                        state.stopped = state.finalizing;
                        state.retry_after_error = !state.finalizing;
                        return Some((Err(error), state));
                    }
                }
            }
        },
    ))
}

/// Find one rollout by its native id.
///
/// # The filename carries the id, and that is measured rather than assumed
///
/// A rollout is `sessions/<yyyy>/<mm>/<dd>/rollout-<iso>-<uuid>.jsonl`, and the uuid equals
/// the header's `id` on **every** file. So the store can be narrowed by name before
/// anything is opened, which is what keeps `locate` from reading every header — the walk is
/// already ordered newest-first by `candidates`, and a name test is one string comparison.
///
/// The header is nevertheless still read on the candidate that matched, because the name is a
/// filesystem fact and the id is the vendor's: a copied or renamed file would otherwise be
/// adopted under an id its records do not carry. Cheap, since it is one file.
fn locate(id: &NativeId) -> Result<PathBuf> {
    let root = store_root()
        .ok_or_else(|| anyhow!("HOME is not set, so ~/.codex/sessions cannot be located"))?;
    let wanted = id.as_str();
    for candidate in candidates(&root, is_rollout) {
        let Some(name) = candidate.path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.contains(wanted) {
            continue;
        }
        if header_of(&candidate)
            .is_some_and(|header| header.native_id.as_ref().map(NativeId::as_str) == Some(wanted))
        {
            return Ok(candidate.path);
        }
    }
    Err(anyhow!(
        "no `codex` session {} in {}",
        id.as_str(),
        root.display()
    ))
}

/// Whether this path is a rollout.
///
/// Both halves are needed and both are measured: **every** file under the store is
/// `rollout-*.jsonl` and there is nothing else there at all — no sidecars, no index, no
/// per-delegate directory. The `thread_history_*.sqlite` projection lives one level *up*, in
/// `~/.codex`, so it is not reachable by this walk even by accident.
///
/// **A delegate is a rollout of its own and is enumerated as one**, which is the opposite of
/// `claude`'s rule and is right for this vendor: a codex delegate has its own thread id, its
/// own `session_meta`, and can be resumed by that id, so hiding it would make 214 real
/// resumable sessions unreachable. Its lineage travels in `parent_native_id` instead.
fn is_rollout(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-"))
}

/// Parse everything the store already holds for one session, once.
///
/// The stream ends when the pass does.
pub fn adopt(id: &NativeId) -> Result<EventStream> {
    let path = locate(id)?;
    Ok(follow_rollout(id.clone(), path, true))
}

/// Parse, then follow the rollout as it grows.
///
/// Follow the rollout and any descendant rollouts as they grow. The cursor is generation-aware:
/// a replacement or truncate-rewrite resets parsing to a new generation, while unchanged lines
/// at the same offsets are replayed only for state reconstruction and are not emitted twice.
pub fn watch(id: &NativeId) -> Result<EventStream> {
    let path = locate(id)?;
    Ok(follow_rollout(id.clone(), path, false))
}

/// Which native sessions this machine's store holds.
///
/// # The rollouts are authoritative, and the SQLite projection is deliberately not read
///
/// *Measured 2026-08-28:* the rollout files outnumbered the threads projected into
/// `~/.codex/thread_history_1.sqlite` by more than an order of magnitude, so a reader that
/// consulted only the projection would miss the large majority of sessions on this machine. `thread_history_projection_state` names it
/// outright — it is an incrementally-built *index over* the rollouts, tracking a
/// `next_rollout_byte_offset` into each one.
///
/// **What the projection alone carries, and what this listing therefore does not report:**
/// turn-level `status` (`completed` 171 / `interrupted` 23 / `failed` 19 on this install),
/// `duration_ms`, `started_at`, and `completed_at`, per turn rather than per session. Three
/// reasons it is not read here, in the order that decides it:
///
/// 1. **This crate has no SQLite dependency and must not gain one** for a field a rollout scan
///    can answer. `crates/adapters/Cargo.toml` carries neither `sqlx` nor `rusqlite`, and
///    adding one for 6% of sessions would put a C library and a schema assumption in every
///    envagent to enrich a listing.
/// 2. **A `NativeSummary` is per session, and those four fields are per turn.** There is no
///    field on this row for them, so reading them would mean either inventing one or
///    collapsing N turns into one number — and a collapsed `duration_ms` is a plausible
///    wrong answer rather than a missing one.
/// 3. **A rollout already carries the session-level equivalents.** `task_complete` records the
///    vendor's own `duration_ms` per turn (2900 of 4317 closes), and `turn_aborted` records an
///    interruption — both projected as events by [`adopt`], where a reader that wants turn
///    status can have it at full fidelity rather than as a listing summary.
///
/// So the decision is: **the projection is not consulted, and the facts it holds are reached
/// through the rollout's own events instead.** If a future surface genuinely needs per-turn
/// status without a parse, the right move is a dependency added deliberately with a
/// measurement behind it — not a silent read of a vendor's private schema, which
/// `_sqlx_migrations` shows is versioned and free to change.
///
/// # One header read per file, not one parse
///
/// `sylph_protocol::Adapter::enumerate` makes this a requirement rather than an optimization.
/// The whole header is on the **first line** of every file, so a listing is one short read
/// each; a full parse of the store is 344k records.
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
    for candidate in candidates(&root, is_rollout) {
        let Some(header) = header_of(&candidate) else {
            continue;
        };
        let Some(native_id) = header.native_id.clone() else {
            // A header with no `id` names no session, so it cannot be adopted. Measured, no
            // real file is in this state — it is the one field with no fallback.
            continue;
        };

        if let Some(wanted) = &filter.workdir {
            // **An exact path match after normalizing trailing slashes, never a substring.**
            // The store this replaces compiled the filter to `LIKE '%' || ? || '%'`, so asking
            // for `/w/app` also returned `/w/app-old` and every subdirectory
            // (`StoreFilter::workdir`).
            let Some(actual) = &header.workdir else {
                continue;
            };
            if normalize_workdir(actual) != normalize_workdir(wanted) {
                continue;
            }
        }

        let ended_at_ms = last_recorded_ms(&candidate);
        if let Some(since) = filter.since_ms {
            // The session's own last activity when it recorded one, and the file's mtime
            // otherwise — an absent end is not an ended session, so falling back to the
            // filesystem keeps a still-being-written session from being filtered out of every
            // listing that names a window.
            let last = ended_at_ms.unwrap_or(candidate.modified_at_ms);
            if last < since {
                continue;
            }
        }

        found.push(NativeSummary {
            native_id,
            workdir: header.workdir,
            // **`codex` records no session title, and that is verified rather than
            // inherited from `docs/agents.md`.** Measured over the whole store: exactly **one**
            // file carries a `thread_name_updated` and 15 carry a `thread_goal_updated`
            // objective — so almost none have anything that could serve, and reading those few
            // would mean scanning whole files to title 1.3% of a listing. The server's
            // opening-ask fallback owns this (`NativeSummary::title`), which is also where
            // the injected-envelope rule lives — and this store needs it badly, since its
            // first `user` record is measurably an `<environment_context>` or an
            // `# AGENTS.md instructions` envelope.
            title: None,
            started_at_ms: header.started_at_ms,
            ended_at_ms,
            // Absent rather than estimated: a line count is a full read, and a *guessed*
            // count is a number a UI renders as fact. A record count is not an event count
            // here either — one `item_completed` can hold a tool result plus its own timing.
            event_count: None,
            // The relation this vendor's key already distinguishes, promoted out of the
            // private `LineageKind` so the server can write a typed edge. It was previously
            // reachable only as a string inside a child declaration's `data` blob, which
            // made the same fact have two derivations and one of them untyped.
            parent_relation: header.lineage_kind.map(|kind| match kind {
                LineageKind::Delegation => sylph_protocol::SessionEdgeRelation::Delegation,
                LineageKind::Fork => sylph_protocol::SessionEdgeRelation::Fork,
            }),
            parent_native_id: header.parent_native_id,
            fidelity: NATIVE_FIDELITY,
        });
        if let Some(limit) = filter.limit {
            if found.len() >= limit {
                break;
            }
        }
    }

    // `candidates` already ordered by mtime, which is the cheap ordering key every listing
    // wants. Deliberately not re-sorted on the session's own recorded end: that is a different
    // fact — a file being written has an mtime and no end stamp — and sorting on it would
    // reorder by a field a live session does not have.
    Ok(found)
}

/// A trailing slash is not a different directory.
/// One listing row's worth of header, from the file's first line.
///
/// # The header is one line, and reading a second would be reading the parent's
///
/// Measured: **every** rollout opens with `session_meta`, so one line is the whole
/// header. That is also what makes the *first* one the right one — 44 files carry a second
/// `session_meta` further in, which is the **parent's** header replayed into a forked delegate
/// (see [`project_record`], which keeps the first for the same reason). A reader taking the
/// last would give those 44 sessions their parent's id, cwd, and era.
///
/// `started_at_ms` comes from the record's **outer** timestamp rather than the payload's own.
/// Both are present on every file and they differ: measured, the outer is later by a median of
/// 769 ms and by up to 38 hours on a resumed thread, because the payload's stamp is when the
/// *thread* was created while the outer one is when this file was written. The outer stamp is
/// the one consistent with every other record in the file, and mixing the two would make a
/// session's own start later than its first event.
fn header_of(candidate: &SessionCandidate) -> Option<Header> {
    /// Enough for the largest real header. The `base_instructions` payload alone is 17.7 KB of
    /// the vendor's own system prompt, and a collaboration header carries a second copy of the
    /// team preamble — measured, the largest first line in the store is under 64 KB.
    const HEAD_BYTES: u64 = 96 * 1024;

    let bytes = read_span(&candidate.path, 0, HEAD_BYTES)?;
    let text = String::from_utf8_lossy(&bytes);
    // The first *complete* line only. A header longer than the window would otherwise be
    // parsed from a truncated string, which fails rather than yielding a partial header — and
    // that failure is the right outcome: a listing row built from half a header would name a
    // session whose workdir is absent for a reason nothing records.
    let line = text.lines().next()?;
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let mut header = Header::read(record.get("payload")?);
    header.started_at_ms = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(epoch_millis_from_iso);
    Some(header)
}

/// The last timestamped record in a file, from its tail.
///
/// A window rather than a parse, for the reason [`enumerate`] states. The first line of the
/// window is dropped when the file is larger than the window, because the read starts
/// mid-record — a tail read cannot simply be split on newlines.
///
/// `None` when the tail holds no parseable record, which for this store means a file being
/// appended to at the moment it was read.
fn last_recorded_ms(candidate: &SessionCandidate) -> Option<i64> {
    /// A record here can be a whole tool output. Measured, the largest single line in the
    /// store is 1.1 MB, but the *last* line is a `task_complete` or a `token_count` and those
    /// run to a few KB — so 256 KB finds the final stamp without paying for the outliers.
    const TAIL_BYTES: u64 = 256 * 1024;

    let from = candidate.size.saturating_sub(TAIL_BYTES);
    let bytes = read_span(&candidate.path, from, TAIL_BYTES)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if from > 0 {
        // The first line of the window began before it.
        lines.next();
    }
    let mut last = None;
    for line in lines {
        if let Some(stamp) = serde_json::from_str::<Value>(line.trim())
            .ok()
            .and_then(|record| {
                record
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(epoch_millis_from_iso)
            })
        {
            last = Some(stamp);
        }
    }
    last
}

/// Read at most `len` bytes from `offset`.
fn read_span(path: &Path, offset: u64, len: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).ok()?;
    }
    let mut buffer = Vec::new();
    file.take(len).read_to_end(&mut buffer).ok()?;
    Some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture directory, from this file's location rather than the process cwd.
    ///
    /// `cargo test` runs with the *crate* as cwd while `cargo test --workspace` may not, so a
    /// relative fixture path is a test that passes in one invocation and not the other. Same
    /// helper the sibling parsers use, for the same reason.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/native/codex")
            .join(name)
    }

    /// One whole fixture, read the way production reads it.
    ///
    /// **Deliberately a [`RolloutCursor`] rather than a loop of its own.** These fixtures are
    /// the measured evidence about the vendor's store, so what they must be evidence *about* is
    /// the projection that actually runs: [`read_complete_records`] feeding
    /// [`RolloutProjector::apply`], which is exactly what [`RolloutFollowState::poll`] does per
    /// source. A bespoke reader here would be a second derivation of the same fact — and the
    /// one this file used to have was reached only from tests, so every assertion below
    /// confirmed a parser no `adopt` or `watch` ever called.
    ///
    /// One `poll` is the whole file: these fixtures are static and every one ends in a newline,
    /// so the first pass has no unterminated tail to hold back and no second generation to
    /// observe. `watch_fixture_dir`'s tests own the incremental and generation behaviour.
    fn replay(name: &str) -> (Option<Header>, Vec<Event>) {
        let mut cursor = RolloutCursor::new(fixture(name));
        let messages = cursor.poll().expect("the fixture must parse");
        let events = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event.clone()),
                _ => None,
            })
            .collect();
        (cursor.projector.parsed.header.clone(), events)
    }

    /// Project one fixture and keep only the events.
    fn project(name: &str) -> Vec<Event> {
        replay(name).1
    }

    #[test]
    fn raw_revision_survives_reopen_and_distinguishes_rewritten_bytes() {
        let dir = std::env::temp_dir().join(format!("sylph-codex-raw-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-test.jsonl");
        std::fs::write(&path, "{\"type\":\"future\",\"payload\":1}\n").unwrap();
        let read = || {
            RolloutCursor::new(path.clone()).poll().unwrap().into_iter().find_map(|message| {
                match message { SessionMessage::NativeRecord(record) => Some(record), _ => None }
            }).unwrap()
        };
        let first = read();
        assert_eq!(first.version, read().version);
        std::fs::write(&path, "{ \"type\" : \"future\", \"payload\" : 1 }\n").unwrap();
        let rewritten = read();
        assert_eq!(first.payload, rewritten.payload);
        assert_ne!(first.version, rewritten.version, "lexical changes are retained revisions");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retained_record_reprojection_uses_payload_without_source_path() {
        let record = NativeRecord { source: "/does/not/exist".into(), version: 1, raw_json: None, payload: json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"m","content":[{"type":"Text","text":"ok"}]}}}), blobs: Vec::new() };
        let mut projector = RolloutProjector::default();
        assert!(reproject_native_record(&mut projector, &record).iter().any(|message| matches!(message, SessionMessage::Event(event) if event.text == "ok")));
    }

    #[test]
    fn item_recorded_time_uses_completion_even_without_an_outer_stamp() {
        for outer in [Some("2026-09-02T00:00:00.100Z"), None] {
            let mut projector = RolloutProjector::default();
            projector.apply(&json!({"type":"session_meta","payload":{"id":"time","history_mode":"paginated"}}));
            let mut record = json!({"type":"event_msg","payload":{"type":"item_completed","completed_at_ms":1788307200090_i64,"started_at_ms":1788307200000_i64,"item":{"type":"AgentMessage","id":"msg","content":[{"type":"Text","text":"answer"}]}}});
            if let Some(outer) = outer { record["timestamp"] = json!(outer); }
            let messages = projector.apply(&record);
            let event = messages.iter().find_map(|message| match message { SessionMessage::Event(event) if event.kind == EventKind::Assistant => Some(event), _ => None }).unwrap();
            assert_eq!(event.recorded_at_ms, Some(1788307200090));
        }
    }

    #[test]
    fn adopt_retains_each_rollout_record_before_projection() {
        let mut cursor = RolloutCursor::new(fixture("ordinary-turn-item-completed-era.jsonl"));
        let messages = cursor.poll().expect("fixture must parse");
        let retained: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::NativeRecord(record) => Some(record),
                _ => None,
            })
            .collect();
        let expected = records("ordinary-turn-item-completed-era.jsonl").len();
        assert_eq!(
            retained.len(),
            expected,
            "every JSONL row is retained as raw"
        );
        assert!(retained.iter().all(|record| record.raw_json.is_some()));
        assert_eq!(retained.iter().map(|record| record.version).collect::<HashSet<_>>().len(), expected);
    }

    /// The header one fixture's own `session_meta` yields, from the same pass.
    fn header(name: &str) -> Header {
        replay(name)
            .0
            .expect("every measured rollout opens with a session_meta")
    }

    fn of_kind(events: &[Event], kind: EventKind) -> Vec<&Event> {
        events.iter().filter(|event| event.kind == kind).collect()
    }

    /// One line of a fixture, as the vendor wrote it. Used where a test has to be evidence
    /// about the *store* rather than about this parser.
    fn records(name: &str) -> Vec<Value> {
        std::fs::read_to_string(fixture(name))
            .expect("the fixture must be readable")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every fixture line is JSON"))
            .collect()
    }

    /// **The era check selects one content stream, and a paginated file must not
    /// double-count.**
    ///
    /// This is the defect `Era` exists to prevent: `item_completed` and
    /// `event_msg/agent_message` are mutually exclusive per file, so a reader that took
    /// "both must be read" literally would emit every assistant message twice on a
    /// paginated file. Asserted by counting distinct `stream_id`s rather than events,
    /// because a duplicate is the same block arriving twice.
    #[test]
    fn a_paginated_file_yields_no_duplicated_assistant_text() {
        let events = project("ordinary-turn-item-completed-era.jsonl");
        let assistants = of_kind(&events, EventKind::Assistant);
        assert!(
            !assistants.is_empty(),
            "the paginated fixture must yield assistant text at all"
        );

        let distinct: std::collections::BTreeSet<_> = assistants
            .iter()
            .filter_map(|e| e.stream_id.as_deref())
            .collect();
        assert_eq!(
            distinct.len(),
            assistants.len(),
            "each assistant block must appear once; a repeated stream_id is the \
             double-count the era check exists to prevent"
        );

        // And the file really is the era under test, or the assertion above proves
        // nothing about paginated handling.
        assert_eq!(
            header("ordinary-turn-item-completed-era.jsonl").era,
            Era::Paginated
        );
    }

    /// **A delegation edge points parent → child, and the child's own rollout must not
    /// invert it.**
    ///
    /// `agent_thread_id` names the *other party*: in a parent's rollout it is the child,
    /// and in the child's own `interacted` record it is the parent. Mapping it to `span`
    /// unconditionally therefore produces a tree that is **backwards rather than
    /// incomplete**, which nothing downstream can detect — so this is the highest-value
    /// assertion in the file.
    #[test]
    fn a_delegation_edge_is_not_inverted_by_the_childs_own_record() {
        let parent_file = "subagent-parent-collab-namespace-newest-format-spawn-agent.jsonl";
        let child_file = "subagent-child-collab-namespace-newest-format-of-parent-019ff38d.jsonl";

        // Parent side: each delegation opens a trajectory whose stream is the *child's*
        // thread and whose span is the hosting call.
        let events = project(parent_file);
        let subagents = of_kind(&events, EventKind::Subagent);
        assert_eq!(subagents.len(), 3, "the fixture spawns three agents");
        for sub in &subagents {
            let stream = sub
                .stream_id
                .as_deref()
                .expect("a trajectory has a stream id");
            let span = sub.span.as_deref().expect("and names its hosting call");
            assert!(
                span.starts_with("call_"),
                "the span must be the host tool call, got {span:?}"
            );
            assert_eq!(sub.interior_state, None, "the single-file projector cannot establish child reachability");
            assert_ne!(
                stream, span,
                "a trajectory keyed by its host cannot distinguish two delegates \
                 under one call"
            );
        }

        // Child side: its header names its parent, and the id relationship runs the
        // right way round.
        let child = header(child_file);
        let parent = header(parent_file);
        assert_eq!(
            child.parent_native_id, parent.native_id,
            "the child must name the parent, not the reverse"
        );
        assert_ne!(
            parent.parent_native_id, child.native_id,
            "and the parent must not name the child as its own parent, which is what \
             reading agent_thread_id unconditionally would produce"
        );

        // The child's own transcript must not re-open a trajectory for itself: it *is*
        // the delegate, so a `subagent` event here would nest it inside itself.
        let child_events = project(child_file);
        assert!(
            of_kind(&child_events, EventKind::Subagent).is_empty(),
            "a delegate's own rollout must not declare a trajectory for itself"
        );
    }

    /// **A delegation's name is absent when the brief is ciphertext, never a placeholder.**
    ///
    /// From 0.147 the `spawn_agent` brief is encrypted, so the parent's record cannot say
    /// what the delegate was asked to do. `docs/session-protocol.md` § "`name` is absent
    /// when unknown" governs: a journal makes every guess permanent, and a row written
    /// with a placeholder is afterwards indistinguishable from one that carried a real
    /// name.
    ///
    /// The older collab-era fixture is the control — there the brief is plaintext, so a
    /// name and text both survive. Without it this test would pass against a parser that
    /// never named anything.
    #[test]
    fn a_ciphertext_brief_yields_no_invented_name() {
        let newest = project("subagent-parent-collab-namespace-newest-format-spawn-agent.jsonl");
        for sub in of_kind(&newest, EventKind::Subagent) {
            assert!(
                sub.text.is_empty(),
                "an encrypted brief must not produce delegate text; got {:?}",
                sub.text
            );
        }

        // Control: the era whose brief is readable does carry one.
        let older = project("subagent-parent-collab-era-spawn-agent.jsonl");
        let named = of_kind(&older, EventKind::Subagent)
            .into_iter()
            .find(|sub| !sub.text.is_empty())
            .expect("the plaintext-brief era must still yield a brief");
        assert!(
            named.name.is_some(),
            "a delegation with a readable brief must be named"
        );
    }

    /// **Reasoning emits its summary or nothing — never an empty thinking event.**
    ///
    /// Measured: every `response_item/reasoning` record carries `encrypted_content` and
    /// only ~8% also carry a plaintext `summary`. An empty `thinking` event renders as a
    /// blank block, which reads as a rendering bug rather than as an absent record.
    #[test]
    fn reasoning_without_a_plaintext_summary_emits_nothing() {
        for name in [
            "subagent-parent-legacy-shape1-spawn-agent.jsonl",
            "ordinary-turn-item-completed-era.jsonl",
        ] {
            for event in of_kind(&project(name), EventKind::Thinking) {
                assert!(
                    !event.text.trim().is_empty(),
                    "{name} emitted an empty thinking event; the summary is absent on \
                     ~92% of reasoning records and must be omitted rather than blanked"
                );
            }
        }

        // The store really does hold reasoning records with no plaintext summary, or the
        // loop above is vacuous. This asserts about the *vendor's* file, not the parser.
        let has_encrypted_only = records("ordinary-turn-item-completed-era.jsonl")
            .iter()
            .filter_map(|record| record.get("payload"))
            .any(|payload| {
                payload.get("type").and_then(Value::as_str) == Some("reasoning")
                    && payload.get("encrypted_content").is_some()
                    && payload
                        .get("summary")
                        .and_then(Value::as_array)
                        .is_none_or(|summary| summary.is_empty())
            });
        assert!(
            has_encrypted_only,
            "this fixture must contain a reasoning record with ciphertext and no \
             summary, or the assertion above proves nothing"
        );
    }

    /// **An interrupted turn can lack a start record as well as a completion**, so a
    /// parser must not synthesize either bracket.
    ///
    /// Measured on this fixture: it holds `event_msg/turn_aborted` and **no
    /// `task_started`** at all. So the honest projection reports no `turn` event rather
    /// than inventing one — `docs/session-protocol.md` § "`name` is absent when unknown"
    /// is the same rule for a different field, and a fabricated bracket would put a turn
    /// in the transcript that the vendor never recorded.
    ///
    /// The content around it still projects, which is what makes this a partial record
    /// rather than an unreadable one.
    #[test]
    fn an_interrupted_turn_is_not_given_a_bracket_it_never_had() {
        let events = project("turn-aborted-interrupted.jsonl");
        assert!(
            of_kind(&events, EventKind::Turn).is_empty(),
            "this rollout records no task_started, so no turn event may be synthesized"
        );

        // The record is partial, not empty: its reasoning and usage still arrive.
        assert!(
            !events.is_empty(),
            "an aborted turn's content must still project"
        );

        // Evidence about the store rather than about the parser: the abort is recorded
        // and the start genuinely is not.
        let payloads: Vec<_> = records("turn-aborted-interrupted.jsonl")
            .iter()
            .filter_map(|record| {
                record
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        assert!(
            payloads.iter().any(|kind| kind == "turn_aborted"),
            "the fixture must carry the abort"
        );
        assert!(
            !payloads.iter().any(|kind| kind == "task_started"),
            "and must not carry a start, or this test is asserting the wrong thing"
        );
    }

    /// The era discriminant, and why it is not the version.
    #[test]
    fn an_absent_history_mode_is_legacy_rather_than_an_error() {
        // The large majority of files carry no `history_mode` at all.
        assert_eq!(Era::of(None), Era::Legacy);
        assert_eq!(Era::of(Some("legacy")), Era::Legacy);
        assert_eq!(Era::of(Some("paginated")), Era::Paginated);
    }

    /// An unrecognized value reads as legacy rather than panicking.
    ///
    /// Deliberate: the vendor has added a mode before and the older vocabulary is the
    /// one every pre-`history_mode` file speaks, so an unknown mode degrades to the
    /// reading that is right for the majority rather than refusing the file.
    #[test]
    fn an_unknown_history_mode_degrades_to_legacy() {
        assert_eq!(Era::of(Some("something-new")), Era::Legacy);
    }

    fn watch_fixture_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("sylph-codex-watch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("2026/09/03")).unwrap();
        let parent = root.join("2026/09/03/rollout-parent.jsonl");
        let child = root.join("2026/09/03/rollout-child.jsonl");
        std::fs::copy(fixture("watch-parent.jsonl"), &parent).unwrap();
        (root, parent, child)
    }

    fn watch_events(messages: &[SessionMessage]) -> Vec<Event> {
        messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event.clone()),
                _ => None,
            })
            .collect()
    }

    /// A child rollout can be created after the parent watcher starts. The parent declaration
    /// is emitted once, and the child's own records arrive later with the child trajectory as
    /// their span. This is the path that a one-shot `adopt`/`watch` implementation misses.
    #[test]
    fn an_absent_child_source_is_pending_live_and_unreadable_on_final_read() {
        for finalizing in [false, true] {
            let (root, parent, _) = watch_fixture_dir(if finalizing { "missing-final" } else { "missing-live" });
            let mut state = RolloutFollowState::new_with_root(NativeId::new("watch-parent"), parent, root.clone(), finalizing);
            let events = watch_events(&state.poll().unwrap());
            let declaration = events.iter().find(|event| event.kind == EventKind::Subagent && event.stream_id.as_deref() == Some("watch-child")).unwrap();
            assert_eq!(declaration.interior_state, Some(if finalizing { InteriorState::Unreadable } else { InteriorState::Pending }));
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_watcher_discovers_a_child_rollout_created_after_the_first_poll() {
        let (root, parent, child) = watch_fixture_dir("late-child");
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id.clone(), parent, root.clone(), false);

        let first = state.poll().unwrap();
        let first_events = watch_events(&first);
        assert!(first_events.iter().any(|event| {
            event.kind == EventKind::Subagent
                && event.stream_id.as_deref() == Some("watch-child")
                && event.interior_state == Some(InteriorState::Pending)
        }));
        assert!(
            first_events
                .iter()
                .all(|event| event.text != "child prefix"),
            "the child file does not exist yet"
        );

        std::fs::copy(fixture("watch-child.jsonl"), &child).unwrap();
        let second = state.poll().unwrap();
        let second_events = watch_events(&second);
        let child_messages: Vec<&Event> = second_events
            .iter()
            .filter(|event| event.text == "child prefix")
            .collect();
        assert_eq!(child_messages.len(), 1);
        assert_eq!(child_messages[0].span.as_deref(), Some("watch-child"));
        assert_eq!(
            second_events
                .iter()
                .filter(|event| {
                    event.kind == EventKind::Subagent
                        && event.phase == Some(Phase::Started)
                        && event.stream_id.as_deref() == Some("watch-child")
                })
                .count(),
            0,
            "the parent-side declaration must not be duplicated on child discovery"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A fresh reader rebuilds the same parent/child graph from native headers and the parent
    /// activity record. Reload must not depend on the in-memory links of an earlier watcher.
    #[test]
    fn a_reload_reconstructs_the_child_edge_and_trajectory_span() {
        let (root, parent, child) = watch_fixture_dir("reload");
        std::fs::copy(fixture("watch-child.jsonl"), &child).unwrap();
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id, parent, root.clone(), true);
        let events = watch_events(&state.poll().unwrap());

        let declarations: Vec<&Event> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("watch-child")
            })
            .collect();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].span.as_deref(), Some("call-watch-child"));
        assert_eq!(declarations[0].interior_state, Some(InteriorState::Full));
        let child = events
            .iter()
            .find(|event| event.text == "child prefix")
            .expect("the reloaded child content must be projected");
        assert_eq!(child.span.as_deref(), Some("watch-child"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_fork_child_keeps_fork_provenance_separate_from_delegation() {
        let (root, parent, fork) = watch_fixture_dir("fork");
        std::fs::copy(fixture("watch-fork.jsonl"), &fork).unwrap();
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id, parent, root.clone(), true);
        let events = watch_events(&state.poll().unwrap());

        let declaration = events
            .iter()
            .find(|event| {
                event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("watch-fork")
            })
            .expect("the fork edge is represented");
        assert_eq!(declaration.span, None);
        assert_eq!(
            declaration
                .data
                .as_ref()
                .and_then(|data| data.get("relation")),
            Some(&json!("fork"))
        );
        let content = events
            .iter()
            .find(|event| event.text == "fork content")
            .expect("the fork rollout content is projected");
        assert_eq!(content.span.as_deref(), Some("watch-fork"));

        let _ = std::fs::remove_dir_all(root);
    }

    /// Codex may forward a child's item into the parent rollout as well as writing the child's
    /// own rollout. The graph reader keeps the parent copy's trajectory provenance and suppresses
    /// the later duplicate from the child source.
    #[test]
    fn forwarded_child_items_are_not_emitted_twice_after_child_discovery() {
        let (root, parent, child) = watch_fixture_dir("forwarded");
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id, parent.clone(), root.clone(), false);
        let _ = state.poll().unwrap();

        use std::io::Write;
        let mut parent_file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        writeln!(
            parent_file,
            r#"{{"timestamp":"2026-09-03T00:00:00.700Z","type":"event_msg","payload":{{"type":"item_completed","thread_id":"watch-child","item":{{"type":"AgentMessage","id":"msg-watch-child","content":[{{"type":"Text","text":"child prefix"}}]}}}}}}"#
        )
        .unwrap();
        let parent_events = watch_events(&state.poll().unwrap());
        assert_eq!(
            parent_events
                .iter()
                .filter(|event| event.text == "child prefix")
                .count(),
            1
        );

        std::fs::copy(fixture("watch-child.jsonl"), &child).unwrap();
        let child_events = watch_events(&state.poll().unwrap());
        assert_eq!(
            child_events
                .iter()
                .filter(|event| event.text == "child prefix")
                .count(),
            0,
            "the forwarded parent item already represents this child block"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    /// A truncate/replace creates a new generation. Unchanged complete lines are replayed only
    /// to rebuild the projector, while a newly appended line is emitted once.
    #[test]
    fn a_replaced_rollout_advances_generation_without_replaying_its_prefix() {
        let (root, parent, _child) = watch_fixture_dir("generation");
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id, parent.clone(), root.clone(), false);
        let _ = state.poll().unwrap();
        assert_eq!(
            state
                .sources
                .get(&parent)
                .expect("root cursor")
                .generation(),
            0
        );

        let replacement = parent.with_extension("replacement.jsonl");
        let mut contents = std::fs::read(&parent).unwrap();
        contents.extend_from_slice(
            br#"{"timestamp":"2026-09-03T00:00:01.000Z","type":"event_msg","payload":{"type":"item_completed","thread_id":"watch-parent","item":{"type":"AgentMessage","id":"msg-watch-new","content":[{"type":"Text","text":"after reset"}]}}}
"#,
        );
        std::fs::write(&replacement, contents).unwrap();
        std::fs::rename(&replacement, &parent).unwrap();

        let events = watch_events(&state.poll().unwrap());
        assert_eq!(
            state
                .sources
                .get(&parent)
                .expect("root cursor")
                .generation(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.text == "parent prefix")
                .count(),
            0,
            "the unchanged prefix must not be published a second time"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.text == "after reset")
                .count(),
            1
        );
        assert!(events.iter().all(|event| {
            !(event.kind == EventKind::Subagent
                && event.stream_id.as_deref() == Some("watch-child")
                && event.phase == Some(Phase::Started))
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    /// An unterminated line is held at the cursor boundary; once the producer completes it, the
    /// record is parsed and emitted. A complete malformed line is retried rather than skipped.
    #[test]
    fn a_partial_line_is_retried_at_the_same_cursor_boundary() {
        let (root, parent, _child) = watch_fixture_dir("partial");
        let id = NativeId::new("watch-parent");
        let mut state = RolloutFollowState::new_with_root(id, parent.clone(), root.clone(), false);
        let _ = state.poll().unwrap();

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        write!(
            file,
            "{{\"timestamp\":\"2026-09-03T00:00:02.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"thread_id\":\"watch-parent\",\"item\":{{\"type\":\"AgentMessage\",\"id\":\"msg-partial\",\"content\":[{{\"type\":\"Text\",\"text\":\"partial\"}}]}}}}}}"
        )
        .unwrap();
        let offset_before = state.sources.get(&parent).unwrap().offset;
        assert!(watch_events(&state.poll().unwrap())
            .iter()
            .all(|event| event.text != "partial"));
        assert_eq!(state.sources.get(&parent).unwrap().offset, offset_before);

        writeln!(file).unwrap();
        let events = watch_events(&state.poll().unwrap());
        assert_eq!(
            events
                .iter()
                .filter(|event| event.text == "partial")
                .count(),
            1
        );

        // A complete malformed line must not advance the cursor. Repairing the line then lets
        // the same poll boundary consume it.
        writeln!(file, "{{not-json").unwrap();
        let offset_before_bad = state.sources.get(&parent).unwrap().offset;
        assert!(state.poll().is_err());
        assert_eq!(
            state.sources.get(&parent).unwrap().offset,
            offset_before_bad
        );

        let bytes = std::fs::read(&parent).unwrap();
        let repaired = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| *line != b"{not-json")
            .flat_map(|line| line.iter().copied().chain(std::iter::once(b'\n')))
            .collect::<Vec<_>>();
        std::fs::write(&parent, repaired).unwrap();
        let _ = state.poll().unwrap();

        let _ = std::fs::remove_dir_all(root);
    }

    /// Nested teams use path ancestry, not a fixed `/root` literal, to decide which side of a
    /// `SubAgentActivity` record the reader is on.
    #[test]
    fn nested_team_paths_resolve_delegation_direction() {
        let child = json!({
            "type": "SubAgentActivity",
            "agent_thread_id": "parent",
            "agent_path": "/root/research",
            "kind": "interacted"
        });
        assert_eq!(
            delegation_edge_with_path(&child, "child", Some("/root/research/db")),
            Some(Edge::Parent("parent".to_string()))
        );

        let parent = json!({
            "type": "SubAgentActivity",
            "agent_thread_id": "child",
            "agent_path": "/root/research/db",
            "kind": "started"
        });
        assert!(matches!(
            delegation_edge_with_path(&parent, "parent", Some("/root/research")),
            Some(Edge::Child { thread, .. }) if thread == "child"
        ));

        let sibling = json!({
            "type": "SubAgentActivity",
            "agent_thread_id": "other",
            "agent_path": "/root/operations",
            "kind": "started"
        });
        assert_eq!(
            delegation_edge_with_path(&sibling, "parent", Some("/root/research")),
            None,
            "a sibling path is not evidence of a parent/child edge"
        );
    }
}
