//! The event vocabulary. One set of kinds, whichever of the three adapter modes
//! produced them.
//!
//! `docs/session-protocol.md` §§ Events and Delegated trajectories govern.
//!
//! # What lives here and what stays on the server
//!
//! This is the event **an adapter emits**. The server's own wire projection
//! (`backend/src/events/mod.rs`) is this plus two fields that are not an adapter's to
//! set, and it is left there until step 3 of `docs/roadmap.md` § Implementation order
//! moves the server:
//!
//! - `seq`, because ordering is the server's to decide. An adapter numbering its own
//!   events could not interleave correctly with the server's own — the user's message,
//!   a status — which share one stream.
//! - `source` (`agent` | `server`), because an adapter can only ever be one of them, so
//!   a field for it would be a constant an adapter could get wrong.
//!
//! That is a layering, not a second copy of one fact: no field is declared in both
//! places, and every derivation below has exactly one producer.

use serde::{Deserialize, Serialize};

/// What an event is about.
///
/// # Typed, with an escape hatch, and both halves are deliberate
///
/// `kind` carries no `CHECK` at the storage layer and must not: 001 constrained it to
/// ten values, which broke on the next kind added — `append_event` logged and
/// continued, so a sequence number was consumed and the event reached live clients
/// while nothing reached the journal. A turn streamed correctly and came back empty on
/// reload.
///
/// So the vocabulary is additive, and [`Self::Other`] is where a kind this build does
/// not know lands. That is not dead weight even though adapters are now linked in and
/// construct these values directly: `sylph.env.v1` carries events as NDJSON across a
/// machine boundary (`docs/environments.md`), so a newer envagent really can send an
/// older server a kind it has never heard of. Unknown-but-preserved is the only
/// behaviour that neither drops the event nor fails the frame.
///
/// # `plan` and `diff` are reserved and deliberately not declared
///
/// `docs/session-protocol.md` § Events records them as the intended shape for making
/// plan mode reviewable and a file change render as a diff. **No adapter emits either
/// and no timeline branch handles them.** Declaring them here would ship two variants
/// nothing constructs, and `AGENTS.md` is explicit that a declared-only entry is
/// indistinguishable from one whose condition is merely rare. They arrive as variants
/// on the same commit as the producer that emits them, and until then they reach a
/// reader as [`Self::Other`] — which is honest about what happened.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Thinking,
    Assistant,
    ToolCall,
    ToolResult,
    Usage,
    /// Server- or adapter-authored narration of state, carrying no phase.
    Status,
    /// Declares a delegated trajectory. Its `stream_id` **is** that trajectory's id,
    /// and its `span` names the trajectory it was delegated from.
    Subagent,
    /// A compaction boundary, so the timeline shows where context was condensed.
    Compact,
    /// `started` / `completed` with usage, cost, duration, stop reason.
    Turn,
    /// Facet-scoped capability updates ([`crate::CapabilityFacets`]).
    Capabilities,
    /// Agent-originated notification worth surfacing.
    Notice,
    /// A kind this build does not model, preserved verbatim.
    ///
    /// `untagged` is what makes an unrecognized string land here instead of failing the
    /// whole event, and it must stay last: serde tries variants in order.
    #[serde(untagged)]
    Other(String),
}

impl EventKind {
    /// The wire spelling, which is also what the `kind` column stores.
    ///
    /// One function so a storage write and a wire frame cannot disagree about the
    /// spelling of a kind.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Thinking => "thinking",
            Self::Assistant => "assistant",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Usage => "usage",
            Self::Status => "status",
            Self::Subagent => "subagent",
            Self::Compact => "compact",
            Self::Turn => "turn",
            Self::Capabilities => "capabilities",
            Self::Notice => "notice",
            Self::Other(raw) => raw,
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where in a block's lifecycle this event sits.
///
/// Absent means the event is not part of a block — a `status`, or anything
/// server-authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Started,
    Delta,
    /// **On a `tool_call` this means its *arguments* finished streaming**, not that the
    /// tool finished. A promoted trajectory's lifecycle is its `tool_result`; reading
    /// the phase reports every delegation as finished the instant its input parsed
    /// (`docs/session-protocol.md` § Promotion).
    Completed,
}

impl Phase {
    /// The wire spelling, and the journal's.
    ///
    /// Here rather than in the server for the same reason [`EventKind::as_str`] is: the
    /// spelling is protocol vocabulary, and a `match` written at each storage boundary is
    /// one copy per boundary to keep in step with this enum.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Delta => "delta",
            Self::Completed => "completed",
        }
    }
}

/// Whether an event reaches the durable journal or is a transient projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// Broadcast to attached clients and kept in a bounded in-memory ring, never
    /// written to the journal.
    Ephemeral,
    /// Journaled and replayed on reconnect.
    Stable,
}

/// One normalized event.
///
/// Emitted identically by all three adapter modes — that is the point of the shape.
/// The journal, the history fold, the timeline, replay, and search see one vocabulary
/// and cannot tell which mode produced a given row. The only visible difference is
/// [`crate::Fidelity`] on the record, which says how much resolution the source had.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// Stable identifier for one logical block across its phases.
    ///
    /// Exposed as `streamId` rather than `id` so it cannot be confused with a database
    /// row id.
    ///
    /// **Vendor block state must be keyed by `(trajectory, stream_id)`, never by the
    /// vendor's own index alone.** `claude` numbers content blocks per message, so two
    /// concurrent subagents both open index 0 — one index-keyed map made each one's
    /// deltas append to the other's text. Data corruption, not a display fault, and
    /// invisible until two delegates run at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    /// The delegated trajectory this event happened in. Absent means the root turn.
    ///
    /// One pointer carries subagents, nested subagents, and a workflow's parallel
    /// phases, because a [`EventKind::Subagent`] event's `stream_id` is itself a
    /// trajectory id. Depth is unbounded and siblings may run concurrently; no case in
    /// the protocol distinguishes any of these.
    ///
    /// Three rules a consumer needs, each from a shipped defect:
    ///
    /// - **Any `stream_id` another event names here is a trajectory**, whatever kind
    ///   opened it — a `tool_call` so named is *promoted in place*. Delegation is only
    ///   knowable retroactively, so nothing may decide it at open time; guessing by
    ///   tool name was right until the vendor shipped `Agent` and `Workflow`.
    /// - **An unknown `span` is not an error.** Render at the root rather than
    ///   dropping, or work is lost exactly where structure is least reliable.
    /// - **A cycle silently deletes both nodes.** Attachment holds the first as an
    ///   orphan awaiting the second, which then adopts it *and* is adopted by it, so
    ///   neither is a root and the pair vanishes with nothing reporting it missing.
    ///   Repair by reachability after attachment; no single pass detects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    /// What this block is called, when the adapter knows.
    ///
    /// **Absent when unknown, and this is not a tidiness rule.** A guess written here
    /// becomes permanent: the history fold keeps the best name any phase gave a block,
    /// so a later `delta` carrying the real brief normally wins — but when the brief
    /// never finishes, because the delegation fails early or the link dies mid-input,
    /// the placeholder becomes that trajectory's durable name, indistinguishable from
    /// one an agent actually chose. Rows already written that way **cannot be
    /// repaired**. `Option`/`null` is the only value that stays honest in a journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Append-only across a block's phases, with one exception: a delegation's
    /// **progress is a snapshot, not delta text**. A vendor's progress notification
    /// re-sends its whole current summary rather than a suffix, so appending each one
    /// repeats it — measured at six copies of one sentence. Such a summary replaces.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The vendor's own payload for this event, when there is one.
    ///
    /// Also where `is_backgrounded` travels, because it is one vendor's fact about one
    /// delegation rather than a property every event has. The server reads it with
    /// `json_extract` (`docs/database-schema.md`) — and when it does, it compares
    /// against SQLite's integer `1`: a comparison written against `true` matches
    /// nothing, and every backgrounded delegation is then swept closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Whether this event reports a failure.
    ///
    /// On a trajectory's close this is what escalates its ancestors' status, so a
    /// failure three levels down is visible at the top without a client inferring it
    /// from a missing result.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// **A field, never a method.** One fact with two derivations diverges the moment
    /// either grows a case: stability was both a field and a method recomputing it
    /// from the phase, equivalent until `subagent` became stable at every phase — after
    /// which a trajectory's `started` was classified stable, published as stable, and
    /// silently *not* journaled, because the method disagreed with the field beside it.
    /// The reload then flattened every delegated subtree onto the root, which is the
    /// exact bug `span` exists to prevent, reappearing one layer down.
    ///
    /// [`Self::derive_stability`] is the one producer. Read this field everywhere else.
    pub stability: Stability,
    pub created_at_ms: i64,
    /// When this block's *first* phase arrived, on a consolidated block only.
    ///
    /// A live event is one arrival, so `created_at_ms` is the whole story. A
    /// consolidated block is many arrivals folded into one row keyed by its close, and
    /// `created_at_ms` then means "when it finished" — so a client computing a duration
    /// from a settled turn would have an end and no beginning. The fold has always
    /// computed this; only the wire once omitted it, and that absence made the
    /// delegation overview get *designed* around durations being unreconstructible.
    ///
    /// Absent on a live event, so a reader can tell "streaming, one arrival" from
    /// "settled, and here is the span it covered".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
}

impl Event {
    /// The one place stability is decided.
    ///
    /// A `completed` phase carries its block's whole stable value, and an event with no
    /// phase is authored rather than streamed and is always stable.
    ///
    /// [`EventKind::Subagent`] is stable at **every** phase, and the exception is
    /// structural rather than a special case for one vendor: its `started` phase is
    /// where a trajectory's id, name, and parent live. Dropped as an ephemeral
    /// projection, the journal keeps every child event pointing at a parent it never
    /// recorded, and a reload flattens the whole delegated subtree into its parent's
    /// own work.
    ///
    /// Called once, at construction. Anything downstream reads [`Self::stability`].
    pub fn derive_stability(kind: &EventKind, phase: Option<Phase>) -> Stability {
        match phase {
            Some(Phase::Completed) | None => Stability::Stable,
            _ if *kind == EventKind::Subagent => Stability::Stable,
            _ => Stability::Ephemeral,
        }
    }

    /// A minimal event with its stability derived and everything optional absent.
    ///
    /// The constructor exists so no adapter sets `stability` by hand — the field must
    /// have one producer, and a builder that takes it as an argument is not one.
    pub fn new(kind: EventKind, phase: Option<Phase>, created_at_ms: i64) -> Self {
        Self {
            stability: Self::derive_stability(&kind, phase),
            kind,
            phase,
            stream_id: None,
            span: None,
            name: None,
            text: String::new(),
            data: None,
            is_error: false,
            created_at_ms,
            started_at_ms: None,
        }
    }

    /// True when this event belongs in the durable journal.
    ///
    /// Reads the field. See [`Self::stability`] for what happened when it did not.
    pub fn is_stable(&self) -> bool {
        self.stability == Stability::Stable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trajectory_is_stable_at_every_phase() {
        // `started` is where a trajectory's id, name, and parent live. Dropped as an
        // ephemeral projection, the journal keeps children pointing at a parent it
        // never recorded, and a reload flattens the subtree onto the root.
        for phase in [Phase::Started, Phase::Delta, Phase::Completed] {
            let event = Event::new(EventKind::Subagent, Some(phase), 0);
            assert_eq!(event.stability, Stability::Stable, "subagent/{phase:?}");
            assert!(
                event.is_stable(),
                "subagent/{phase:?} must reach the journal"
            );
        }

        // The exception is scoped to structure: content still projects.
        assert!(!Event::new(EventKind::Assistant, Some(Phase::Delta), 0).is_stable());
        assert!(!Event::new(EventKind::Assistant, Some(Phase::Started), 0).is_stable());
        assert!(Event::new(EventKind::Assistant, Some(Phase::Completed), 0).is_stable());
        // Authored events carry no phase and are always durable.
        assert!(Event::new(EventKind::Status, None, 0).is_stable());
    }

    /// `is_stable` must read the field, so setting the field to something the
    /// derivation would not have chosen has to change the answer. A method
    /// recomputing from the phase would pass every other test in this file and fail
    /// this one — which is the defect that shipped.
    #[test]
    fn is_stable_reads_the_field_rather_than_recomputing_it() {
        let mut event = Event::new(EventKind::Assistant, Some(Phase::Delta), 0);
        assert!(!event.is_stable());
        event.stability = Stability::Stable;
        assert!(event.is_stable());
    }

    /// A kind this build does not model must reach a reader intact rather than
    /// failing the frame. `sylph.env.v1` carries events across a machine boundary, so
    /// a newer envagent really can send one.
    #[test]
    fn an_unknown_kind_is_preserved_rather_than_failing_the_event() {
        let event: Event =
            serde_json::from_str(r#"{"kind":"telemetry","stability":"stable","createdAtMs":1}"#)
                .unwrap();
        assert_eq!(event.kind, EventKind::Other("telemetry".into()));
        // And it round-trips under its own name, not under a wrapper.
        assert_eq!(serde_json::to_value(&event).unwrap()["kind"], "telemetry");
    }

    /// A known kind must not be swallowed by the catch-all. `untagged` tries variants
    /// in declaration order, so this is a real ordering dependency rather than a
    /// tautology.
    #[test]
    fn a_known_kind_parses_as_itself_and_not_as_other() {
        let event: Event =
            serde_json::from_str(r#"{"kind":"tool_call","stability":"stable","createdAtMs":1}"#)
                .unwrap();
        assert_eq!(event.kind, EventKind::ToolCall);
        assert_eq!(event.kind.as_str(), "tool_call");
    }

    #[test]
    fn a_trajectory_pointer_survives_the_wire_in_both_directions() {
        // Both halves matter and they fail differently: a parse miss silently flattens
        // the tree, and a serialize miss lets the server hold structure it never
        // sends — which looks like a frontend bug because the journal is right and the
        // client is empty.
        let mut event = Event::new(EventKind::Assistant, Some(Phase::Completed), 7);
        event.stream_id = Some("b1".into());
        event.span = Some("s1".into());
        event.text = "hi".into();

        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["streamId"], "b1");
        assert_eq!(wire["span"], "s1");
        assert!(wire.get("stream_id").is_none(), "snake_case must not leak");
        assert_eq!(serde_json::from_value::<Event>(wire).unwrap(), event);
    }

    /// An absent name is absent on the wire, because a journal makes every guess
    /// permanent and `null` is the only value that stays honest after the fact.
    #[test]
    fn an_unnamed_block_omits_the_field_entirely() {
        let wire =
            serde_json::to_value(Event::new(EventKind::Subagent, Some(Phase::Started), 1)).unwrap();
        assert!(wire.get("name").is_none());
        for absent in ["phase_missing", "text", "data", "isError", "startedAtMs"] {
            assert!(wire.get(absent).is_none(), "{absent} should be omitted");
        }
    }
}
