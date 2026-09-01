//! The adapter contract: one trait, three modes, one vocabulary out of all of them.
//!
//! `docs/session-protocol.md` §§ An adapter is a trait, with three modes and What the
//! adapter owns, and what it must not are authoritative.
//!
//! **Declarations only.** The implementations are step 2 of `docs/roadmap.md`
//! § Implementation order.

use std::collections::BTreeMap;
use std::pin::Pin;

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::{
    CapabilityFacets, ConfigOption, Event, Fidelity, FlagSchema, NativeId, Request, Resolution,
    SessionCapabilities,
};

/// What the local CLI reports about itself, plus what this module knows about it.
///
/// This is what [`Adapter::probe`] returns: the answer to "is this agent usable here, and
/// what does it offer". Availability itself is just whether the vendor CLI is on this
/// machine's `PATH` (`docs/agents.md`), so there is no descriptor to discover and no path
/// to record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// The agent's name, matching what a [`crate::SessionId`] carries.
    pub name: String,
    /// The CLI's own version, when it reported one.
    ///
    /// `Option`, because absent is a real state and **never synthesized**: a fabricated
    /// version is a lie a journal makes permanent, and it would make an unsupported
    /// install look supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Spawn-time options: what can be chosen before a session exists.
    ///
    /// Distinct from [`ConfigOption`], which is what is adjustable on a session that
    /// already exists. Nothing in the UI hardcodes a model list, which is why `codex`
    /// correctly shows effort with no model selector.
    #[serde(default)]
    pub flags: FlagSchema,
    /// The controls this agent offers on a live session.
    #[serde(default)]
    pub supports: Vec<crate::Capability>,
    /// Whether this agent accepts a session id Sylph chooses.
    ///
    /// A typed fact rather than a [`crate::Capability`], because it decides what the
    /// *server* does before the first event rather than what a composer renders.
    /// *Measured 2026-08-27:* `claude` takes `--session-id`, `pi` takes `--session-id`
    /// and creates the session if it is missing, `codex` assigns its own and reports it.
    ///
    /// **Dictating is the preferred path**, because then the server can establish the
    /// durable identity without correlating a later event. A prepared link may still
    /// have no id while it is idle: some vendors create the native history record only
    /// when the first turn is accepted. In that case the adapter returns the id from
    /// [`Session::send_initial`]; an id is required for promotion, but not for opening
    /// a disposable prepared link.
    pub dictates_native_id: bool,
    /// The best fidelity this agent's *native store* can yield.
    ///
    /// `run` is always [`crate::Fidelity::Stream`] because it holds the process's own stdio, so
    /// this describes `adopt` and `watch` only. Per-vendor and measured
    /// (`docs/agents.md` § Capability matrix): `claude` flushes at tool-call and
    /// content-block boundaries, `codex` at item completion, `pi` at every
    /// `message_end`.
    ///
    /// A client reads it to know, *before* adopting, whether a waterfall over the result
    /// will mean anything.
    pub native_fidelity: Fidelity,
}

/// What a session is opened with.
///
/// `docs/session-protocol.md` § Opening a session.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    /// The agent's `current_dir`, **already contained by the envagent**.
    ///
    /// Only the machine holding a path can canonicalize it, resolve `..`, or see whether
    /// a component is a symlink, so a server checking a remote path would be matching a
    /// string against a filesystem it cannot see (`docs/configuration.md`). The adapter
    /// does not re-check: it receives a path the envagent has already bounded, and a
    /// second containment rule written here is one that will diverge from the first.
    pub workdir: String,
    pub native_id: RequestedNativeId,
    /// Whether to append `SOUL.md`. **The server decides, never the adapter.**
    ///
    /// # This is a narrowing, and it is worth stating precisely
    ///
    /// `LinkConfig.soul` was deliberately tri-state: a path, `Some("")` meaning inject
    /// nothing, and `None` meaning let the adapter resolve its own default. `AGENTS.md`
    /// records that the empty value is *meaningful rather than missing* — the protocol
    /// reserves it for "no identity" — so a `bool` genuinely cannot express what that
    /// field could.
    ///
    /// The third state is unreachable under this architecture rather than merely
    /// dropped, and the argument is that resolution moved server-side. The old `None`
    /// meant "adapter, read `SYLPH_SOUL` from your environment and decide", which every
    /// adapter implemented as `[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`. That is the
    /// adapter answering a question the server is now the only holder of: `Config`
    /// resolves the path once, an explicitly empty setting resolves to none, and a
    /// configured-but-absent file resolves to none too — so by the time a `RunSpec` is
    /// built there is no undecided case left. `docs/session-protocol.md` § What the
    /// adapter owns lists "decide identity injection" under **must not**.
    ///
    /// Two consequences that follow from that and are not obvious:
    ///
    /// - The *text* has to reach the adapter, not a path, because the file lives beside
    ///   the server and the adapter runs on another machine. That is why this is a
    ///   boolean here and [`Self::identity_preamble`] carries the content.
    /// - `false` and "there is no SOUL.md" are the same instruction to an adapter and
    ///   different facts about the server. The server records the *setting*
    ///   (`sessions.inject_identity`) and this field is what it resolved to for this
    ///   launch, so the distinction is kept where it is legible rather than encoded in
    ///   an `Option<String>` whose empty case a reader has to know about.
    pub inject_identity: bool,
    /// The identity text to append, present exactly when `inject_identity` is true and
    /// the server resolved a non-empty file.
    ///
    /// Content rather than a path: the server holds the file and the adapter runs
    /// wherever the agent does. An adapter writes it to a temporary file if its CLI
    /// wants one — `claude` takes `--append-system-prompt-file`, which is additive so
    /// the agent's own rules and the project's `CLAUDE.md` survive.
    pub identity_preamble: Option<String>,
    /// Spawn-time options: model, effort, and whatever the agent declared in
    /// [`AgentInfo::flags`].
    ///
    /// A `BTreeMap` so an argv is deterministic. Two launches of one spec producing
    /// different command lines makes a reproduction depend on hash iteration order.
    pub flags: BTreeMap<String, String>,
    /// Initial permission mode, if the vendor takes one at launch.
    ///
    /// **A launch flag gating a runtime-mutable value must be registered
    /// unconditionally.** *Measured on `claude` 2.1.246:* `--permission-prompt-tool` can
    /// only be asked for on the command line while the mode it serves changes through
    /// `set_config`, so gating it on this field lost the approval channel permanently
    /// for anyone who switched — launch bypassing, switch to `default`, and every tool
    /// call answers `Claude requested permissions to use Bash, but you haven't granted
    /// it yet`. A mode that decides alone simply never consults the channel.
    pub permission_mode: Option<String>,
}

/// Which id a session should be opened with.
///
/// Three cases, and `Option<String>` could express only two of them: `LinkConfig`
/// carried `session_id: Option<String>` where `Some` meant *resume* and `None` meant
/// *fresh*, so "dictate this id for a new session" — which is what `claude`'s adapter
/// has always actually done, minting a UUID and passing `--session-id` — had no
/// spelling. It worked because resume and dictate happen to produce the same flag on
/// that CLI, which is a coincidence of one vendor's command line rather than a property
/// of the protocol: `codex` spells them `resume <id>` and nothing, and an adapter that
/// cannot tell them apart cannot choose.
///
/// The distinction is also load-bearing on the server: dictating a **new** id must
/// refuse if that id already names a session (`409`), while resuming requires that it
/// does. Collapsed into one variant, one of those two checks is impossible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedNativeId {
    /// Open a new session under this id, which the server minted.
    ///
    /// The preferred path: identity is known before the first event.
    Dictate(NativeId),
    /// Continue the session the vendor already has under this id.
    Resume(NativeId),
    /// The vendor assigns one and reports it. Only for agents whose
    /// [`AgentInfo::dictates_native_id`] is false.
    New,
}

/// Which native sessions to list.
///
/// Every field narrows; all absent means "everything this store holds".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreFilter {
    /// **An exact path match after normalizing trailing slashes, never a substring.**
    ///
    /// The store this replaced compiled a path filter to `LIKE '%' || ? || '%'`, so
    /// asking for `/w/app` also returned `/w/app-old` and every subdirectory. And
    /// because the match was unanchored, a parent directory's pattern matched everything
    /// beneath it while those sorted by recency rather than depth — one real directory's
    /// first exact match landed ~1300 rows in and a paged scan gave up first, reporting
    /// it empty.
    pub workdir: Option<String>,
    /// Only sessions whose last activity is at or after this epoch-millisecond mark.
    pub since_ms: Option<i64>,
    /// At most this many, newest first. `None` means the adapter's own bound.
    pub limit: Option<usize>,
}

/// Normalize a workdir for comparison: trailing separators dropped, root preserved.
///
/// One function because [`StoreFilter::workdir`] states one rule, and it was written out
/// three times — once per adapter, byte-identical — plus a fourth time in the server with
/// a **different** root case. That divergence is why this preserves `/`: an adapter's copy
/// trimmed it to `""`, so a session whose workdir is the filesystem root compared equal to
/// one with no workdir at all. Harmless while both sides of a comparison used the same
/// copy, and exactly the kind of thing that stops being harmless when only one of four is
/// changed.
///
/// Comparison only. This is not a containment check and not a canonicalization — a path
/// reaching an adapter has already been bounded by the envagent
/// ([`RunSpec::workdir`]), and only the machine holding a path can resolve it.
pub fn normalize_workdir(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// One row of "what is on this machine", cheap enough to list without parsing.
///
/// [`Adapter::enumerate`] is what makes adopt and watch offerable in a UI rather than
/// requiring a user to know a vendor id.
///
/// **Every field an adapter cannot answer is absent, not defaulted.** A synthesized
/// title or timestamp is a lie a journal makes permanent, and this row is what a picker
/// renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSummary {
    pub native_id: NativeId,
    /// Where it ran. `None` means the store recorded none, which makes the session
    /// **not resumable** — the server answers `NATIVE_SESSION_NOT_RESUMABLE` rather than
    /// guessing a directory to run an agent in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// The vendor's own recorded title, when it keeps one.
    ///
    /// *Measured:* `claude` records one for ~73% of sessions; `codex` and `pi` record
    /// **none**. So every surface showing this needs the opening-ask fallback, and that
    /// fallback belongs to the server rather than here — because the rule it needs is
    /// subtle enough that two implementations would disagree. **The first `user` event
    /// is usually not what the user typed**: agents inject their own context as leading
    /// pseudo user turns, so taking it verbatim labels most sessions with boilerplate
    /// byte-identical across every session of that agent. Skipping injected envelopes
    /// raised recognizable labels from ~10% to 98% over 311 measured sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// Last recorded activity. `None` while a session is still being written and the
    /// store has not settled — an absent end is not an ended session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
    /// How many events one pass would yield, when the store can say cheaply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_count: Option<i64>,
    /// The session this one was delegated from or forked out of, as the vendor recorded
    /// it.
    ///
    /// **Not derived from an id-naming convention.** The `/agent-` suffix catches most
    /// subagents and misses those recording only a parent id — `codex` among them,
    /// measured from its own store: a rollout with `parent_thread_id` set,
    /// `thread_source: subagent`, and no `/agent-` segment. Those sat at the top level
    /// of every listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_native_id: Option<NativeId>,
    /// What fidelity parsing this session would actually yield.
    ///
    /// Per session rather than only per agent, because a store can hold both: a session
    /// truncated mid-write, or one whose early turns predate a vendor's flush change,
    /// yields less than [`AgentInfo::native_fidelity`] promises.
    pub fidelity: Fidelity,
}

/// Anything an adapter has to tell the server about a session.
///
/// # Why a stream of messages and not a stream of events
///
/// A session multiplexes three unrelated kinds of fact — an event to journal, a request
/// blocking the agent on a human, and a capability surface that changed. All three
/// arrive on one channel because all three are *ordered with respect to each other*: an
/// approval raised mid-turn belongs between the `tool_call` before it and the
/// `tool_result` after, and a capability change caused by a `system/init` belongs where
/// that turn began. Two channels would let a client render a dialog for a tool call it
/// has not seen yet.
///
/// # What does not survive
///
/// `LinkEvent::Closed { exit_code, stderr_tail }` is gone. **An exit code is not a
/// concept an in-process adapter has** — there is no process of its own to exit, and a
/// vendor CLI's exit code was only ever readable because a subprocess wrapped it. The
/// stream simply ends, which is what a `Stream` already expresses, and a failure that
/// needs explaining arrives as an `Err` item before it does.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMessage {
    /// One normalized event, in the order it happened.
    Event(Event),
    /// The agent is waiting on a human.
    ///
    /// **Durable state, not UI state**: journaled, and re-delivered on reconnect, or
    /// closing a laptop lid strands an agent forever.
    Request(Request),
    /// The capability surface changed — a skill was edited, a plugin loaded.
    ///
    /// **A partial, and the server owns the merge** ([`CapabilityFacets::apply_to`]).
    /// The v3 transport merged inside the link so the server never saw a partial;
    /// in-process that would put one rule in three adapters, so the facets travel and
    /// the server applies them. An unmentioned facet is not changed.
    Capabilities(CapabilityFacets),
}

/// A session's messages, as they arrive.
///
/// `Send` because the server drives it from a task that is not the one that opened the
/// session; boxed because every adapter's stream is a different concrete type and the
/// server holds them uniformly.
///
/// An `Err` item is a failure **reading** the session, not a failure of the session: a
/// truncated native file, a vendor payload that could not be parsed. The stream may
/// continue after one — `pi`'s `_rewriteFile()` truncate-rewrites with no lock, so a
/// reader can observe an empty or half-written file and the correct response is to keep
/// reading rather than to declare the session over.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<SessionMessage>> + Send>>;

/// A live session. **Carries turns both ways.**
///
/// Held for the session's lifetime, not for a turn's. v1 spawned a process per turn,
/// which made every stateful thing a vendor CLI knows — loaded skills, permission
/// grants, a warmed context, an MCP connection — reachable only by restarting and
/// re-resolving it. Worse, it made "the turn ended" and "the process exited" the same
/// event, so an agent that speaks *after* its turn had nowhere to speak from — and
/// *measured on `claude` 2.x*, a session with backgrounded work emits the delegate's
/// completion after its turn's `result` and then an entire exchange of its own to report
/// it, stdout still open, process still alive.
///
/// So: open once, a turn per [`Self::send`], and the outcome arrives as an event on
/// [`Self::events`] rather than as an exit code.
#[async_trait]
pub trait Session: Send + Sync {
    /// The id the vendor actually used, when it has assigned one.
    ///
    /// A prepared link may be fully usable before the vendor creates a native history
    /// record.  In particular Claude assigns its id only after the first user turn;
    /// Pi and Codex may expose an in-memory id that is not yet listed by their native
    /// history store.  The server must therefore delay durable promotion until this
    /// becomes `Some` rather than inventing an id.
    fn native_id(&self) -> Option<NativeId>;

    /// This session's current capability surface.
    ///
    /// The surface the *adapter* knows, which is what the vendor last reported. The
    /// server holds the merged copy it built from [`SessionMessage::Capabilities`]; this
    /// is what an adapter answers on open and after a re-attach.
    ///
    /// **Read from the vendor, never from what was last set.** `pi`'s adapter reported a
    /// field only one code path populated, so any surface built by another path — a
    /// re-attach, a set reply — reported nothing while the session plainly had a model,
    /// and the chip fell back to "agent default" with the checkmark on no row. The
    /// vendor is also the only thing that knows whether a set was *honoured*, and a
    /// session can be steered by the agent's own TUI besides.
    async fn capabilities(&self) -> Result<SessionCapabilities>;

    /// Start a turn.
    ///
    /// Returns when the agent has **accepted** it, not when it has finished: a turn
    /// outlives the call that started it, which is the same reason `POST /turns` answers
    /// `202`. Completion arrives on [`Self::events`].
    async fn send(&self, turn: TurnInput) -> Result<()>;

    /// Start the first turn of a prepared session and return the native identity.
    ///
    /// A prepared link is useful before it has a durable identity, because capabilities
    /// and configuration are live properties.  Some vendors only assign their native id
    /// when the first user message is accepted (Claude is one measured example), so the
    /// server cannot require [`Self::native_id`] before sending that message.  The adapter
    /// owns the wait for its id and buffers any events emitted while the server promotes the
    /// link; the ordinary [`Self::events`] stream remains lossless for the caller.
    async fn send_initial(&self, turn: TurnInput) -> Result<NativeId>;

    /// End the current turn, keep the session.
    ///
    /// Distinct from a stop signal, which kills the process. Conflating them loses one
    /// of two real intentions: "that is not what I meant" versus "stop everything".
    /// Refuses with [`crate::AdapterError`] when [`crate::Capability::Interrupt`] is not
    /// offered.
    async fn interrupt(&self) -> Result<()>;

    /// Add text to a turn that is already running.
    ///
    /// `expected_turn_id` is what keeps a steer racing a turn boundary from being
    /// delivered into the **next** turn — the message would arrive, nothing would error,
    /// and it would land in the wrong conversation. `codex` requires the same
    /// precondition. `None` means "whatever is running", which is a coherent ask from a
    /// client with no turn in view.
    async fn steer(&self, text: &str, expected_turn_id: Option<&str>) -> Result<()>;

    /// Add text to run **after** the current turn.
    ///
    /// Distinct from [`Self::steer`] by *when the agent reads it*, which is the whole
    /// difference: a steer is a correction and this is a follow-up. Refuses with
    /// [`crate::AdapterError`] when [`crate::Capability::Queue`] is not offered.
    ///
    /// **No default implementation, deliberately.** A default that queued nothing and
    /// answered `Ok` would be the silent-success failure this trait's own docs forbid, and a
    /// default that refused would make a vendor's real capability invisible until someone
    /// noticed. Every adapter states what it does.
    ///
    /// Note what a caller cannot assume: a vendor may accept the text and *report* nothing.
    /// `pi` emits `queue_update` carrying `steering` and `followUp`, so a client can show
    /// what is waiting; the other two do not, and there a queued message is only observable
    /// when it runs.
    async fn queue(&self, text: &str) -> Result<()>;

    /// Set one config option, and answer with the **refreshed array**.
    ///
    /// Not an acknowledgement, because setting one option can change another's choices:
    /// `pi`'s thinking levels depend on the selected model, and `claude` carries
    /// `supportedEffortLevels` per model. A design where effort is one session-global
    /// enumeration is wrong for both.
    ///
    /// Refuses with [`crate::AdapterError`], and **the two codes are not
    /// interchangeable**: [`crate::RefusalCode::UnsupportedField`] for no such option,
    /// [`crate::RefusalCode::InvalidValue`] for this value right now.
    async fn set_config(&self, id: &str, value: serde_json::Value) -> Result<Vec<ConfigOption>>;

    /// Map a runtime config change to the launch flag that should be remembered when
    /// this session is re-attached.
    ///
    /// The server stores only spawn-time flags on a durable session.  A live config
    /// surface is intentionally broader: some controls are process-local (for example
    /// Claude's output style), while others use a different launch spelling (Claude's
    /// `permissionMode` versus its `mode` flag).  Keeping this decision in the adapter
    /// means the server never grows a vendor-name table and never persists a setting
    /// that a resumed process cannot consume.  Returning `None` means the setting is
    /// runtime-only, or the vendor did not provide a safe launch equivalent.
    ///
    /// The method is called only after [`Self::set_config`] succeeds.  The value is the
    /// user's request, not a claim about the vendor's current state; the capability
    /// surface remains the sole source of confirmed `current_value` fields.
    fn launch_flag_for_config(
        &self,
        _id: &str,
        _value: &serde_json::Value,
    ) -> Option<(String, String)> {
        None
    }

    /// Answer a request this session raised.
    ///
    /// The server publishes the answer to every client **before** forwarding here: the
    /// claim is what makes the answer final, so a request answered against a dead
    /// session must still retire everywhere.
    async fn resolve(&self, resolution: Resolution) -> Result<()>;

    /// This session's messages.
    ///
    /// Takes `&mut self` and yields the stream once. A second call is a bug rather than
    /// a second subscription: two consumers of one vendor's stdio would each see half
    /// the turn, and fan-out is the server's job, which already does it.
    fn events(&mut self) -> Option<EventStream>;

    /// Ask the agent to exit, and make sure it did.
    ///
    /// Consumes nothing and is idempotent, because the idle reaper, an explicit detach,
    /// and shutdown can all reach it. A `close` a vendor ignores must not leak a
    /// process, so an adapter escalates unconditionally — signalling an already-exited
    /// group is defined to be harmless.
    async fn close(&self) -> Result<()>;
}

/// One turn's input.
///
/// A struct rather than five arguments because they travel together and two of them are
/// `String`. `Link::send` took `(text, attachments, command, args, config)` positionally,
/// where swapping the first and third compiles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnInput {
    pub text: String,
    /// Paths within the session's workdir, already contained by the envagent.
    pub attachments: Vec<String>,
    /// A [`crate::CommandKind::Prompt`] command to run instead of plain text.
    ///
    /// A **control** command never reaches here: it is typed state, resolved through
    /// [`Session::set_config`]. Sent as prompt text, `/model` returns a paragraph
    /// listing models, which is the CLI behaving correctly because a headless client has
    /// no picker to open.
    pub command: Option<String>,
    pub args: BTreeMap<String, String>,
    /// Config overrides for this turn only.
    ///
    /// Sent **with** the turn rather than as separate preceding calls: those were extra
    /// round trips with a window in between where a turn could run under a mix of old
    /// and new settings.
    pub config: BTreeMap<String, serde_json::Value>,
}

/// Everything Sylph knows about one vendor CLI.
///
/// One implementation per agent (`docs/agents.md`), holding how to launch it, how to
/// read its native store, what it exposes to a second party, and where it lies.
///
/// # All three modes emit the same events
///
/// That is the point of the shape. The journal, the history fold, the timeline, replay,
/// and search see one vocabulary and cannot tell which mode produced a given row. The
/// only visible difference is [`Fidelity`] on the record.
///
/// | Mode | Source | Fidelity | Control facet |
/// | --- | --- | --- | --- |
/// | [`Self::run`] | the process's own stdio | `stream` | full |
/// | [`Self::watch`] | native store, followed | `blocks` or `messages`, per vendor | whatever the vendor offers a second party |
/// | [`Self::adopt`] | native store, one pass | `blocks` or `messages`, per vendor | none while it runs; full once resumed by `run` |
///
/// # What an adapter owns, and what it must not
///
/// **Owns:** the vendor CLI's command line and native store format, translation in both
/// directions, the vendor's own ids, and the decision to **omit rather than guess**.
///
/// **Must not:** decide identity injection (the server passes it), invent an event kind
/// the vocabulary does not have, retry a turn on its own, or report a refusal as a
/// success. Nor synthesize a value the vendor did not give — a synthesized id, name, or
/// timestamp is a lie a journal makes permanent.
///
/// # There is no `probe` for availability, and no discovery
///
/// [`Self::probe`] answers *what this CLI is*, not *whether Sylph can find an adapter
/// for it*. Availability is whether the vendor CLI is on that machine's `PATH` — one
/// question with one answer and an error message that can say it. The mechanism this
/// replaces produced two total failures, both recorded in
/// `docs/session-protocol.md` § What changed: zero discovered agents made `/` a dead
/// keystroke three layers from the cause, and a stale fixture failed a whole E2E suite
/// against an agent that did not exist.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Which agent this speaks for, and what the local CLI reports about itself.
    ///
    /// Fails when the CLI is absent or unrunnable. **The absence is one fact about the
    /// machine, not one fault per caller**: a caller polled on a timer reports it once
    /// at `error` and at `debug` thereafter, or one missing binary becomes a warning per
    /// session per pass — measured at 2339 identical lines in one E2E run.
    async fn probe(&self) -> Result<AgentInfo>;

    /// Hold a live session's stdio. The returned handle carries turns both ways.
    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>>;

    /// Parse what the vendor's native store already holds, **once**.
    ///
    /// The stream ends when the pass does. Yields [`Fidelity::Blocks`] or
    /// [`crate::Fidelity::Messages`], never [`crate::Fidelity::Stream`]: no care recovers a timing
    /// that was never written.
    async fn adopt(&self, id: &NativeId) -> Result<EventStream>;

    /// Parse, then follow the native store as it grows.
    ///
    /// The same first pass as [`Self::adopt`], then whatever the vendor appends. All
    /// three supported agents write mid-turn, which is what makes this offerable at all
    /// — an observer sees progress rather than only outcomes.
    ///
    /// Two hazards a watcher must be built around, both measured on `pi` 0.84.3 and both
    /// general enough to expect elsewhere: `_rewriteFile()` truncate-rewrites with mode
    /// `"w"` and no lock, so a reader can observe an empty or half-written file; and
    /// persistence buffers until the first assistant message, so the file **does not
    /// exist** early in a turn. **An absent file is not an absent session.**
    async fn watch(&self, id: &NativeId) -> Result<EventStream>;

    /// Which native sessions this machine's store holds.
    ///
    /// Answers "what is on this machine" **without parsing every session**, which is the
    /// requirement rather than an optimization: measured against a real store, asking per
    /// session costs two orders of magnitude more per fact than one bounded read per file,
    /// which is the difference between a listing that is affordable and one that is not.
    /// **An adapter that implements this by parsing each session has not implemented it.**
    async fn enumerate(&self, filter: &StoreFilter) -> Result<Vec<NativeSummary>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trailing separators are dropped and the filesystem root survives.
    ///
    /// The root case is the reason this function exists rather than four copies. Three of
    /// those trimmed `/` to `""`, which made a session whose workdir *is* the root compare
    /// equal to one that recorded no workdir at all — invisible while both sides of a
    /// comparison used the same copy, and a silent mismatch the moment one of four changed.
    #[test]
    fn normalizing_a_workdir_drops_trailing_slashes_and_keeps_the_root() {
        assert_eq!(normalize_workdir("/w/app/"), "/w/app");
        assert_eq!(normalize_workdir("/w/app"), "/w/app");
        assert_eq!(normalize_workdir("/w/app///"), "/w/app");
        // The divergence this replaced.
        assert_eq!(normalize_workdir("/"), "/");
        assert_eq!(normalize_workdir("///"), "/");
        assert_eq!(normalize_workdir(""), "/");
    }

    /// A prefix is not a match, which is the substring bug `StoreFilter::workdir` records.
    #[test]
    fn normalizing_does_not_make_a_sibling_directory_equal() {
        assert_ne!(normalize_workdir("/w/app-old"), normalize_workdir("/w/app"));
        assert_ne!(normalize_workdir("/w/app/sub"), normalize_workdir("/w/app"));
    }
}
