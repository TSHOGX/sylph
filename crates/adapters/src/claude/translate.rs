//! `claude`'s stdout payloads, projected onto [`sylph_protocol`] events.
//!
//! The vendor→Sylph projection: `_stream_event`, `_tool_results`,
//! `_system`, `_delegate_of`, `_task_notification`, `_task_label` and `flatten`
//! (roughly :993-1483). `docs/session-protocol.md` §§ Events and Delegated trajectories
//! govern, and `docs/agents.md` § Reading a vendor payload holds the defect family behind
//! every quirk below.
//!
//! # Why this is a separate, process-free module
//!
//! **Every classification bug in this adapter's history lived in this projection, and
//! every one of them compiled.** An end-to-end check that spends real tokens could not see any
//! of them: it spends real tokens, needs a login, and only exercises whatever shapes one
//! live turn happens to produce. A mistranslation does not raise; it produces a
//! plausible timeline that misdescribes what the agent did.
//!
//! So the whole projection is pure functions over one [`Correlation`] value, with no
//! process, no I/O, and no clock of its own. The 13
//! classes exist for that reason and are ported to the bottom of this file.
//!
//! # Time arrives as an argument
//!
//! [`sylph_protocol::Event::created_at_ms`] is required, and **claude's `stream_event`
//! envelopes carry no timestamp at all** — printed and checked, not assumed: the
//! envelope holds `type`, `session_id`, `parent_tool_use_id`, `event` and `uuid`. The
//! only stamp the vendor offers anywhere is `duration_ms` on a `result`, which is an
//! elapsed span rather than an instant. So the adapter stamps arrival, and the clock is
//! a parameter rather than a call to `now()` inside: a test that cannot fix the stamps
//! cannot assert on ordering, and a translation that reads a global clock is no longer
//! the pure function that makes these defects catchable.
//!
//! # Two id types, because one of them is multiplexed
//!
//! `claude` uses one `tool_use_id` for two different things, and conflating them is two
//! separate shipped defects (`AGENTS.md`, `docs/session-protocol.md` § Keying rules that
//! produce corruption when broken). [`HostId`] and [`TrajectoryId`] are therefore
//! distinct types with exactly one conversion between them — [`HostId::as_trajectory`],
//! for the one case where the vendor genuinely reuses an id for both roles — so the
//! wrong keying is a type error rather than a comment asking a reader not to.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};
use sylph_protocol::{CapabilityFacets, Event, EventKind, InteriorState, Phase, SessionMessage};

/// A vendor `tool_use_id` in its role as the tag on forwarded output.
///
/// This is the id `--forward-subagent-text` stamps onto a delegate's events as
/// `parent_tool_use_id`, and **it is precisely the id the vendor multiplexes**: a
/// `Workflow` fans out to N agents under one of these. So it is not a trajectory, and
/// the type says so.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostId(String);

impl HostId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host read *as* a trajectory, for the one case where they are the same thing.
    ///
    /// The built-in `Task` publishes its trajectory under the spawning `tool_use_id` and
    /// claude forwards the child's own events under it too, so for that tool host and
    /// trajectory are one id wearing both hats. This conversion is where that is
    /// admitted.
    ///
    /// **There is deliberately no conversion the other way.** A `TrajectoryId` that
    /// could become a `HostId` would make [`BlockKey`] constructible from a resolved
    /// trajectory, which is the silent block-loss described on `Correlation::blocks`.
    /// Nothing needs the reverse direction, so nothing offers it.
    pub fn as_trajectory(&self) -> TrajectoryId {
        TrajectoryId(self.0.clone())
    }
}

/// A trajectory id: what an event's `span` names and what a `subagent` event's
/// `stream_id` is.
///
/// Per-delegate, never per-host. `claude`'s per-delegate identity is `task_id`; keyed by
/// the host instead, a two-agent workflow rendered as **one** card reading
/// `Demo: agent-2` — the last name to arrive won and `agent-1` was absent from the
/// timeline entirely.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrajectoryId(String);

impl TrajectoryId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What kind of work a `task_*` notification describes.
///
/// **Not every `task_*` notification is a delegation, and `task_type` is the only thing
/// that says so.** Measured on claude 2.x: `run_in_background` on Bash reports through
/// this exact channel with `local_bash`, against `local_agent` for a real delegation.
/// Treating both as trajectories published a *shell command* as a subagent — one card
/// named `find / -maxdepth 8 -ipath "*workflows*" …`, whose error flag escalated the
/// Bash call hosting it, so a turn that had done nothing wrong read `failed` from its
/// first moment.
///
/// [`Self::Other`] is load-bearing rather than tidiness: **an unknown kind must nest.**
/// Guessing wrong that way costs a missing subtree; guessing wrong the other way
/// mislabels a tool call as an agent, which is what shipped. The filter is therefore
/// written against `LocalBash` alone ([`Self::is_delegation`]) and can never widen by
/// accident to a kind the vendor adds later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    /// A real subagent.
    LocalAgent,
    /// `run_in_background` on Bash, reporting through the delegation channel.
    LocalBash,
    /// A kind this build has not seen. Nests, deliberately.
    Other(String),
}

impl TaskKind {
    fn parse(raw: &str) -> Self {
        match raw {
            "local_agent" => Self::LocalAgent,
            "local_bash" => Self::LocalBash,
            other => Self::Other(other.to_string()),
        }
    }

    /// Whether a notification of this kind opens a trajectory.
    ///
    /// One exclusion, named. Anything else — including a kind nobody has written yet —
    /// nests.
    pub fn is_delegation(&self) -> bool {
        !matches!(self, Self::LocalBash)
    }
}

/// The key for in-flight vendor block state: the **envelope's own tag** and the vendor's
/// content-block index.
///
/// Both halves are a shipped defect.
///
/// The index alone is corruption: `claude` numbers content blocks per message, so two
/// concurrent subagents both open index 0, and one index-keyed map made each one's
/// deltas append to the other's text. Data corruption rather than a display fault, and
/// invisible until two delegates run at once.
///
/// The tag rather than the trajectory it resolves to is silent *loss*. A resolution is
/// derived from tables that grow while the turn runs; the tag is the only part the
/// vendor guarantees is identical across a block's `content_block_start`, `_delta` and
/// `_stop`. Keyed by the resolution, a `task_started` landing between a block's start
/// and its first delta moves the key out from under an open block: the delta looks up a
/// key that does not exist and returns, the stop misses too, and no `completed` is ever
/// emitted. The block's whole text is dropped and its node stays open, with nothing at
/// any layer reporting it.
///
/// That is why this type holds a [`HostId`] and there is no `TrajectoryId` → `HostId`
/// conversion: the second defect is unrepresentable rather than merely documented.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// `None` is the root agent's own stream.
    host: Option<HostId>,
    index: u64,
}

impl BlockKey {
    /// The only constructor, named after the one thing it may be built from.
    pub fn for_envelope_tag(host: Option<&HostId>, index: u64) -> Self {
        Self {
            host: host.cloned(),
            index,
        }
    }
}

/// One content block the vendor has opened and not yet closed.
#[derive(Debug, Clone, PartialEq)]
struct OpenBlock {
    kind: EventKind,
    /// This block's `stream_id`, stable across its phases.
    id: String,
    /// The vendor's tool name, for a `tool_call`. Never a delegation's display name —
    /// see [`task_label`].
    name: Option<String>,
    /// Accumulated across deltas. For a `tool_call` or a `subagent` this is streamed
    /// partial JSON and is only parseable once the block closes.
    text: String,
    /// The trajectory this block's `started` claimed, carried forward so every later
    /// phase reports the same one rather than re-deriving a resolution that may have
    /// changed underneath it.
    span: Option<TrajectoryId>,
}

/// Everything one link has to remember to translate the next payload correctly.
///
/// This is the Python original's thirteen correlation tables, with the two id types
/// above replacing the comments that used to be the only thing keeping them apart. Every
/// field here exists because some payload arrives *after* the payload that needed it, or
/// *without* a field an earlier one carried — those are the only two reasons an event
/// translator needs memory at all, and each field below says which of the two it is.
#[derive(Debug, Default)]
pub struct Correlation {
    /// `tool_use_id` → the name to report on its `tool_result`.
    ///
    /// A result payload carries no tool name, so it has to come from the call. For a
    /// declared trajectory this entry is **overwritten** by the parsed brief at
    /// `content_block_stop`, including when the brief yielded nothing — see
    /// [`Correlation::content_block_stop`], where leaving the vendor's `"Task"` in place
    /// would put the tool's name on the card as though the agent had chosen it.
    tool_names: HashMap<String, Option<String>>,

    /// In-flight content blocks. See [`BlockKey`] for why the key is shaped that way; it
    /// is two defects, one of them silent loss of a whole block.
    blocks: HashMap<BlockKey, OpenBlock>,

    /// `tool_use_id`s that *declared* themselves trajectories — the built-in `Task`, and
    /// only it.
    ///
    /// Every other delegating tool is discovered rather than declared
    /// (`docs/session-protocol.md` § Promotion): a tool call is known to have delegated
    /// when a child arrives carrying its id, which is always after the call was
    /// published. The name test was correct until the vendor shipped `Agent` and
    /// `Workflow`, and an MCP server can add another at any time — so the list could
    /// never be completed, only extended after each report.
    ///
    /// `Task` stays declared because declaration buys something promotion cannot: it
    /// names a trajectory before any child exists, and its `tool_result` is the delegated
    /// work's outcome rather than a tool's return value.
    subagent_tools: HashSet<TrajectoryId>,

    /// A declared trajectory → the trajectory its *spawning* call ran in.
    ///
    /// So a nested `Task` closes back into its parent rather than at the root. Reading
    /// the forwarded envelope's own resolution instead reparents a depth-2 delegation
    /// onto itself, because for a nested Task that resolution is the child's own id.
    subagent_parent: HashMap<TrajectoryId, Option<TrajectoryId>>,

    /// Trajectories already opened by a notification, so a repeat becomes a `delta`.
    ///
    /// These notifications arrive many times for one delegate; a second `started` under
    /// one id opens a duplicate node.
    opened_trajectories: HashSet<TrajectoryId>,

    /// Trajectories already closed, so a late notification cannot reopen one.
    ///
    /// A reopened trajectory is a second node under the same id that nothing ever
    /// closes — the permanently running duplicate this whole family of rules exists to
    /// remove.
    closed_trajectories: HashSet<TrajectoryId>,

    /// A host → the delegates announced under it.
    ///
    /// `BTreeSet` rather than `HashSet` so "the sole delegate" is a deterministic answer:
    /// [`Correlation::delegate_of`] latches whatever it reads first, and a latch that
    /// depends on hash order would make one stream's attribution vary between runs.
    host_tasks: HashMap<HostId, BTreeSet<TrajectoryId>>,

    /// A host → the trajectory its forwarded output was **first** attributed to, latched
    /// for the life of the link.
    ///
    /// [`Correlation::delegate_of`] reads a table that grows while the turn runs, so the
    /// same host has one answer before `task_started` and another after it — and the
    /// forwarded stream does not pause in between. Answered freshly each time, one
    /// delegate's output splits across the container and the delegate at whatever instant
    /// the notification landed, and *both halves render*, nested one inside the other, so
    /// no layer reports the split.
    host_attribution: HashMap<HostId, TrajectoryId>,

    /// A trajectory → its host. The inverse of [`Self::host_tasks`], and the reason a
    /// close can be placed in the tree at all.
    ///
    /// **`task_updated` carries no `tool_use_id`**, measured on a real payload. Without
    /// this the close lands at the root while the trajectory it closes sits inside a
    /// container, and the container is then never resolved — the same spinner one level
    /// up.
    task_hosts: HashMap<TrajectoryId, HostId>,

    /// A trajectory → its [`TaskKind`], remembered from `task_started`.
    ///
    /// **Only `task_started` carries `task_type`.** Neither `task_updated` nor
    /// `task_notification` does, so a close filtered only at `started` would open the
    /// very node its start was rejected from — and nothing would ever close *that*.
    task_kinds: HashMap<TrajectoryId, TaskKind>,

    /// Trajectories the vendor said run in the background.
    ///
    /// **Reported only by `task_started`**, and it inverts the server's turn-settle
    /// sweep: backgrounded work is *expected* to outlive its turn and report its own
    /// outcome on a later agent-initiated turn, so closing it as abandoned writes a
    /// contradiction the real completion cannot undo — both records then exist, on
    /// different turns, with the earlier card still saying the work never finished. The
    /// server reads the flag off whichever row it has, so it is stamped onto **every**
    /// later phase (`docs/session-protocol.md` § `is_backgrounded` inverts the sweep).
    task_backgrounded: HashSet<TrajectoryId>,

    /// The current vendor message id, for synthesizing a `stream_id` on a content block
    /// that has none of its own.
    message_id: String,

    /// How many messages this link has seen, for the same synthesis when the vendor's
    /// message carries no id either.
    message_seq: u64,

    /// Whether the **root** agent's stream has opened a turn.
    ///
    /// A subagent's `message_start` must neither open the turn nor be counted as the
    /// root's: only the root agent's stream bounds a turn.
    turn_open: bool,
}

/// What one vendor payload turned into.
///
/// Three fields rather than one `Vec`, because a payload can carry two facts of
/// different *kinds* and flattening them loses the one the caller has to act on.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Emission {
    /// Messages to publish, in the order they happened.
    pub messages: Vec<SessionMessage>,

    /// The vendor said its command surface changed and the adapter must re-read it.
    ///
    /// A flag rather than a [`CapabilityFacets`] because re-reading needs an RPC to the
    /// CLI, which this module deliberately cannot do — `system/commands_changed` carries
    /// only that *something* changed, so trusting its payload would publish a surface
    /// nobody read. The live session performs the round trip; translation only reports
    /// that one is owed.
    pub refresh_capabilities: bool,

    /// The model the vendor reported it is now running, when a payload said so.
    ///
    /// Reported rather than stored, because a config surface must be built from what the
    /// **vendor** last said and never from what the adapter last set — `pi`'s adapter
    /// reported a field only one code path populated, so any surface built by another
    /// path said the session had no model while it plainly had one. The model can also
    /// change without anyone asking: a fallback under load, or a `/model` the user typed
    /// as prose.
    pub observed_model: Option<String>,
}

impl Emission {
    fn push(&mut self, event: Event) {
        self.messages.push(SessionMessage::Event(event));
    }

    /// Events only, for a test that wants to assert on the projection.
    pub fn events(&self) -> Vec<&Event> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect()
    }
}

/// Read a string field, treating an empty string as absent.
///
/// The vendor sends `""` and omits a field interchangeably in several places, and every
/// downstream decision here — whether a block is named, whether a status is terminal —
/// wants the same answer for both. One helper so the two spellings cannot be handled
/// differently in two branches.
fn text_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The vendor's content-block index, which is what a block's phases are numbered by.
///
/// Absent means 0, matching the Python's `nested.get("index", 0)`. A vendor that stopped
/// sending the field would collapse every block onto one key, which the
/// `(host, index)` pairing above localizes to one host rather than corrupting across
/// delegates.
fn index_of(payload: &Value) -> u64 {
    payload.get("index").and_then(Value::as_u64).unwrap_or(0)
}

impl Correlation {
    pub fn new() -> Self {
        Self {
            // Matches the Python's initial `self.message_id`, so a content block arriving
            // before any `message_start` still synthesizes a stable `stream_id` rather
            // than one containing an empty segment.
            message_id: "claude".to_string(),
            ..Default::default()
        }
    }

    /// Whether the root agent's stream currently has a turn open.
    ///
    /// Exposed because the session half needs it to decide whether a vendor `result`
    /// closes anything, and a second copy of the flag there would be a second answer to
    /// one question.
    pub fn turn_is_open(&self) -> bool {
        self.turn_open
    }

    /// Close an Agent/Workflow whose native transcript records completion directly on
    /// `toolUseResult` rather than emitting a `task_notification`.
    ///
    /// The discriminant is deliberately the complete native shape: `agentId`,
    /// `isAsync: false`, and `status: completed`. In particular, `status` alone is not
    /// sufficient because asynchronous launches also report a status field and must stay
    /// open until their later notification.
    pub fn native_agent_completed(
        &mut self,
        agent_id: &str,
        host_tool_use_id: Option<&str>,
        summary: String,
        at_ms: i64,
        out: &mut Emission,
    ) {
        let trajectory = TrajectoryId::new(agent_id.to_string());
        if self.closed_trajectories.contains(&trajectory) {
            return;
        }
        let span = host_tool_use_id
            .map(TrajectoryId::new)
            .filter(|host| host.as_str() != trajectory.as_str())
            .or_else(|| self.task_hosts.get(&trajectory).map(HostId::as_trajectory))
            .or_else(|| self.subagent_parent.get(&trajectory).cloned().flatten());

        self.closed_trajectories.insert(trajectory.clone());
        self.opened_trajectories.remove(&trajectory);

        let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), at_ms);
        event.stream_id = Some(agent_id.to_string());
        event.span = span.map(|id| id.as_str().to_string());
        event.name = self.tool_names.get(agent_id).cloned().flatten();
        event.text = summary;
        out.push(event);
    }

    /// One vendor stdout payload, projected.
    ///
    /// # What is deliberately not here
    ///
    /// `control_response` and `control_request` are the session half's, not this one's. A
    /// control response has to wake the waiter that sent the request, and a control
    /// request has to become a [`sylph_protocol::Request`] whose answer is routed back
    /// over the same stdin — both are correlation against *outbound* traffic this module
    /// never sees. Splitting them out is also what keeps the whole projection testable
    /// without a process.
    ///
    /// An unrecognized `type` produces an empty [`Emission`] rather than an error: the
    /// vendor adds payload types between releases, and one unknown line must not fail a
    /// turn.
    pub fn translate(&mut self, payload: &Value, at_ms: i64) -> Emission {
        match payload.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                // **`parent_tool_use_id` lives on the envelope, not on the nested
                // event**, and it is the only thing distinguishing a subagent's output
                // from the parent's own. Dropping it here — which this adapter did —
                // published every delegate's thinking, prose and tool calls as the root
                // agent's, and the timeline had no way to tell them apart.
                let host = text_field(payload, "parent_tool_use_id").map(HostId::new);
                let nested = payload.get("event").cloned().unwrap_or(Value::Null);
                self.stream_event(&nested, host.as_ref(), at_ms)
            }
            Some("user") => self.tool_results(payload, at_ms),
            Some("system") => self.system(payload, at_ms),
            Some("result") => self.result(payload, at_ms),
            _ => Emission::default(),
        }
    }

    /// claude's partial-message stream, projected onto event kinds.
    ///
    /// `host` is the envelope's `parent_tool_use_id`: the delegating call this output came
    /// from, or `None` for the root agent's own work. Resolved through
    /// [`Self::delegate_of`] it becomes the event's `span`, which is what lets the
    /// timeline nest a delegation to any depth.
    ///
    /// **The block map is keyed by the envelope's own tag, never by the resolved
    /// trajectory** — see [`BlockKey`], where the failure is silent loss of a whole
    /// content block.
    pub fn stream_event(&mut self, nested: &Value, host: Option<&HostId>, at_ms: i64) -> Emission {
        let mut out = Emission::default();
        let parent = self.delegate_of(host);

        match nested.get("type").and_then(Value::as_str) {
            Some("message_start") => self.message_start(nested, parent.as_ref(), at_ms, &mut out),
            Some("content_block_start") => {
                self.content_block_start(nested, host, parent, at_ms, &mut out);
            }
            Some("content_block_delta") => self.content_block_delta(nested, host, at_ms, &mut out),
            Some("content_block_stop") => self.content_block_stop(nested, host, at_ms, &mut out),
            _ => {}
        }
        out
    }

    fn message_start(
        &mut self,
        nested: &Value,
        parent: Option<&TrajectoryId>,
        at_ms: i64,
        out: &mut Emission,
    ) {
        self.message_seq += 1;
        self.message_id = nested
            .get("message")
            .and_then(|message| text_field(message, "id"))
            .unwrap_or_else(|| format!("claude-{}", self.message_seq));

        // A subagent's message must neither open the turn nor be counted as the root's:
        // only the root agent's stream bounds a turn. Without the `parent` test a
        // delegate's first message opened a turn of its own, which the server then had to
        // close, so one exchange reported two turns.
        if !self.turn_open && parent.is_none() {
            self.turn_open = true;
            out.push(Event::new(EventKind::Turn, Some(Phase::Started), at_ms));
        }
    }

    fn content_block_start(
        &mut self,
        nested: &Value,
        host: Option<&HostId>,
        parent: Option<TrajectoryId>,
        at_ms: i64,
        out: &mut Emission,
    ) {
        let index = index_of(nested);
        let content = nested.get("content_block").cloned().unwrap_or(Value::Null);

        // The three block types that carry content. Anything else — the vendor adds
        // types — is not projected, because a kind this vocabulary does not have would
        // have to be invented, which `crates/adapters/src/lib.rs` forbids an adapter
        // from doing.
        let Some(mut kind) = content
            .get("type")
            .and_then(Value::as_str)
            .and_then(|vendor| match vendor {
                "text" => Some(EventKind::Assistant),
                "thinking" => Some(EventKind::Thinking),
                "tool_use" => Some(EventKind::ToolCall),
                _ => None,
            })
        else {
            return;
        };

        let stream_id =
            text_field(&content, "id").unwrap_or_else(|| format!("{}:{}", self.message_id, index));
        let name = text_field(&content, "name");
        let text = text_field(&content, "text")
            .or_else(|| text_field(&content, "thinking"))
            .unwrap_or_default();

        if let Some(tool) = name.clone() {
            // `Task` is *declared* a trajectory here, and it is the only tool that can
            // be. claude forwards a Task delegate's own events under the spawning id, so
            // this block and the child's output share one identity and no correlation
            // table is needed — which also means its `tool_result` is the delegated
            // work's outcome rather than a tool's return value, so it closes the
            // trajectory rather than merely reporting a value.
            //
            // Every *other* delegating tool is discovered rather than declared. This used
            // to be the only path, and it was a guess standing in for a fact that arrives
            // later: a tool call is known to have delegated when a child arrives carrying
            // its id, which is always after the call was published. The name test was
            // right until the vendor shipped `Agent` and `Workflow`, and an MCP server
            // can add another at any time — so the list could never be completed, only
            // extended after each report. Those are promoted by the `span` their children
            // already carry (`docs/session-protocol.md` § Promotion), which needs no list
            // and covers tools nobody has written yet.
            let declared = TrajectoryId::new(stream_id.clone());
            self.tool_names
                .insert(stream_id.clone(), Some(tool.clone()));
            if tool == "Task" {
                self.subagent_tools.insert(declared.clone());
                self.subagent_parent
                    .insert(declared.clone(), parent.clone());
                // **Recorded as opened here, which the Python original did not do.** A
                // declared `Task`'s notifications reuse the spawning id, so the
                // notification path would find the trajectory unopened and emit a *second*
                // `started` under it — the duplicate node its own "started only on first
                // sighting" rule exists to prevent, arriving through the one path that rule
                // did not cover. The classification suite's
                // `TaskDelegationKeepsOneIdentity` could not see it because it populated
                // `subagent_tools` by hand instead of through a real block, so no
                // block-level `started` was ever emitted in that test; driving the
                // declaration through the vendor's own payload is what surfaced it.
                self.opened_trajectories.insert(declared);
                kind = EventKind::Subagent;
            }
        }

        self.blocks.insert(
            BlockKey::for_envelope_tag(host, index),
            OpenBlock {
                kind: kind.clone(),
                id: stream_id.clone(),
                name: name.clone(),
                text: text.clone(),
                // The resolution, recorded once. Every later phase of this block reports
                // the trajectory its `started` claimed rather than re-deriving one that
                // may have changed underneath it.
                span: parent.clone(),
            },
        );

        let mut event = Event::new(kind.clone(), Some(Phase::Started), at_ms);
        event.stream_id = Some(stream_id);
        event.span = parent.map(|span| span.as_str().to_string());
        // A delegation's display name is the kind of agent, not the string "Task" — and
        // when the brief has not arrived, it is nothing at all. See [`task_label`].
        event.name = if kind == EventKind::Subagent {
            task_label(&content)
        } else {
            name
        };
        if kind == EventKind::Subagent {
            // Built-in Task forwards its delegate on stdout. The source is live at
            // declaration time and becomes complete only when the delegated result closes.
            event.interior_state = Some(InteriorState::Partial);
        }
        event.text = text;
        out.push(event);
    }

    fn content_block_delta(
        &mut self,
        nested: &Value,
        host: Option<&HostId>,
        at_ms: i64,
        out: &mut Emission,
    ) {
        let index = index_of(nested);
        let Some(block) = self
            .blocks
            .get_mut(&BlockKey::for_envelope_tag(host, index))
        else {
            return;
        };
        let delta = nested.get("delta").cloned().unwrap_or(Value::Null);
        // Three spellings for one thing, per block type: prose, reasoning, and a tool
        // input arriving as partial JSON. Read in that order because a delta carries
        // exactly one of them.
        let Some(text) = text_field(&delta, "text")
            .or_else(|| text_field(&delta, "thinking"))
            .or_else(|| text_field(&delta, "partial_json"))
        else {
            return;
        };

        block.text.push_str(&text);

        // A trajectory's own partial JSON is its brief arriving a token at a time, which
        // is noise on a card that already names the agent — and it is *not* the
        // delegation's output, which arrives as separately forwarded events. Its
        // `started` and the `delta` carrying the parsed brief hold everything worth
        // showing.
        if block.kind == EventKind::Subagent {
            return;
        }

        let mut event = Event::new(block.kind.clone(), Some(Phase::Delta), at_ms);
        event.stream_id = Some(block.id.clone());
        event.span = block.span.as_ref().map(|span| span.as_str().to_string());
        event.name = block.name.clone();
        event.text = text;
        out.push(event);
    }

    fn content_block_stop(
        &mut self,
        nested: &Value,
        host: Option<&HostId>,
        at_ms: i64,
        out: &mut Emission,
    ) {
        let index = index_of(nested);
        let Some(block) = self.blocks.remove(&BlockKey::for_envelope_tag(host, index)) else {
            return;
        };

        // Tool input arrives as streamed partial JSON, so it is only parseable once the
        // block is closed. A brief that never finished streaming is kept as the raw
        // string rather than discarded: it is what the agent said, and it is the only
        // evidence a reader has about a delegation that died mid-input.
        let data = match block.kind {
            EventKind::ToolCall | EventKind::Subagent => {
                Some(match serde_json::from_str::<Value>(&block.text) {
                    Ok(parsed) => json!({ "input": parsed }),
                    Err(_) => json!({ "input": block.text }),
                })
            }
            _ => None,
        };

        if block.kind == EventKind::Subagent {
            // The trajectory does **not** close here. This block closing means only that
            // the *request* is fully stated; the delegation closes when its `tool_result`
            // arrives, which is when the delegated work actually finished. Reading the
            // phase instead reports every delegation as finished the instant its input
            // parsed (`docs/session-protocol.md` § Promotion).
            //
            // It also carries the real name: the brief is streamed partial JSON, so at
            // `started` there was nothing to read it from, and this is the first point
            // where the agent's own type is known.
            let label = task_label(data.as_ref().unwrap_or(&Value::Null));
            // Overwritten **even when the brief yielded nothing**, deliberately: this
            // entry currently holds the vendor's tool name `"Task"`, recorded above for
            // ordinary tool results, and the `tool_result` closing this trajectory reads
            // it for the close's `name`. Leaving it would put "Task" on the card as
            // though the agent had chosen it, which is the one label this field exists to
            // avoid; `None` correctly says nobody named it.
            self.tool_names.insert(block.id.clone(), label.clone());

            let mut event = Event::new(EventKind::Subagent, Some(Phase::Delta), at_ms);
            event.stream_id = Some(block.id);
            event.span = block.span.as_ref().map(|span| span.as_str().to_string());
            event.name = label;
            event.data = data;
            out.push(event);
            return;
        }

        let mut event = Event::new(block.kind, Some(Phase::Completed), at_ms);
        event.stream_id = Some(block.id);
        event.span = block.span.as_ref().map(|span| span.as_str().to_string());
        event.name = block.name;
        event.text = block.text;
        event.data = data;
        out.push(event);
    }
}

/// What the agent called this delegation, or `None` when it has not said yet.
///
/// "Task" is the tool's name, not the work's: a timeline of six identical rows reading
/// "Task" is unreadable, so the vendor's tool name is deliberately kept out of this
/// field. The brief carries the real name, but it arrives as streamed partial JSON, so at
/// `started` there is usually nothing to read it from and the trajectory is renamed by
/// the `delta` that carries the parsed brief.
///
/// **Returning `None` rather than a placeholder is the point.** `name` means "what this
/// delegation is called", so `None` says nobody knows yet while any string asserts the
/// agent chose it — and that assertion is journaled. The history fold keeps the best name
/// any phase gave a block, so the later `delta` normally overwrites a placeholder and the
/// difference is invisible. It stops being invisible exactly when the brief never
/// finishes streaming: a delegation that fails early, or a link that dies mid-input,
/// leaves the placeholder as the trajectory's durable name in `history.db`,
/// **indistinguishable from a name an agent actually chose, and unrepairable after the
/// fact**. A presentation default belongs at render time, where the client already has
/// one, not in stored data (`docs/session-protocol.md` § `name` is absent when unknown).
fn task_label(content: &Value) -> Option<String> {
    let payload = content.get("input")?;
    if !payload.is_object() {
        // A brief that did not finish streaming parses as a bare string, which is
        // deliberately *not* a name: it is a JSON fragment, and putting `{"subagent_ty`
        // on a card would be the placeholder problem with extra steps.
        return None;
    }
    text_field(payload, "subagent_type").or_else(|| text_field(payload, "description"))
}

impl Correlation {
    /// The trajectory that forwarded output tagged with `host` belongs to.
    ///
    /// Usually `host` itself. The exception is a host that announced a delegate by
    /// `task_id` — `Agent`, and anything else reporting through `task_*` — where the
    /// trajectory is the delegate and the host is only its container. claude still tags
    /// the forwarded output with the *host*, so without this the work lands on the
    /// container while the delegate's own node stays empty, and one delegation reads as
    /// two: a node holding everything and a sibling holding nothing.
    ///
    /// **Only resolved when the host has exactly one delegate.** With several, claude
    /// forwards no interior at all, so there is nothing to attribute and a guess would put
    /// one delegate's work under another — the corruption this whole family of rules
    /// exists to prevent. Attributing to the host is *true*; picking a delegate is
    /// corruption indistinguishable from correct output.
    ///
    /// **The answer is latched.** `Self::host_tasks` grows while the turn runs, so this
    /// question has a different answer before and after `task_started`, and the forwarded
    /// stream does not pause for it. Answering freshly each time splits one delegate's
    /// output across the container and the delegate at whatever instant the notification
    /// landed; both halves render, nested one inside the other, so no layer reports the
    /// split. The first answer therefore stands for the rest of the link.
    ///
    /// A host whose only delegate is *itself* — a `Task`, which claude reports under the
    /// spawning id — resolves to itself either way, and the identity case is still
    /// latched, which is harmless: the latch records the answer, not a claim that a
    /// re-label happened.
    ///
    /// # Measured caveat, and why this is a guard rather than a feature
    ///
    /// On claude 2.1.245 **no host both forwards interior events and multiplexes.**
    /// `--forward-subagent-text` forwards only for the built-in `Task`, whose
    /// notifications reuse the host id and so never populate `Self::host_tasks`; `Agent`
    /// and `Workflow` populate it and forward nothing. So the re-label cannot fire today.
    /// It is kept because the vendor adding forwarding for `Agent` would activate it
    /// silently, and that is the shape a whitelist-shaped bug takes here.
    pub fn delegate_of(&mut self, host: Option<&HostId>) -> Option<TrajectoryId> {
        // The root is never latched: there is nothing to resolve, and an entry for it
        // would be a mapping from "no delegation" to something.
        let host = host?;
        if let Some(latched) = self.host_attribution.get(host) {
            return Some(latched.clone());
        }
        let mut resolved = host.as_trajectory();
        if let Some(tasks) = self.host_tasks.get(host) {
            if tasks.len() == 1 {
                let sole = tasks.iter().next().expect("len() == 1");
                if *sole != resolved {
                    resolved = sole.clone();
                }
            }
        }
        self.host_attribution.insert(host.clone(), resolved.clone());
        Some(resolved)
    }

    /// A vendor `user` payload, which is how tool results arrive.
    ///
    /// The vendor writes a tool result as a synthetic user message, so this is not the
    /// user's own turn text — that never comes back from the CLI at all.
    fn tool_results(&mut self, payload: &Value, at_ms: i64) -> Emission {
        let mut out = Emission::default();

        // `--forward-subagent-text` tags a subagent's own output with the tool call that
        // spawned it. That tag is what lets the timeline nest it, and `delegate_of`
        // resolves it to the delegate when the host is only a container for one.
        let host = text_field(payload, "parent_tool_use_id").map(HostId::new);
        let parent = self.delegate_of(host.as_ref());

        let content = payload
            .get("message")
            .and_then(|message| message.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        // A single block may arrive unwrapped, which is the vendor's own inconsistency
        // rather than a shape this code invents.
        let blocks: Vec<Value> = match content {
            Value::Array(items) => items,
            Value::Null => vec![],
            single => vec![single],
        };

        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            // The fallback is the Python's, kept because a result with no id still
            // happened and dropping it loses a tool's whole output. It cannot collide with
            // a real vendor id, which is a `toolu_`-prefixed token.
            let tool_id = text_field(&block, "tool_use_id")
                .unwrap_or_else(|| "claude-tool-result".to_string());
            let text = flatten(block.get("content").unwrap_or(&Value::Null));
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let trajectory = TrajectoryId::new(tool_id.clone());

            if self.subagent_tools.contains(&trajectory) {
                // The delegated work finished, so this closes the trajectory rather than
                // reporting a tool's return value.
                //
                // **Its span is the trajectory the *spawning* call ran in**, not
                // `parent` — which for a nested `Task` is the child's own id, because the
                // vendor tags the result envelope with the delegation it came from.
                // Reading `parent` here reparents a depth-2 delegation onto itself, and a
                // node that is its own ancestor is unreachable: no attachment pass can
                // root it, so the whole subtree vanishes with nothing reporting it
                // missing.
                let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), at_ms);
                event.stream_id = Some(tool_id.clone());
                event.span = self
                    .subagent_parent
                    .get(&trajectory)
                    .cloned()
                    .flatten()
                    .map(|span| span.as_str().to_string());
                event.name = self.tool_names.get(&tool_id).cloned().flatten();
                event.interior_state = Some(InteriorState::Full);
                event.text = text;
                event.is_error = is_error;
                // So a `task_notification` arriving after the result cannot reopen what
                // this just closed. The declared and notified paths close the same ids,
                // and only a shared record of what is closed keeps them from disagreeing.
                self.closed_trajectories.insert(trajectory);
                out.push(event);
                continue;
            }

            let mut event = Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
            event.stream_id = Some(tool_id.clone());
            // A delegate's own tool results are tagged with the `Task` that spawned it,
            // which is precisely this event's trajectory.
            event.span = parent.as_ref().map(|span| span.as_str().to_string());
            event.name = self.tool_names.get(&tool_id).cloned().flatten();
            event.text = text;
            event.is_error = is_error;
            out.push(event);
        }
        out
    }
}

/// A tool result's content, which arrives as a string or as a list of content blocks.
///
/// Both shapes are real and the vendor picks per tool. A non-string, non-list value is
/// serialized rather than dropped: it is the tool's whole output, and showing JSON beats
/// showing nothing.
fn flatten(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let mut parts = String::new();
            for block in blocks {
                match block {
                    Value::String(text) => parts.push_str(text),
                    Value::Object(_) => {
                        if let Some(text) =
                            text_field(block, "text").or_else(|| text_field(block, "content"))
                        {
                            parts.push_str(&text);
                        }
                    }
                    _ => {}
                }
            }
            parts
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The statuses that close a trajectory.
///
/// `cancelled` closes without an error flag and `error` closes with one — the pair is
/// spelled out because a cancelled delegation coloured red reports an interrupted turn as
/// a broken one (`docs/session-protocol.md` § Outcomes).
const TERMINAL_STATUSES: &[&str] = &["completed", "failed", "cancelled", "error"];

/// Which terminal statuses mean the delegation *failed*.
const FAILED_STATUSES: &[&str] = &["failed", "error"];

/// The `task_*` subtypes claude actually emits.
///
/// **Measured against claude 2.x with `--forward-subagent-text`: these four and no
/// others.** `task_completed` and `task_failed` **do not exist**, and an earlier version
/// of this branch listed them — so nothing ever closed a delegate and every workflow card
/// spun forever. Names guessed from a vendor's vocabulary compile and then silently never
/// fire, and a branch that never fires is indistinguishable from one whose condition is
/// merely rare. `/tmp/probe_task.py` printed one real turn's payloads to settle it.
///
/// The fields each one carries, printed rather than inferred:
///
/// | subtype | fields |
/// | --- | --- |
/// | `task_started` | `task_id`, `tool_use_id`, `description`, `subagent_type`, `prompt`, `spawn_depth`, `task_type`, `is_backgrounded` |
/// | `task_progress` | `task_id`, `tool_use_id`, `description`, `last_tool_name`, `usage` |
/// | `task_updated` | `task_id`, `patch{status, end_time}` — and **no** `tool_use_id` |
/// | `task_notification` | `task_id`, `tool_use_id`, `status`, `summary`, `output_file` |
///
/// So a close arrives two ways: `task_updated` with a terminal `patch.status`, and
/// `task_notification` with a terminal `status` plus the delegate's own `summary`.
const TASK_SUBTYPES: &[&str] = &[
    "task_started",
    "task_progress",
    "task_updated",
    "task_notification",
];

/// Which `task_started` fields travel in an event's `data`.
///
/// An allowlist rather than the whole payload, because `prompt` is the delegation's entire
/// brief — often kilobytes — and it is already the block's own input on the declared path.
/// Journaling it per notification phase would store the same text once per progress
/// update.
const FORWARDED_TASK_FIELDS: &[&str] = &[
    "task_id",
    "subagent_type",
    "tool_use_id",
    "last_tool_name",
    "spawn_depth",
    "task_type",
    "is_backgrounded",
    "output_file",
    "usage",
];

impl Correlation {
    /// A vendor `system` payload.
    fn system(&mut self, payload: &Value, at_ms: i64) -> Emission {
        let mut out = Emission::default();
        let Some(subtype) = payload.get("subtype").and_then(Value::as_str) else {
            return out;
        };

        match subtype {
            "init" => {
                // **`init` arrives once per *turn*, not once per process** — measured, two
                // user messages produce two of them. So this must speak about the
                // permission mode and nothing else.
                //
                // This is where the 46-commands-to-0 bug lived: it answered with a whole
                // capability surface carrying only this one field set, and the server
                // assigned it — erasing the project's 46 slash commands on the first
                // exchange of every session, presenting two layers away as a composer menu
                // that would not open. Naming the facet is what makes the rest survive
                // (`docs/session-protocol.md` § Capability facets).
                let facets = CapabilityFacets {
                    permission_mode: text_field(payload, "permissionMode"),
                    ..Default::default()
                };
                // An update naming no facet is a no-op rather than a clear, so an `init`
                // that omitted the mode must not be published at all — publishing it would
                // be an empty surface reaching the merge.
                if !facets.is_empty() {
                    out.messages.push(SessionMessage::Capabilities(facets));
                }
                // The model can change without anyone asking — a fallback under load, or a
                // `/model` the user typed as text — so the surface is refreshed from what
                // the CLI now reports rather than from what was last set.
                out.observed_model = text_field(payload, "model");
            }
            "commands_changed" => {
                // A skill was edited or a plugin loaded, so the composer's list is stale.
                // The notification carries only that *something* changed, so the surface
                // has to be re-read rather than reconstructed from this payload — which is
                // an RPC, and therefore the session's job rather than this module's.
                out.refresh_capabilities = true;
            }
            "compact_boundary" => {
                let mut event = Event::new(EventKind::Compact, Some(Phase::Completed), at_ms);
                event.text = "Context was compacted".to_string();
                event.data = payload.get("compact_metadata").cloned();
                out.push(event);
            }
            "error" | "api_error" => {
                let mut event = Event::new(EventKind::Status, Some(Phase::Completed), at_ms);
                event.text = text_field(payload, "error")
                    .or_else(|| text_field(payload, "message"))
                    .unwrap_or_else(|| "claude error".to_string());
                event.is_error = true;
                out.push(event);
            }
            "notification" => {
                let mut event = Event::new(EventKind::Notice, None, at_ms);
                event.text = text_field(payload, "message").unwrap_or_default();
                out.push(event);
            }
            _ if TASK_SUBTYPES.contains(&subtype) => {
                self.task_notification(payload, at_ms, &mut out);
            }
            _ => {}
        }
        out
    }

    /// A delegate's lifecycle, reported by the host tool rather than streamed.
    ///
    /// `Task` forwards its delegate's own events, so its trajectory is opened by the tool
    /// block and filled by forwarded output. Every other delegating tool — `Agent`,
    /// `Workflow`, an MCP server's own — reports only these notifications, and they are
    /// then the **whole** record of the delegated work. That is why a closed trajectory
    /// with no children is a real terminal state here rather than a loading one.
    ///
    /// Public so the ported tests can drive one notification at a time, which is how every
    /// defect in this function was originally caught: the classification is decidable from
    /// one payload plus the correlation state, and nothing about it needs a process.
    pub fn task_notification(&mut self, payload: &Value, at_ms: i64, out: &mut Emission) {
        let host = text_field(payload, "tool_use_id").map(HostId::new);

        // **A host that already declared itself a trajectory *is* the delegation.**
        // `Task` publishes its block under this id and claude forwards the child's own
        // events under it too. Minting a second id from `task_id` there splits one
        // delegation into two nodes: the work lands on the declared id and the minted one
        // stays empty, and since nothing ever closes it, it reads as running forever.
        // Measured against a real delegated turn, which is what a live run caught.
        let declared_host = host
            .as_ref()
            .map(HostId::as_trajectory)
            .filter(|id| self.subagent_tools.contains(id));
        let trajectory = match declared_host {
            Some(id) => Some(id),
            None => text_field(payload, "task_id")
                .map(TrajectoryId::new)
                .or_else(|| host.as_ref().map(HostId::as_trajectory)),
        };

        let Some(trajectory) = trajectory else {
            // Nothing addressable. Reported as a plain notice rather than dropped, because
            // a delegation nobody can name still happened.
            let mut event = Event::new(EventKind::Notice, None, at_ms);
            event.text = text_field(payload, "summary")
                .or_else(|| text_field(payload, "subtype"))
                .unwrap_or_default();
            out.push(event);
            return;
        };

        // A late notification must not reopen a closed trajectory: that leaves a second,
        // permanently running node under the same id — the spinner this whole family of
        // rules exists to remove.
        if self.closed_trajectories.contains(&trajectory) {
            return;
        }

        // Remembered from `task_started`, because **neither `task_updated` nor
        // `task_notification` carries `task_type`** — so a close would otherwise be
        // unclassifiable and would open the very node its `started` was filtered out of,
        // and nothing would ever close *that*. See [`TaskKind`] for what the filter is and
        // why an unknown kind is allowed through.
        if let Some(kind) = text_field(payload, "task_type") {
            self.task_kinds
                .insert(trajectory.clone(), TaskKind::parse(&kind));
        }
        // Likewise reported only by `task_started`, and it inverts the server's
        // turn-settle sweep — see [`Correlation::task_backgrounded`].
        if payload
            .get("is_backgrounded")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.task_backgrounded.insert(trajectory.clone());
        }
        if !self
            .task_kinds
            .get(&trajectory)
            .map(TaskKind::is_delegation)
            // An unseen kind nests, which is the same default `TaskKind::Other` gives:
            // a notification whose `started` was never observed is not evidence that it
            // is a shell job.
            .unwrap_or(true)
        {
            return;
        }

        // Which delegates run under this host, recorded on the way in, and its inverse.
        // **`task_updated` carries no `tool_use_id` at all**, so a close cannot be placed
        // in the tree from its own payload — without this it would land at the root while
        // the trajectory it closes sits inside a container, and the container would then
        // never resolve.
        if let Some(host) = host.as_ref().filter(|h| h.as_trajectory() != trajectory) {
            self.host_tasks
                .entry(host.clone())
                .or_default()
                .insert(trajectory.clone());
            self.task_hosts.insert(trajectory.clone(), host.clone());
        }

        // The host's own trajectory, so a delegation inside a delegation closes back into
        // its parent rather than at the root. A host that is not itself a known delegation
        // is its own span, and promotion makes that a trajectory
        // (`docs/session-protocol.md` § Promotion) — which is why no list of delegating
        // tool names is needed here.
        let span: Option<TrajectoryId> = match host.as_ref() {
            Some(host) if host.as_trajectory() != trajectory => Some(host.as_trajectory()),
            _ => self
                .task_hosts
                .get(&trajectory)
                .map(HostId::as_trajectory)
                .or_else(|| self.subagent_parent.get(&trajectory).cloned().flatten()),
        };

        // `description` is a live label, not a stable name: measured as "Read README.md
        // first heading" at `task_started` and "Reading README.md" during progress. The
        // later one is better informed, and the server keeps the latest a phase gave.
        let name =
            text_field(payload, "description").or_else(|| text_field(payload, "subagent_type"));

        let mut data = Map::new();
        if let Some(object) = payload.as_object() {
            for field in FORWARDED_TASK_FIELDS {
                if let Some(value) = object.get(*field) {
                    data.insert((*field).to_string(), value.clone());
                }
            }
        }
        // Carried on **every** phase, not only the `started` that reports it, because the
        // server reads it off whichever row it has. A delegation the vendor says is
        // backgrounded is expected to outlive its turn, so the turn-settle sweep must not
        // close it as abandoned — it will report its own outcome later, and a close written
        // before that arrives contradicts it permanently.
        if self.task_backgrounded.contains(&trajectory) {
            data.insert("is_backgrounded".to_string(), Value::Bool(true));
        }
        // **A status line is a snapshot, so it goes in `data`.** These notifications
        // re-send the whole current label rather than a suffix; appending it as delta text
        // stacked six copies of one sentence in the journal. `text` stays append-only as
        // everywhere else in the protocol.
        if let Some(progress) = name.clone() {
            data.insert("progress".to_string(), Value::String(progress));
        }
        let data = Some(Value::Object(data));

        let status = payload
            .get("patch")
            .and_then(Value::as_object)
            .and_then(|patch| patch.get("status"))
            .and_then(Value::as_str)
            .or_else(|| payload.get("status").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();

        if TERMINAL_STATUSES.contains(&status.as_str()) {
            self.closed_trajectories.insert(trajectory.clone());
            self.opened_trajectories.remove(&trajectory);
            let mut event = Event::new(EventKind::Subagent, Some(Phase::Completed), at_ms);
            event.stream_id = Some(trajectory.as_str().to_string());
            event.span = span.map(|id| id.as_str().to_string());
            event.name = name;
            // `summary` is the delegate's own answer, which only `task_notification`
            // carries — so a close arriving via `task_updated` has no text, and that is
            // honest rather than empty.
            event.text = text_field(payload, "summary").unwrap_or_default();
            event.is_error = FAILED_STATUSES.contains(&status.as_str());
            event.data = data;
            out.push(event);
            return;
        }

        let phase = if self.opened_trajectories.insert(trajectory.clone()) {
            // `started` only on first sighting. A second `started` under one id opens a
            // duplicate node, and these notifications arrive repeatedly for one delegate —
            // which is why progress is a `delta` however it was announced.
            Phase::Started
        } else {
            Phase::Delta
        };
        let mut event = Event::new(EventKind::Subagent, Some(phase), at_ms);
        event.stream_id = Some(trajectory.as_str().to_string());
        event.span = span.map(|id| id.as_str().to_string());
        // No placeholder name: `description`/`subagent_type` is what the agent called this,
        // and when neither has arrived the honest report is silence. A literal here is
        // journaled and, if no later phase carries a real name, becomes the trajectory's
        // durable name — see [`task_label`].
        event.name = name;
        // Agent/Workflow lifecycle notices name work whose interior is expected from the
        // bundle reader. Do not claim it is absent merely because it has not arrived yet.
        event.interior_state = Some(InteriorState::Pending);
        event.data = data;
        out.push(event);
    }

    /// A vendor `result` payload: the turn's own close.
    fn result(&mut self, payload: &Value, at_ms: i64) -> Emission {
        let mut out = Emission::default();
        if let Some(usage) = payload.get("usage") {
            let mut event = Event::new(EventKind::Usage, Some(Phase::Completed), at_ms);
            event.stream_id = Some("claude-usage".to_string());
            event.data = Some(usage.clone());
            out.push(event);
        }
        self.turn_open = false;
        let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), at_ms);
        event.text = text_field(payload, "result").unwrap_or_default();
        event.is_error = payload
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The vendor's own `subtype` is the stop reason here, which is not obvious: a
        // `result` carries `subtype: "success"` or an error variant rather than a field
        // named for the concept.
        event.data = Some(json!({
            "stopReason": payload.get("subtype").cloned().unwrap_or(Value::Null),
            "durationMs": payload.get("duration_ms").cloned().unwrap_or(Value::Null),
            "costUsd": payload.get("total_cost_usd").cloned().unwrap_or(Value::Null),
        }));
        out.push(event);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One link's worth of translation, with the payloads fed in as the vendor sends them.
    ///
    /// Everything goes through [`Correlation::translate`] rather than calling an inner
    /// method, deliberately: the subtype dispatch is itself where two defects lived — the
    /// invented `task_completed`/`task_failed` names, and dropping the envelope's
    /// `parent_tool_use_id` — and a test calling the inner function directly cannot see
    /// either. The Python originals called `_task_notification` and `_stream_event`
    /// directly and so covered neither.
    ///
    /// The clock advances by one per payload, so an assertion can distinguish arrival
    /// order and a fixture cannot accidentally give every event the same stamp — the
    /// timing hazard `AGENTS.md` records under "a fixture's *timing* is part of the
    /// contract".
    struct Link {
        state: Correlation,
        events: Vec<Event>,
        facets: Vec<CapabilityFacets>,
        refreshes: usize,
        models: Vec<String>,
        at: i64,
    }

    impl Link {
        fn new() -> Self {
            Self {
                state: Correlation::new(),
                events: Vec::new(),
                facets: Vec::new(),
                refreshes: 0,
                models: Vec::new(),
                at: 1_700_000_000_000,
            }
        }

        fn feed(&mut self, payload: Value) -> &mut Self {
            self.at += 1;
            let emission = self.state.translate(&payload, self.at);
            for message in emission.messages {
                match message {
                    SessionMessage::Event(event) => self.events.push(event),
                    SessionMessage::Capabilities(facets) => self.facets.push(facets),
                    SessionMessage::Request(_) => {
                        unreachable!("translation never raises a request")
                    }
                    SessionMessage::NativeRecord(_) => {
                        unreachable!("translation does not emit native records")
                    }
                    SessionMessage::TurnInput(_) => {
                        unreachable!("live translation does not emit native input facts")
                    }
                }
            }
            if emission.refresh_capabilities {
                self.refreshes += 1;
            }
            if let Some(model) = emission.observed_model {
                self.models.push(model);
            }
            self
        }

        /// A `system` payload of the given subtype, with the rest of the fields merged in.
        fn system(&mut self, subtype: &str, fields: Value) -> &mut Self {
            let mut payload = json!({ "type": "system", "subtype": subtype });
            merge(&mut payload, fields);
            self.feed(payload)
        }

        /// One nested `stream_event`, tagged with the envelope's `parent_tool_use_id`.
        fn stream(&mut self, host: Option<&str>, nested: Value) -> &mut Self {
            let mut payload = json!({ "type": "stream_event", "event": nested });
            if let Some(host) = host {
                payload["parent_tool_use_id"] = json!(host);
            }
            self.feed(payload)
        }

        /// A tool result, as the vendor's synthetic `user` message.
        fn results(&mut self, host: Option<&str>, blocks: Value) -> &mut Self {
            let mut payload = json!({ "type": "user", "message": { "content": blocks } });
            if let Some(host) = host {
                payload["parent_tool_use_id"] = json!(host);
            }
            self.feed(payload)
        }

        fn of_kind(&self, kind: EventKind) -> Vec<&Event> {
            self.events.iter().filter(|e| e.kind == kind).collect()
        }

        fn trajectories(&self) -> Vec<&Event> {
            self.of_kind(EventKind::Subagent)
        }

        /// Every phase of one trajectory, in arrival order.
        fn phases_of(&self, stream_id: &str) -> Vec<&Event> {
            self.trajectories()
                .into_iter()
                .filter(|e| e.stream_id.as_deref() == Some(stream_id))
                .collect()
        }

        fn phase_list(&self, stream_id: &str) -> Vec<Option<Phase>> {
            self.phases_of(stream_id).iter().map(|e| e.phase).collect()
        }
    }

    fn merge(into: &mut Value, from: Value) {
        if let (Some(target), Value::Object(source)) = (into.as_object_mut(), from) {
            for (key, value) in source {
                target.insert(key, value);
            }
        }
    }

    fn data_of(event: &Event) -> &Map<String, Value> {
        event
            .data
            .as_ref()
            .and_then(Value::as_object)
            .expect("event carries data")
    }

    fn block_start(index: u64, content: Value) -> Value {
        json!({ "type": "content_block_start", "index": index, "content_block": content })
    }

    fn block_delta(index: u64, delta: Value) -> Value {
        json!({ "type": "content_block_delta", "index": index, "delta": delta })
    }

    fn block_stop(index: u64) -> Value {
        json!({ "type": "content_block_stop", "index": index })
    }

    fn tool_result(id: &str, content: &str, is_error: bool) -> Value {
        let mut block = json!({ "type": "tool_result", "tool_use_id": id, "content": content });
        if is_error {
            block["is_error"] = json!(true);
        }
        json!([block])
    }

    // ---- BackgroundedBashIsNotADelegation --------------------------------------

    /// A backgrounded shell job reports through `task_*` too, and `task_type` is the only
    /// thing that says so.
    ///
    /// Measured on claude 2.x: `local_bash` for a backgrounded Bash against `local_agent`
    /// for a real delegation. Treating both as trajectories published a *shell command* as
    /// a subagent — a card named `find / -maxdepth 8 -ipath "*workflows*" …` — and its
    /// error flag escalated the Bash call hosting it, so a turn that had done nothing wrong
    /// read `failed` from its first moment.
    fn backgrounded_bash_started() -> Value {
        json!({
            "task_id": "bg1", "tool_use_id": "toolu_bash",
            "description": "find / -ipath \"*workflows*\"",
            "is_backgrounded": true, "spawn_depth": 1,
            "task_type": "local_bash",
        })
    }

    #[test]
    fn a_backgrounded_bash_opens_no_trajectory() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_bash_started());
        assert!(link.trajectories().is_empty());
    }

    /// The trap this guards: neither `task_updated` nor `task_notification` carries
    /// `task_type`, so a close filtered only at `started` would open the very node its
    /// start was rejected from — and nothing would ever close *that*.
    #[test]
    fn a_backgrounded_bash_close_opens_nothing_either() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_bash_started());
        link.system(
            "task_updated",
            json!({ "task_id": "bg1", "patch": { "status": "completed" } }),
        );
        link.system(
            "task_notification",
            json!({
                "task_id": "bg1", "tool_use_id": "toolu_bash",
                "status": "completed", "summary": "BG_OK",
            }),
        );
        assert!(link.trajectories().is_empty());
        // And nothing else was published in its place: a filtered notification is silent,
        // not a notice.
        assert!(link.events.is_empty(), "{:?}", link.events);
    }

    /// The other half: filtering must not be so broad that it drops delegations.
    #[test]
    fn a_real_delegation_still_opens_one() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "ag1", "tool_use_id": "toolu_agent",
                "description": "Read README.md first heading",
                "subagent_type": "Explore", "task_type": "local_agent",
            }),
        );
        let opened = link.trajectories();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].phase, Some(Phase::Started));
        assert_eq!(opened[0].stream_id.as_deref(), Some("ag1"));
        assert_eq!(opened[0].span.as_deref(), Some("toolu_agent"));
    }

    /// A vendor adding a delegating kind must nest by default. Guessing wrong in this
    /// direction costs a missing subtree; the other direction mislabels a tool call as an
    /// agent, which is what actually shipped.
    #[test]
    fn an_unknown_task_type_is_allowed_through() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "new1", "tool_use_id": "toolu_new",
                "description": "something new", "task_type": "remote_agent",
            }),
        );
        assert_eq!(link.trajectories().len(), 1);
        assert!(TaskKind::parse("remote_agent").is_delegation());
    }

    /// A notification whose `task_started` was never observed nests too. The memo is what
    /// classifies a close, and its absence is not evidence of a shell job.
    #[test]
    fn a_notification_with_no_remembered_kind_nests() {
        let mut link = Link::new();
        link.system(
            "task_progress",
            json!({ "task_id": "orphan", "tool_use_id": "toolu_x", "description": "mid-flight" }),
        );
        assert_eq!(link.trajectories().len(), 1);
    }

    /// The two names that do not exist. Branching on them compiled, never fired, and left
    /// every delegate spinning forever — so the *absence* of a branch is the thing worth
    /// pinning, and only a payload the dispatch rejects can pin it.
    #[test]
    fn the_two_invented_task_subtypes_are_not_recognized() {
        for invented in ["task_completed", "task_failed"] {
            assert!(
                !TASK_SUBTYPES.contains(&invented),
                "{invented} does not exist"
            );
            let mut link = Link::new();
            link.system(
                invented,
                json!({
                    "task_id": "t1", "tool_use_id": "toolu_wf",
                    "status": "completed", "task_type": "local_agent",
                }),
            );
            assert!(
                link.events.is_empty(),
                "{invented} must not be translated as anything"
            );
        }
        // And the four that do exist are all dispatched.
        for real in TASK_SUBTYPES {
            let mut link = Link::new();
            link.system(real, json!({ "task_id": "t1", "tool_use_id": "toolu_wf" }));
            assert!(!link.events.is_empty(), "{real} must reach the projection");
        }
    }

    // ---- BackgroundedDelegationIsFlagged ---------------------------------------

    fn backgrounded_delegation() -> Value {
        json!({
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "long worker", "task_type": "local_agent",
            "is_backgrounded": true,
        })
    }

    #[test]
    fn a_backgrounded_delegation_is_flagged_on_its_opening_event() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_delegation());
        let opened = link.phases_of("t1");
        assert_eq!(data_of(opened[0])["is_backgrounded"], json!(true));
    }

    /// The phase that matters: a progress notification carries no flag, and the server's
    /// turn-settle sweep reads whichever row it finds. Set only at open, every backgrounded
    /// delegation is closed as `abandoned` — a contradiction its real completion cannot
    /// undo, since both records then exist on different turns.
    #[test]
    fn the_backgrounded_flag_is_re_attached_to_later_phases_that_omit_it() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_delegation());
        link.system(
            "task_progress",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "description": "still going" }),
        );
        let deltas: Vec<&Event> = link
            .phases_of("t1")
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Delta))
            .collect();
        assert_eq!(
            data_of(deltas.last().unwrap())["is_backgrounded"],
            json!(true)
        );
    }

    /// The close too, which is the row the sweep is deciding about.
    #[test]
    fn the_backgrounded_flag_reaches_the_close() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_delegation());
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );
        let closed = link.phases_of("t1");
        let last = closed.last().unwrap();
        assert_eq!(last.phase, Some(Phase::Completed));
        assert_eq!(data_of(last)["is_backgrounded"], json!(true));
    }

    /// The exclusion must stay narrow, or an ordinary delegation left open becomes the
    /// permanent spinner the sweep exists to prevent.
    #[test]
    fn a_foreground_delegation_is_not_flagged() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "t2", "tool_use_id": "toolu_wf2",
                "description": "quick worker", "task_type": "local_agent",
            }),
        );
        let opened = link.phases_of("t2");
        assert!(!data_of(opened[0]).contains_key("is_backgrounded"));
    }

    /// The flag travels as a JSON `true`, which is what the server's `json_extract`
    /// comparison depends on. A comparison written against SQLite's integer `1` is the
    /// server's side of this; sending a string `"true"` would break it from here.
    #[test]
    fn the_backgrounded_flag_is_a_json_boolean() {
        let mut link = Link::new();
        link.system("task_started", backgrounded_delegation());
        let opened = link.phases_of("t1");
        assert!(data_of(opened[0])["is_backgrounded"].is_boolean());
    }

    // ---- OneHostManyDelegates --------------------------------------------------

    /// `Workflow` fans out under a single `tool_use_id`.
    ///
    /// Keyed by the host, N delegates collapse into one row where the last name to arrive
    /// wins: a two-agent workflow rendered as one card reading `Demo: agent-2` and the
    /// other agent was absent from the timeline entirely. A fixture with one delegate per
    /// host cannot observe this — at N=1 the collapse is indistinguishable from correct.
    #[test]
    fn two_delegates_under_one_host_are_two_trajectories() {
        let mut link = Link::new();
        for (task, name) in [("t1", "Demo: agent-1"), ("t2", "Demo: agent-2")] {
            link.system(
                "task_started",
                json!({
                    "task_id": task, "tool_use_id": "toolu_wf",
                    "description": name, "task_type": "local_agent",
                }),
            );
        }
        let opened: Vec<&Event> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Started))
            .collect();
        assert_eq!(
            opened
                .iter()
                .map(|e| e.stream_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["t1", "t2"]
        );
        // Both name the host as their container, which promotion turns into a real
        // trajectory without any list of delegating tool names.
        for event in &opened {
            assert_eq!(event.span.as_deref(), Some("toolu_wf"));
        }
        assert_eq!(
            opened
                .iter()
                .map(|e| e.name.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["Demo: agent-1", "Demo: agent-2"]
        );
    }

    /// `task_updated` carries no `tool_use_id` at all, so the host has to be remembered
    /// from `task_started`. Without that the close lands at the root while the trajectory
    /// it closes sits inside a container — leaving the container open, which is the same
    /// spinner one level up.
    #[test]
    fn a_close_with_no_tool_use_id_still_lands_in_the_tree() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );
        let closed: Vec<&Event> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].stream_id.as_deref(), Some("t1"));
        assert_eq!(closed[0].span.as_deref(), Some("toolu_wf"));
    }

    // ---- ProgressIsASnapshot ---------------------------------------------------

    /// These notifications re-send the whole current label, not a suffix.
    ///
    /// Appended as delta text it stacked six copies of one sentence in the journal, so it
    /// travels in `data` — which replaces — leaving `text` append-only as everywhere else
    /// in the protocol.
    #[test]
    fn repeated_progress_does_not_accumulate_text() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "description": "Working on it", "task_type": "local_agent",
            }),
        );
        for _ in 0..3 {
            link.system(
                "task_progress",
                json!({
                    "task_id": "t1", "tool_use_id": "toolu_wf",
                    "description": "Working on it", "last_tool_name": "Read",
                }),
            );
        }
        for event in link.trajectories() {
            assert!(
                event.text.is_empty(),
                "text must stay empty: {:?}",
                event.text
            );
        }
        let deltas: Vec<&Event> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Delta))
            .collect();
        assert_eq!(deltas.len(), 3);
        assert_eq!(
            data_of(deltas.last().unwrap())["progress"],
            json!("Working on it")
        );
    }

    // ---- ClosingIsIdempotent ---------------------------------------------------

    /// A close arrives two ways, and both may arrive: `task_updated` with a terminal
    /// `patch.status`, and `task_notification` with a terminal `status` plus the delegate's
    /// `summary`. Whichever lands first closes it; reopening a closed trajectory is what
    /// leaves a permanently running duplicate.
    fn opened_worker() -> Link {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        link
    }

    #[test]
    fn the_second_close_is_ignored() {
        let mut link = opened_worker();
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );
        link.system(
            "task_notification",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "status": "completed", "summary": "done",
            }),
        );
        let closed: Vec<&Event> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .collect();
        assert_eq!(closed.len(), 1);
    }

    #[test]
    fn late_progress_does_not_reopen_a_closed_trajectory() {
        let mut link = opened_worker();
        link.system(
            "task_notification",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "status": "completed", "summary": "done",
            }),
        );
        let before = link.events.len();
        link.system(
            "task_progress",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "description": "still going" }),
        );
        assert_eq!(link.events.len(), before);
    }

    #[test]
    fn a_failed_status_closes_with_an_error() {
        let mut link = opened_worker();
        link.system(
            "task_notification",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "status": "failed", "summary": "it broke",
            }),
        );
        let closed: Vec<&Event> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .collect();
        assert_eq!(closed.len(), 1);
        assert!(closed[0].is_error);
        assert_eq!(closed[0].text, "it broke");
    }

    /// `cancelled` closes without an error flag. Colouring an interrupted turn red reports
    /// it as a broken one, which is the distinction `abandoned` versus `failed` exists to
    /// keep (`docs/session-protocol.md` § Outcomes).
    #[test]
    fn a_cancelled_delegation_closes_without_an_error_flag() {
        let mut link = opened_worker();
        link.system(
            "task_notification",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "status": "cancelled" }),
        );
        let closed = link.phases_of("t1");
        let last = closed.last().unwrap();
        assert_eq!(last.phase, Some(Phase::Completed));
        assert!(!last.is_error);
    }

    /// A close arriving via `task_updated` has no `summary`, and empty text is the honest
    /// report rather than a fabricated one.
    #[test]
    fn a_close_without_a_summary_carries_no_text() {
        let mut link = opened_worker();
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );
        let last = *link.phases_of("t1").last().unwrap();
        assert_eq!(last.phase, Some(Phase::Completed));
        assert!(last.text.is_empty());
    }

    // ---- TaskDelegationKeepsOneIdentity ----------------------------------------

    /// The built-in `Task` declares its own trajectory and must not get a second.
    ///
    /// claude forwards a `Task` delegate's events under the *spawning* id, so the block and
    /// the child's output share one identity. Minting a second id from `task_id` split one
    /// delegation into two nodes: the work landed on the declared id and the minted one
    /// stayed empty, never closing — which a live run caught as a trajectory that was
    /// opened and then left with nothing in it.
    #[test]
    fn a_notification_for_a_declared_task_reuses_its_id() {
        let mut link = Link::new();
        // Declared the way the vendor declares it — through a real `Task` block, not by
        // reaching into the correlation state, so the declaration path is under test too.
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_task", "name": "Task" }),
            ),
        );
        link.system(
            "task_started",
            json!({
                "task_id": "inner", "tool_use_id": "toolu_task",
                "description": "Explore", "task_type": "local_agent",
            }),
        );
        let opened: Vec<&str> = link
            .trajectories()
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Started))
            .map(|e| e.stream_id.as_deref().unwrap())
            .collect();
        // **Exactly one `started`, and this is the assertion the Python could not make.**
        // Its version of this test populated `subagent_tools` by hand, so the declared
        // block never emitted a `started` and the notification's own one was the only
        // event — hiding that the two paths both open. Driven through the vendor's real
        // payload, a second `started` here would be a duplicate node under one id.
        assert_eq!(opened, ["toolu_task"]);
        // The notification still reaches the trajectory, as a `delta`: it carries the
        // delegate's live label, which is worth keeping.
        assert_eq!(
            link.phase_list("toolu_task"),
            [Some(Phase::Started), Some(Phase::Delta)]
        );
        // And nothing was minted from `task_id`, which is the node that used to spin.
        assert!(link.phases_of("inner").is_empty());
    }

    #[test]
    fn a_forwarded_task_is_partial_until_its_result_closes() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_task_state", "name": "Task" }),
            ),
        );
        let declaration = link
            .phases_of("toolu_task_state")
            .into_iter()
            .next()
            .expect("Task declaration");
        assert_eq!(declaration.interior_state, Some(InteriorState::Partial));

        link.results(None, tool_result("toolu_task_state", "done", false));
        let close = link
            .phases_of("toolu_task_state")
            .into_iter()
            .last()
            .expect("Task close");
        assert_eq!(close.interior_state, Some(InteriorState::Full));
    }

    // ---- ForwardedOutputFindsItsDelegate ---------------------------------------

    /// A host that announced one delegate is only its container.
    ///
    /// claude tags forwarded output with the *host*, so without re-tagging the work lands on
    /// the container while the delegate's own node stays empty — one delegation reading as
    /// two, one of them spinning. Only resolvable when the host has exactly one delegate;
    /// with several, claude forwards no interior at all, and guessing would put one
    /// delegate's work under another.
    #[test]
    fn output_is_attributed_to_a_lone_delegate() {
        let mut link = opened_worker();
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("t1"))
        );
    }

    #[test]
    fn a_multiplexing_host_keeps_its_own_id() {
        let mut link = Link::new();
        for task in ["t1", "t2"] {
            link.system(
                "task_started",
                json!({
                    "task_id": task, "tool_use_id": "toolu_wf",
                    "description": task, "task_type": "local_agent",
                }),
            );
        }
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("toolu_wf"))
        );
    }

    // ---- AttributionIsLatchedPerHost -------------------------------------------

    /// `delegate_of` reads a table that grows while the turn runs, so the same host has one
    /// answer before `task_started` and another after it — and the forwarded stream does not
    /// pause in between. Asked freshly each time, one delegate's output is split across the
    /// container and the delegate at whatever instant the notification happened to land, and
    /// *both halves render*, nested one inside the other, so no layer reports the split.
    #[test]
    fn an_attribution_given_before_the_delegate_arrived_stands() {
        let mut link = Link::new();
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("toolu_wf"))
        );
        link.system(
            "task_started",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("toolu_wf")),
            "the answer already published must stand"
        );
    }

    #[test]
    fn a_delegate_known_first_is_used_for_the_whole_stream() {
        let mut link = opened_worker();
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("t1"))
        );
        // A second delegate would make the host multiplexing, but the stream already
        // committed: re-deciding now would reparent output already published.
        link.system(
            "task_started",
            json!({
                "task_id": "t2", "tool_use_id": "toolu_wf",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_wf"))),
            Some(TrajectoryId::new("t1"))
        );
    }

    /// A `Task` reports its notifications under the *spawning* id, so host and trajectory
    /// are one id in both roles. The identity case must resolve to itself.
    #[test]
    fn a_host_whose_sole_delegate_is_itself_resolves_to_itself() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_task", "name": "Task" }),
            ),
        );
        link.system(
            "task_started",
            json!({
                "task_id": "inner", "tool_use_id": "toolu_task",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        assert_eq!(
            link.state.delegate_of(Some(&HostId::new("toolu_task"))),
            Some(TrajectoryId::new("toolu_task"))
        );
    }

    /// The root has nothing to resolve, so it is never latched — an entry for it would be a
    /// mapping from "no delegation" to something.
    #[test]
    fn the_root_is_never_latched() {
        let mut link = Link::new();
        assert_eq!(link.state.delegate_of(None), None);
        assert!(link.state.host_attribution.is_empty());
    }

    // ---- AForwardedBlockSurvivesANotification ----------------------------------

    /// A block's three phases must key on the envelope tag, not on the resolution.
    ///
    /// `parent_tool_use_id` is the only part claude guarantees is identical across
    /// `content_block_start`, `_delta` and `_stop`. Keyed by the trajectory that tag
    /// resolves to, a `task_started` landing mid-block moves the key out from under an open
    /// block: the delta looks up a key that no longer exists and returns, the stop misses
    /// too, and no `completed` is ever emitted. The block's whole text is dropped and its
    /// node stays open, with nothing at any layer reporting it.
    #[test]
    fn a_delta_arriving_after_a_task_started_still_lands() {
        let mut link = Link::new();
        link.stream(
            Some("toolu_wf"),
            block_start(0, json!({ "type": "text", "text": "" })),
        );
        // The notification that used to move the key.
        link.system(
            "task_started",
            json!({
                "task_id": "t1", "tool_use_id": "toolu_wf",
                "description": "worker", "task_type": "local_agent",
            }),
        );
        link.stream(
            Some("toolu_wf"),
            block_delta(0, json!({ "text": "half a sentence" })),
        );
        link.stream(Some("toolu_wf"), block_stop(0));

        let assistant = link.of_kind(EventKind::Assistant);
        assert_eq!(
            assistant.iter().map(|e| e.phase).collect::<Vec<_>>(),
            [
                Some(Phase::Started),
                Some(Phase::Delta),
                Some(Phase::Completed)
            ]
        );
        assert_eq!(assistant.last().unwrap().text, "half a sentence");
        // And it stays in the trajectory its `started` claimed, rather than moving to the
        // delegate partway through one block.
        for event in &assistant {
            assert_eq!(event.span.as_deref(), Some("toolu_wf"));
        }
    }

    /// The original keying bug, still pinned: two hosts both stream index 0. One
    /// index-keyed map made each one's deltas append to the other's text — corruption, not
    /// a layout glitch, and invisible until two delegates run at once.
    #[test]
    fn concurrent_delegates_keep_their_own_index_zero() {
        let mut link = Link::new();
        for host in ["toolu_a", "toolu_b"] {
            link.stream(
                Some(host),
                block_start(0, json!({ "type": "text", "text": "" })),
            );
        }
        for (host, text) in [("toolu_a", "from A"), ("toolu_b", "from B")] {
            link.stream(Some(host), block_delta(0, json!({ "text": text })));
        }
        for host in ["toolu_a", "toolu_b"] {
            link.stream(Some(host), block_stop(0));
        }

        let mut done: Vec<(&str, &str)> = link
            .of_kind(EventKind::Assistant)
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .map(|e| (e.span.as_deref().unwrap(), e.text.as_str()))
            .collect();
        done.sort();
        assert_eq!(done, [("toolu_a", "from A"), ("toolu_b", "from B")]);
    }

    /// The root agent's own blocks share the index space too, and must not collide with a
    /// delegate's. The Python keyed `None` for the root, which this pins from the other
    /// side: three hosts, one index.
    #[test]
    fn the_root_stream_does_not_collide_with_a_delegates_index() {
        let mut link = Link::new();
        for host in [None, Some("toolu_a")] {
            link.stream(host, block_start(0, json!({ "type": "text", "text": "" })));
        }
        link.stream(None, block_delta(0, json!({ "text": "root" })));
        link.stream(
            Some("toolu_a"),
            block_delta(0, json!({ "text": "delegate" })),
        );
        for host in [None, Some("toolu_a")] {
            link.stream(host, block_stop(0));
        }

        let mut done: Vec<(Option<&str>, &str)> = link
            .of_kind(EventKind::Assistant)
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .map(|e| (e.span.as_deref(), e.text.as_str()))
            .collect();
        done.sort();
        assert_eq!(done, [(None, "root"), (Some("toolu_a"), "delegate")]);
    }

    // ---- NestedTrajectoriesCarrySpanOnEveryPhase -------------------------------

    /// A nested `subagent` must name its parent on **every** phase, not only on open.
    ///
    /// The history fold folds a declared trajectory's phases into one row keyed by its
    /// close, and it carries `name` forward but **not** `span`. So an adapter that states
    /// the parent only on `started` nests correctly while the turn is live and re-roots
    /// itself beside its parent the moment the turn settles — the delegated subtree flattens
    /// on reload, which is the pre-`span` bug arriving via the storage tier. Both of this
    /// adapter's two paths to a `subagent` event are pinned, because the older specs
    /// asserted only that a close lands in the tree, which passes even if an intermediate
    /// phase drops the pointer.
    #[test]
    fn a_nested_declared_task_names_its_parent_throughout() {
        let mut link = Link::new();
        // The outer Task, at the root.
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_outer", "name": "Task" }),
            ),
        );
        // The inner one, declared inside it — the envelope tags it with the outer id.
        link.stream(
            Some("toolu_outer"),
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_inner", "name": "Task" }),
            ),
        );
        link.stream(
            Some("toolu_outer"),
            block_delta(
                0,
                json!({ "partial_json": "{\"subagent_type\":\"Explore\"}" }),
            ),
        );
        link.stream(Some("toolu_outer"), block_stop(0));
        // Its `tool_result` is the delegated work's outcome, so it closes it.
        link.results(
            Some("toolu_outer"),
            tool_result("toolu_inner", "done", false),
        );

        assert_eq!(
            link.phase_list("toolu_inner"),
            [
                Some(Phase::Started),
                Some(Phase::Delta),
                Some(Phase::Completed)
            ]
        );
        for phase in link.phases_of("toolu_inner") {
            assert_eq!(
                phase.span.as_deref(),
                Some("toolu_outer"),
                "{:?} dropped the pointer",
                phase.phase
            );
        }
        // The root turn is spelled by an *absent* span, not a null one.
        assert_eq!(link.phases_of("toolu_outer")[0].span, None);
    }

    #[test]
    fn a_notified_delegation_names_its_host_throughout() {
        let mut link = opened_worker();
        link.system(
            "task_progress",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "description": "still going" }),
        );
        // The close carries no `tool_use_id` at all, so the host has to be remembered — and
        // it has to reach this phase, not merely be known internally.
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );

        assert_eq!(
            link.phase_list("t1"),
            [
                Some(Phase::Started),
                Some(Phase::Delta),
                Some(Phase::Completed)
            ]
        );
        for phase in link.phases_of("t1") {
            assert_eq!(phase.span.as_deref(), Some("toolu_wf"), "{:?}", phase.phase);
        }
    }

    /// A nested `Task`'s close reads `subagent_parent`, not the envelope's resolution —
    /// which for a nested Task is the child's own id. Reading the wrong one reparents a
    /// depth-2 delegation onto itself, and a node that is its own ancestor is unreachable:
    /// the whole subtree vanishes with nothing reporting it missing.
    #[test]
    fn a_nested_task_closes_into_its_parent_and_not_onto_itself() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_outer", "name": "Task" }),
            ),
        );
        link.stream(
            Some("toolu_outer"),
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_inner", "name": "Task" }),
            ),
        );
        link.stream(Some("toolu_outer"), block_stop(0));
        // The result envelope is tagged with the *inner* delegation, which is what the
        // vendor does once the child is running — and it is precisely the value that must
        // not become the close's span.
        link.results(
            Some("toolu_inner"),
            tool_result("toolu_inner", "done", false),
        );

        let closed = *link.phases_of("toolu_inner").last().unwrap();
        assert_eq!(closed.phase, Some(Phase::Completed));
        assert_eq!(closed.span.as_deref(), Some("toolu_outer"));
        assert_ne!(closed.span.as_deref(), closed.stream_id.as_deref());
    }

    // ---- AnUnnamedTrajectoryStaysUnnamed ---------------------------------------

    /// `name` is what the agent called this, so a guess must not be sent as one.
    ///
    /// `None` says nobody knows yet; a literal asserts the agent chose it. The assertion is
    /// journaled, and the history fold keeps the best name any phase gave a block — so a
    /// later phase carrying the real brief normally overwrites a placeholder and the
    /// difference never shows. It shows exactly when the brief never finishes streaming: a
    /// delegation that fails early, or a link that dies mid-input, leaves the placeholder as
    /// the trajectory's durable name, indistinguishable from one an agent chose and
    /// unrepairable after the fact.
    #[test]
    fn a_notified_delegation_with_no_description_sends_no_name() {
        let mut link = Link::new();
        link.system(
            "task_started",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "task_type": "local_agent" }),
        );
        let opened = link.phases_of("t1");
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].name, None);
    }

    /// The realistic failure: the input is streamed partial JSON, so a delegation that dies
    /// mid-brief has nothing to be named from at any phase. Nothing here may fill that in —
    /// least of all the vendor's tool name `"Task"`, which this field exists to keep off
    /// the card.
    #[test]
    fn a_declared_task_whose_brief_never_parses_sends_no_name() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_x", "name": "Task" }),
            ),
        );
        link.stream(
            None,
            block_delta(0, json!({ "partial_json": "{\"subagent_ty" })),
        );
        link.stream(None, block_stop(0));
        link.results(None, tool_result("toolu_x", "died", true));

        assert_eq!(
            link.phase_list("toolu_x"),
            [
                Some(Phase::Started),
                Some(Phase::Delta),
                Some(Phase::Completed)
            ]
        );
        for phase in link.phases_of("toolu_x") {
            assert_eq!(
                phase.name, None,
                "{:?} named an unnamed delegation",
                phase.phase
            );
        }
        assert!(link.phases_of("toolu_x").last().unwrap().is_error);
    }

    /// The subtraction must not cost the names that do arrive.
    #[test]
    fn a_real_name_is_reported_on_every_phase_that_has_one() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_y", "name": "Task" }),
            ),
        );
        link.stream(
            None,
            block_delta(
                0,
                json!({ "partial_json": "{\"subagent_type\":\"Explore\"}" }),
            ),
        );
        link.stream(None, block_stop(0));
        link.results(None, tool_result("toolu_y", "found it", false));

        assert_eq!(
            link.phases_of("toolu_y")
                .iter()
                .map(|e| e.name.as_deref())
                .collect::<Vec<_>>(),
            [None, Some("Explore"), Some("Explore")]
        );
    }

    /// The vendor's tool name must never be the delegation's name, at any phase. This is the
    /// one assertion that fails if `tool_names` stops being overwritten at
    /// `content_block_stop`, which is a subtraction easy to read as redundant.
    #[test]
    fn the_vendors_tool_name_never_becomes_a_delegations_name() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_z", "name": "Task" }),
            ),
        );
        link.stream(None, block_stop(0));
        link.results(None, tool_result("toolu_z", "done", false));
        for phase in link.phases_of("toolu_z") {
            assert_ne!(phase.name.as_deref(), Some("Task"), "{:?}", phase.phase);
        }
    }

    /// An ordinary tool result still gets its name from the call, which is the entry the
    /// delegation path overwrites. Without this, the subtraction above could be
    /// over-applied and no tool result would be named at all.
    #[test]
    fn an_ordinary_tool_result_is_named_from_its_call() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_r", "name": "Read" }),
            ),
        );
        link.stream(None, block_stop(0));
        link.results(None, tool_result("toolu_r", "file contents", false));
        let results = link.of_kind(EventKind::ToolResult);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_deref(), Some("Read"));
        assert_eq!(results[0].text, "file contents");
    }

    // ---- UnaddressableNotification ---------------------------------------------

    /// A notification with no id at all still happened, so it becomes a notice rather than
    /// being dropped.
    #[test]
    fn an_unaddressable_notification_becomes_a_notice() {
        let mut link = Link::new();
        link.system("task_progress", json!({ "summary": "something" }));
        assert_eq!(
            link.events
                .iter()
                .map(|e| e.kind.clone())
                .collect::<Vec<_>>(),
            [EventKind::Notice]
        );
        assert_eq!(link.events[0].text, "something");
    }

    // ---- The turn bracket, and what a subagent may not open --------------------

    /// Only the root agent's stream bounds a turn. A delegate's `message_start` opening one
    /// makes a single exchange report two turns, the second of which the server then has to
    /// close.
    #[test]
    fn a_subagents_message_does_not_open_the_turn() {
        let mut link = Link::new();
        link.stream(
            Some("toolu_wf"),
            json!({ "type": "message_start", "message": { "id": "m1" } }),
        );
        assert!(link.of_kind(EventKind::Turn).is_empty());
        assert!(!link.state.turn_is_open());

        link.stream(
            None,
            json!({ "type": "message_start", "message": { "id": "m2" } }),
        );
        let turns = link.of_kind(EventKind::Turn);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].phase, Some(Phase::Started));
        assert!(link.state.turn_is_open());
    }

    /// A second root message inside one turn must not open another.
    #[test]
    fn a_second_root_message_does_not_open_a_second_turn() {
        let mut link = Link::new();
        for id in ["m1", "m2"] {
            link.stream(
                None,
                json!({ "type": "message_start", "message": { "id": id } }),
            );
        }
        assert_eq!(link.of_kind(EventKind::Turn).len(), 1);
    }

    /// A `result` closes the turn and reports usage separately, so a client can render cost
    /// without parsing the turn's own payload.
    #[test]
    fn a_result_closes_the_turn_and_reports_usage() {
        let mut link = Link::new();
        link.stream(
            None,
            json!({ "type": "message_start", "message": { "id": "m1" } }),
        );
        link.feed(json!({
            "type": "result", "subtype": "success", "result": "all done",
            "duration_ms": 1234, "total_cost_usd": 0.5,
            "usage": { "input_tokens": 10, "output_tokens": 20 },
        }));

        assert!(!link.state.turn_is_open());
        let usage = link.of_kind(EventKind::Usage);
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].data.as_ref().unwrap()["input_tokens"], json!(10));

        let closed: Vec<&Event> = link
            .of_kind(EventKind::Turn)
            .into_iter()
            .filter(|e| e.phase == Some(Phase::Completed))
            .collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].text, "all done");
        assert!(!closed[0].is_error);
        // The vendor's `subtype` *is* the stop reason, which is not obvious from the name.
        assert_eq!(data_of(closed[0])["stopReason"], json!("success"));
        assert_eq!(data_of(closed[0])["durationMs"], json!(1234));
    }

    // ---- system/init, and the facet that must stay scoped ----------------------

    /// `init` arrives once per **turn**, not once per process. Answering it with a
    /// whole-surface payload carrying only the permission mode erased the project's 46 slash
    /// commands on the first exchange of every session, surfacing two layers away as a
    /// composer menu that would not open.
    #[test]
    fn init_speaks_only_about_the_permission_mode() {
        let mut link = Link::new();
        link.system(
            "init",
            json!({ "permissionMode": "default", "model": "claude-opus-5" }),
        );
        assert_eq!(link.facets.len(), 1);
        let facets = &link.facets[0];
        assert_eq!(facets.permission_mode.as_deref(), Some("default"));
        // Every other facet unmentioned, so the server's merge leaves it alone. This is the
        // whole fix for the 46-commands bug, and it is one assertion per facet because a
        // facet added later must not silently start being cleared.
        assert!(facets.commands.is_none(), "commands must not be mentioned");
        assert!(
            facets.subagents.is_none(),
            "subagents must not be mentioned"
        );
        assert!(facets.config.is_none(), "config must not be mentioned");
        assert!(facets.context_window.is_none());
        // The model is reported for the session to fold into a config surface, never
        // published as a facet from here — a config array built from one field would be the
        // same erasure by another route.
        assert_eq!(link.models, ["claude-opus-5"]);
    }

    /// An `init` with no mode publishes nothing: an update naming no facet is a no-op rather
    /// than a clear, and sending an empty one would put a surface with no facets through the
    /// merge.
    #[test]
    fn an_init_with_no_mode_publishes_no_facets() {
        let mut link = Link::new();
        link.system("init", json!({ "model": "claude-opus-5" }));
        assert!(link.facets.is_empty());
        assert_eq!(link.models, ["claude-opus-5"]);
    }

    /// `commands_changed` carries only that *something* changed, so the surface has to be
    /// re-read rather than reconstructed from the notification.
    #[test]
    fn commands_changed_asks_for_a_reread_rather_than_publishing_a_surface() {
        let mut link = Link::new();
        link.system("commands_changed", json!({}));
        assert_eq!(link.refreshes, 1);
        assert!(link.facets.is_empty(), "the payload is not a surface");
        assert!(link.events.is_empty());
    }

    // ---- Content projection ----------------------------------------------------

    /// Thinking and prose are separate kinds, and a block with no id synthesizes one from
    /// the message id so its phases still correlate.
    #[test]
    fn thinking_and_prose_are_projected_as_their_own_kinds() {
        let mut link = Link::new();
        link.stream(
            None,
            json!({ "type": "message_start", "message": { "id": "msg_1" } }),
        );
        link.stream(
            None,
            block_start(0, json!({ "type": "thinking", "thinking": "" })),
        );
        link.stream(None, block_delta(0, json!({ "thinking": "hmm" })));
        link.stream(None, block_stop(0));
        link.stream(None, block_start(1, json!({ "type": "text", "text": "" })));
        link.stream(None, block_delta(1, json!({ "text": "answer" })));
        link.stream(None, block_stop(1));

        let thinking = link.of_kind(EventKind::Thinking);
        assert_eq!(thinking.last().unwrap().text, "hmm");
        // Synthesized from the vendor's message id and the block index, so two blocks of one
        // message cannot share a `stream_id`.
        assert_eq!(thinking[0].stream_id.as_deref(), Some("msg_1:0"));
        let prose = link.of_kind(EventKind::Assistant);
        assert_eq!(prose.last().unwrap().text, "answer");
        assert_eq!(prose[0].stream_id.as_deref(), Some("msg_1:1"));
    }

    /// A tool call's arguments arrive as partial JSON and are parsed at its close, so the
    /// call's `data.input` is an object rather than a string.
    #[test]
    fn a_tool_calls_arguments_are_parsed_at_its_close() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_1", "name": "Read" }),
            ),
        );
        link.stream(
            None,
            block_delta(0, json!({ "partial_json": "{\"file_path\":" })),
        );
        link.stream(
            None,
            block_delta(0, json!({ "partial_json": "\"/tmp/x\"}" })),
        );
        link.stream(None, block_stop(0));

        let calls = link.of_kind(EventKind::ToolCall);
        let closed = calls.last().unwrap();
        assert_eq!(closed.phase, Some(Phase::Completed));
        assert_eq!(data_of(closed)["input"]["file_path"], json!("/tmp/x"));
        assert_eq!(closed.name.as_deref(), Some("Read"));
    }

    /// A brief that never finished parsing is kept verbatim rather than discarded: it is what
    /// the agent said, and it is the only evidence about a delegation that died mid-input.
    #[test]
    fn an_unparseable_tool_input_survives_as_its_raw_text() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_1", "name": "Read" }),
            ),
        );
        link.stream(
            None,
            block_delta(0, json!({ "partial_json": "{\"file_pa" })),
        );
        link.stream(None, block_stop(0));
        let closed = *link.of_kind(EventKind::ToolCall).last().unwrap();
        assert_eq!(data_of(closed)["input"], json!("{\"file_pa"));
    }

    /// A `subagent` block's own partial JSON is suppressed: it is the brief arriving a token
    /// at a time, which is noise on a card that already names the agent, and it is not the
    /// delegation's output.
    #[test]
    fn a_declared_trajectorys_partial_json_is_not_streamed_as_deltas() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_t", "name": "Task" }),
            ),
        );
        for fragment in ["{\"subagent_", "type\":\"Explore\"}"] {
            link.stream(None, block_delta(0, json!({ "partial_json": fragment })));
        }
        // Exactly one event so far — the `started` — and no delta per fragment.
        assert_eq!(link.phase_list("toolu_t"), [Some(Phase::Started)]);
        link.stream(None, block_stop(0));
        // The close of the *block* is a `delta` carrying the parsed brief, not a `completed`:
        // the trajectory closes on its `tool_result`, when the work actually finished.
        assert_eq!(
            link.phase_list("toolu_t"),
            [Some(Phase::Started), Some(Phase::Delta)]
        );
        let named = *link.phases_of("toolu_t").last().unwrap();
        assert_eq!(named.name.as_deref(), Some("Explore"));
    }

    /// A block type this vocabulary does not model is not projected. Inventing an event kind
    /// is what `crates/adapters/src/lib.rs` forbids an adapter from doing.
    #[test]
    fn an_unmodelled_content_block_type_is_not_projected() {
        let mut link = Link::new();
        link.stream(None, block_start(0, json!({ "type": "redacted_thinking" })));
        link.stream(None, block_delta(0, json!({ "text": "ignored" })));
        link.stream(None, block_stop(0));
        assert!(link.events.is_empty(), "{:?}", link.events);
    }

    /// A delta or stop for a block that was never opened is a no-op rather than a
    /// fabricated block. This is the state a link re-attached mid-turn is in.
    #[test]
    fn a_delta_for_an_unopened_block_is_ignored() {
        let mut link = Link::new();
        link.stream(None, block_delta(0, json!({ "text": "orphan" })));
        link.stream(None, block_stop(0));
        assert!(link.events.is_empty());
    }

    // ---- flatten ---------------------------------------------------------------

    /// Tool results arrive as a string or as a list of content blocks, and the vendor picks
    /// per tool.
    #[test]
    fn a_tool_results_content_is_flattened_from_either_shape() {
        assert_eq!(flatten(&json!("plain")), "plain");
        assert_eq!(
            flatten(&json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }])),
            "ab"
        );
        assert_eq!(flatten(&json!(["a", { "content": "b" }])), "ab");
        assert_eq!(flatten(&Value::Null), "");
        // A shape neither of those is serialized rather than dropped: it is the tool's whole
        // output, and JSON beats nothing.
        assert_eq!(flatten(&json!({ "unexpected": 1 })), "{\"unexpected\":1}");
    }

    /// An unwrapped single block is a real vendor shape rather than one this code invents.
    #[test]
    fn a_single_unwrapped_result_block_is_still_read() {
        let mut link = Link::new();
        link.feed(json!({
            "type": "user",
            "message": { "content": { "type": "tool_result", "tool_use_id": "t", "content": "x" } },
        }));
        let results = link.of_kind(EventKind::ToolResult);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "x");
    }

    // ---- task_label ------------------------------------------------------------

    /// `subagent_type` wins over `description`, and neither present is `None` rather than a
    /// placeholder.
    #[test]
    fn a_task_label_prefers_the_agent_type_and_never_invents_one() {
        assert_eq!(
            task_label(&json!({ "input": { "subagent_type": "Explore", "description": "d" } })),
            Some("Explore".to_string())
        );
        assert_eq!(
            task_label(&json!({ "input": { "description": "d" } })),
            Some("d".to_string())
        );
        assert_eq!(task_label(&json!({ "input": {} })), None);
        assert_eq!(task_label(&json!({})), None);
        // A brief that did not finish parsing is a bare string, which is a JSON fragment
        // rather than a name — putting `{"subagent_ty` on a card is the placeholder problem
        // with extra steps.
        assert_eq!(task_label(&json!({ "input": "{\"subagent_ty" })), None);
        // An empty string is absent, not a name.
        assert_eq!(
            task_label(&json!({ "input": { "subagent_type": "" } })),
            None
        );
    }

    // ---- Stability, which structure depends on --------------------------------

    /// A `subagent` event is stable at **every** phase, and this file's job is to construct
    /// events through the one constructor that derives it. Built any other way, a
    /// trajectory's `started` is published and silently not journaled, and a reload flattens
    /// the whole delegated subtree onto the root.
    #[test]
    fn every_trajectory_phase_this_module_emits_is_stable() {
        let mut link = opened_worker();
        link.system(
            "task_progress",
            json!({ "task_id": "t1", "tool_use_id": "toolu_wf", "description": "going" }),
        );
        link.system(
            "task_updated",
            json!({ "task_id": "t1", "patch": { "status": "completed" } }),
        );
        for phase in link.phases_of("t1") {
            assert!(
                phase.is_stable(),
                "{:?} must reach the journal",
                phase.phase
            );
        }
    }

    /// And the declared path too, whose `started` is where the trajectory's parent lives.
    #[test]
    fn a_declared_trajectorys_phases_are_stable_as_well() {
        let mut link = Link::new();
        link.stream(
            None,
            block_start(
                0,
                json!({ "type": "tool_use", "id": "toolu_t", "name": "Task" }),
            ),
        );
        link.stream(
            None,
            block_delta(
                0,
                json!({ "partial_json": "{\"subagent_type\":\"Explore\"}" }),
            ),
        );
        link.stream(None, block_stop(0));
        link.results(None, tool_result("toolu_t", "done", false));
        for phase in link.phases_of("toolu_t") {
            assert!(phase.is_stable(), "{:?}", phase.phase);
        }
    }

    // ---- Errors and unknown payloads ------------------------------------------

    #[test]
    fn a_vendor_error_becomes_a_failed_status() {
        let mut link = Link::new();
        link.system("error", json!({ "error": "rate limited" }));
        let status = link.of_kind(EventKind::Status);
        assert_eq!(status.len(), 1);
        assert!(status[0].is_error);
        assert_eq!(status[0].text, "rate limited");
    }

    #[test]
    fn a_compact_boundary_is_recorded_with_its_metadata() {
        let mut link = Link::new();
        link.system(
            "compact_boundary",
            json!({ "compact_metadata": { "trigger": "auto" } }),
        );
        let compact = link.of_kind(EventKind::Compact);
        assert_eq!(compact.len(), 1);
        assert_eq!(compact[0].data.as_ref().unwrap()["trigger"], json!("auto"));
    }

    /// An unknown payload type must not fail a turn: the vendor adds them between releases,
    /// and one unrecognized line is not a reason to lose the rest of the stream.
    #[test]
    fn an_unknown_payload_type_is_a_no_op() {
        let mut link = Link::new();
        link.feed(json!({ "type": "something_new", "detail": 1 }));
        link.feed(json!({ "no_type_at_all": true }));
        assert!(link.events.is_empty());
        assert!(link.facets.is_empty());
        assert_eq!(link.refreshes, 0);
    }
}
