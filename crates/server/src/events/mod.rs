//! Normalized event plumbing between adapter links and SSE clients.
//!
//! Two storage tiers back a turn's event stream, mirroring the split described
//! in `docs/architecture.md`:
//!
//! * **Stable events** (`phase == "completed"`, plus server-authored status and
//!   error events) are persisted to `journal.db` and replayed on reconnect.
//! * **Ephemeral events** (`started` / `delta`) are transient projections. They
//!   are broadcast to attached clients and kept in a bounded in-memory ring so
//!   a client that reconnects mid-stream can still catch the tail, but they are
//!   never written to the journal.
//!
//! Sequence numbers come from one monotonic per-session counter, so stable and
//! ephemeral events share a single ordering. The journal therefore contains
//! gaps where ephemeral events were dropped, which is expected and allowed by
//! the `UNIQUE(session_id, seq)` constraint. The session trace channel reuses
//! that ordering across turns; `arrivalTurnId` on the wire preserves where a
//! late delegated event arrived without making that turn its owner.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::models::now_ms;
use crate::models::Event;
use sylph_protocol::InteriorState;

pub mod seq;

pub use seq::SeqAllocator;

/// How many ephemeral events to retain per turn for reconnect replay.
///
/// Deltas arrive at token rate, so this is a tail window rather than a full
/// history: a client that reconnects promptly resumes seamlessly, and one that
/// reconnects late still gets every stable event from the journal.
const PROJECTION_CAPACITY: usize = 512;

/// Broadcast capacity per turn. Slow SSE consumers that fall behind receive a
/// lag error and resubscribe from the journal rather than blocking the runner.
const BROADCAST_CAPACITY: usize = 1024;

/// Whether a stored `data_json` column carries anything, and what.
///
/// `None` means absent. Absence has no `NULL` to spell it with — the column is
/// `TEXT NOT NULL DEFAULT '{}'` in both tiers — so `"{}"` is what
/// [`StreamEvent::to_row`] writes for an event with no data, and it has to read
/// back as absent rather than as `Some({})`. An empty object is a payload that
/// says nothing while claiming to say something, which leaves every consumer
/// downstream re-deciding whether it counts as data.
///
/// `""` is matched for legibility, not for behaviour: it is what a row carries
/// when nothing ever wrote the field, and it would reach `None` through the
/// malformed arm below anyway since it is not valid JSON. Naming it says that
/// case was considered.
///
/// Malformed JSON is absent too, deliberately rather than as an error: a corrupt
/// data column must not make an otherwise readable event unrenderable, since its
/// kind and text still carry the useful content. `"null"` is *not* absent — it
/// parses to `Some(Value::Null)`, which is an adapter having explicitly sent
/// null, and flattening that into "no data" would discard the distinction.
///
/// One function because this is one decision. It was written out four times —
/// here, in `storage::history::consolidate` twice, and in
/// `db::store::reconcile_trajectory` — and four copies of a rule is a rule that
/// diverges on the first case anyone adds to only one of them.
pub fn data_from_column(data_json: &str) -> Option<serde_json::Value> {
    match data_json {
        "" | "{}" => None,
        raw => serde_json::from_str(raw).ok(),
    }
}

pub mod tool_summary;

/// A normalized event as it appears on the wire, both from adapters
/// (`sylph.agent.v3` event messages) and to browsers (SSE `data:` payloads).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    pub seq: i32,
    /// The turn on which the server received this event.
    ///
    /// This is intentionally separate from trajectory ownership. A delegated trajectory
    /// may outlive the turn that opened it, so a consumer needs both the arrival position
    /// and the trajectory's owner. Older clients may ignore this field.
    #[serde(rename = "arrivalTurnId", skip_serializing_if = "Option::is_none")]
    pub arrival_turn_id: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Stable identifier for one logical block across its phases. Named `id`
    /// by the runner protocol; exposed as `streamId` so it cannot be confused
    /// with a database row id.
    #[serde(rename = "streamId", skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    /// The delegated trajectory this event happened in; absent is the root turn.
    ///
    /// `docs/session-protocol.md` § Delegated trajectories. One pointer carries
    /// subagents, nested subagents, and a workflow's parallel phases, because a
    /// `subagent` event's `stream_id` is itself a trajectory id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    /// Typed state of a delegated trajectory's interior, when this phase reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interior_state: Option<InteriorState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    pub stability: String,
    pub source: String,
    /// Server receipt time. Unlike `created_at_ms`, this is always the server's clock.
    pub arrival_at_ms: i64,
    /// Vendor/source timestamp when one was actually recorded. `None` means the source
    /// carried no usable timestamp; it is never synthesized from arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at_ms: Option<i64>,
    pub created_at_ms: i64,
    /// When this block's *first* phase arrived, on a consolidated block only.
    ///
    /// A live event is one arrival, so `created_at_ms` is the whole story. A
    /// consolidated block is many arrivals folded into one row keyed by its close,
    /// and `created_at_ms` then means "when it finished" — so a client computing a
    /// duration from a settled turn has an end and no beginning.
    ///
    /// `history::consolidate` has always computed and stored this; only the wire
    /// omitted it. That absence is why the delegation overview was designed around
    /// durations being unreconstructible after a turn settles: the measurement
    /// existed and was not published, which looks identical to not existing.
    ///
    /// Absent on a live event, so a reader can tell "streaming, one arrival" from
    /// "settled, and here is the span it covered".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// Present when the event carries a bounded preview and the full body is fetched lazily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<BodyRef>,
    /// Server-authored tool semantics, shared by headers and aggregate counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_summary: Option<tool_summary::ToolSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyRef {
    pub seq: i32,
    pub text_bytes: u64,
    pub data_bytes: u64,
}

impl StreamEvent {
    /// True when this event belongs in the durable journal.
    ///
    /// Reads the `stability` field rather than re-deriving it from the phase. It
    /// used to do the latter, and the two answers were the same until `subagent`
    /// became stable at every phase — at which point a trajectory's `started` was
    /// classified `stable`, published as stable, and then not journaled, because
    /// this method disagreed with the field beside it. One fact with two
    /// derivations diverges the moment either grows a case.
    pub fn is_stable(&self) -> bool {
        self.stability == "stable"
    }

    /// Convert into a persistable row.
    pub fn to_row(&self, session_id: &sylph_protocol::SessionId, turn_id: &str) -> Event {
        Event {
            id: None,
            session_id: session_id.clone(),
            turn_id: Some(turn_id.to_string()),
            seq: self.seq,
            kind: self.kind.clone(),
            phase: self.phase.clone(),
            stream_id: self.stream_id.clone(),
            span: self.span.clone(),
            interior_state: self.interior_state.map(|state| state.to_string()),
            name: self.name.clone(),
            text: self.text.clone(),
            data_json: self
                .data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "{}".to_string()),
            is_error: self.is_error,
            stability: self.stability.clone(),
            source: self.source.clone(),
            arrival_at_ms: self.arrival_at_ms,
            recorded_at_ms: self.recorded_at_ms,
            created_at_ms: self.created_at_ms,
        }
    }

    /// Build from an adapter's [`sylph_protocol::Event`], for a **live** link.
    ///
    /// `seq` is left at zero for the caller to assign, because ordering is the
    /// server's to decide: an adapter numbering its own events could not
    /// interleave correctly with the server's own (the user's message, status),
    /// which share one stream.
    ///
    /// # Infallible, where the JSON version was not
    ///
    /// This replaces a `from_adapter(&serde_json::Value) -> Option<Self>` that answered
    /// `None` when `kind` was missing or not a string. With adapters linked in there is no
    /// such state: `EventKind` is an enum whose `Other` variant preserves a kind this
    /// build does not model, so an event that would have been dropped as unparseable now
    /// arrives verbatim.
    ///
    /// # Stability is re-derived here, and read as given by [`Self::from_protocol`]
    ///
    /// Both call [`sylph_protocol::Event::derive_stability`], the protocol's single
    /// producer, so there is one derivation of this fact and not two. The difference is
    /// only *what it is derived from*. [`sylph_protocol::Event::stability`] is a public
    /// field, so a live adapter can set it to anything after construction — and it decides
    /// which tier a row lands in, so a wrong value silently keeps a delta out of the
    /// journal or floods it with one. A parsed native record has no such author: its
    /// stability came from `Event::new` in this process, one call earlier.
    pub fn from_event(event: &sylph_protocol::Event) -> Self {
        // **The server's own arrival time, not the adapter's.** An adapter stamps
        // `created_at_ms` too, and reading it here would make a duration depend on a
        // remote machine's clock — which for step 4's envagent is a different clock
        // entirely. The fixture that emits its whole tree inside one millisecond is
        // measurable only because this is stamped on arrival.
        Self::from_protocol_parts(
            event,
            sylph_protocol::Event::derive_stability(&event.kind, event.phase),
            now_ms(),
            // Straight from an adapter: one arrival.
            None,
            None,
            None,
        )
    }

    /// Build from the value an adapter constructs for `adopt` and `watch`, arriving over
    /// the envagent bridge.
    ///
    /// **The vendor's own timestamp is preserved in both `recorded_at_ms` and
    /// `created_at_ms`, while `arrival_at_ms` receives the server clock.** A live
    /// event's arrival *is* when it happened. An adopted event happened when the vendor
    /// recorded it, possibly months ago, and stamping `created_at_ms` with now
    /// would make every adopted transcript claim to have occurred at the moment it was
    /// adopted — collapsing a session's whole duration to zero and ordering it into the
    /// sidebar as the newest thing on the machine. The parsers go to real trouble to recover
    /// these stamps
    /// (`crates/adapters/src/native/timestamp.rs`), and this is where that work would
    /// have been discarded.
    ///
    /// `seq` is left at zero for the caller to assign, for the same reason as
    /// [`Self::from_event`]: ordering is the server's.
    pub fn from_protocol(event: &sylph_protocol::Event) -> Self {
        let recorded_at_ms = event
            .recorded_at_ms
            .or_else(|| (event.created_at_ms != 0).then_some(event.created_at_ms));
        Self::from_protocol_parts(
            event,
            event.stability,
            now_ms(),
            event.started_at_ms,
            recorded_at_ms,
            recorded_at_ms,
        )
    }

    /// The shared body of [`Self::from_event`] and [`Self::from_protocol`].
    ///
    /// Every field is copied identically by both; the three that differ are arguments, so
    /// a reader comparing the two paths sees exactly what a live event and an adopted one
    /// disagree about and nothing else. Before this existed the two carried byte-identical
    /// copies of the phase mapping and the span normalization, and disagreed about
    /// stability in a way neither comment acknowledged.
    fn from_protocol_parts(
        event: &sylph_protocol::Event,
        stability: sylph_protocol::Stability,
        arrival_at_ms: i64,
        started_at_ms: Option<i64>,
        recorded_at_ms: Option<i64>,
        source_at_ms: Option<i64>,
    ) -> Self {
        // `created_at_ms` is the one always-present timestamp, which is what makes it
        // worth keeping beside the explicit pair rather than a field awaiting removal:
        // `recorded_at_ms` is absent whenever the vendor supplied nothing usable, so a
        // consumer that wants "when did this happen, as well as anyone knows" would
        // otherwise have to coalesce the two itself, and every such consumer would be a
        // separate chance to pick the wrong one.
        //
        // Source time wins where there is one. Falling back to arrival — never a
        // sentinel zero — is what keeps an adopted transcript from claiming it occurred
        // at the moment it was adopted.
        let created_at_ms = source_at_ms.unwrap_or(arrival_at_ms);
        Self {
            seq: 0,
            arrival_turn_id: None,
            kind: event.kind.as_str().to_string(),
            phase: event.phase.map(|phase| phase.as_str().to_string()),
            stream_id: event.stream_id.clone(),
            // Empty is normalized to absent: an adapter setting `span` to `""` for a
            // root event means the root, and a client comparing against a trajectory id
            // would otherwise look for one named "".
            span: event.span.clone().filter(|span| !span.is_empty()),
            interior_state: event.interior_state,
            name: event.name.clone(),
            text: event.text.clone(),
            data: event.data.clone().filter(|data| !data.is_null()),
            is_error: event.is_error,
            stability: match stability {
                sylph_protocol::Stability::Stable => "stable",
                sylph_protocol::Stability::Ephemeral => "ephemeral",
            }
            .to_string(),
            source: "agent".to_string(),
            arrival_at_ms,
            recorded_at_ms,
            created_at_ms,
            started_at_ms,
            body_ref: None,
            tool_summary: None,
        }
    }

    /// Rebuild the wire form from a journal row.
    ///
    /// `turn_id` becomes `arrivalTurnId` here. No `started_at_ms`: a journal row is
    /// one arrival, so its `created_at_ms` is already the moment it happened. The
    /// field exists only for a consolidated block, which is many arrivals folded
    /// into one row.
    pub fn from_row(row: &Event) -> Self {
        Self {
            seq: row.seq,
            arrival_turn_id: row.turn_id.clone(),
            kind: row.kind.clone(),
            phase: row.phase.clone(),
            stream_id: row.stream_id.clone(),
            span: row.span.clone(),
            interior_state: row.interior_state.as_deref().and_then(parse_interior_state),
            name: row.name.clone(),
            text: row.text.clone(),
            data: data_from_column(&row.data_json),
            is_error: row.is_error,
            stability: row.stability.clone(),
            source: row.source.clone(),
            arrival_at_ms: if row.arrival_at_ms == 0 {
                row.created_at_ms
            } else {
                row.arrival_at_ms
            },
            recorded_at_ms: row.recorded_at_ms,
            created_at_ms: row.created_at_ms,
            started_at_ms: None,
            body_ref: None,
            tool_summary: None,
        }
    }
}

fn parse_interior_state(value: &str) -> Option<InteriorState> {
    InteriorState::parse(value)
}

/// Live fan-out for one in-flight turn.
struct TurnChannel {
    tx: broadcast::Sender<StreamEvent>,
    /// Bounded tail of ephemeral events, for mid-stream reconnects.
    projection: VecDeque<StreamEvent>,
    /// Set once the turn reaches a terminal state, so late subscribers learn
    /// the stream is finished instead of waiting forever.
    finished: bool,
}

/// A session-level trace channel. Unlike the session-facts channel below, this carries
/// every turn event and keeps the ephemeral tail needed for reconnects.
struct TraceChannel {
    tx: broadcast::Sender<StreamEvent>,
    projection: VecDeque<StreamEvent>,
}

/// Registry of live event channels, keyed by turn id, plus session-scoped ones.
#[derive(Clone, Default)]
pub struct EventHub {
    channels: Arc<RwLock<HashMap<String, TurnChannel>>>,
    /// Session-scoped channels, for facts that are not about a turn.
    ///
    /// A link attaching or dying, a capability list changing, and a request being
    /// raised all belong to the session: they can happen with no turn running, and
    /// a client watching only turns would miss an approval raised while it was
    /// idle. Kept separate from turn channels rather than folded in, because
    /// their lifetimes differ — a session channel outlives every turn on it.
    sessions: Arc<RwLock<HashMap<String, broadcast::Sender<StreamEvent>>>>,
    /// Complete session traces, including events from turns that have already settled.
    /// Kept separate from `sessions`: that channel is a notification stream for current
    /// state and deliberately has no replay semantics.
    traces: Arc<RwLock<HashMap<String, TraceChannel>>>,
}

/// What an SSE handler needs to serve one client: the backlog to replay first,
/// then a live subscription (absent when the turn has already finished).
pub struct Attachment {
    pub replay: Vec<StreamEvent>,
    pub live: Option<broadcast::Receiver<StreamEvent>>,
    pub finished: bool,
}

/// A session trace attachment: a projection snapshot plus a live receiver. Stable
/// history is assembled by the HTTP handler from the two durable storage tiers.
pub struct TraceAttachment {
    pub projection: Vec<StreamEvent>,
    pub live: broadcast::Receiver<StreamEvent>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a channel for a turn that is about to start.
    pub async fn open(&self, turn_id: &str) {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        self.channels.write().await.insert(
            turn_id.to_string(),
            TurnChannel {
                tx,
                projection: VecDeque::with_capacity(PROJECTION_CAPACITY),
                finished: false,
            },
        );
    }

    /// Publish an event to attached clients, retaining ephemeral ones in the
    /// bounded projection. Persisting stable events is the caller's job.
    pub async fn publish(&self, turn_id: &str, event: StreamEvent) {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(turn_id) {
            if !event.is_stable() {
                if channel.projection.len() == PROJECTION_CAPACITY {
                    channel.projection.pop_front();
                }
                channel.projection.push_back(event.clone());
            }
            // A send error only means nobody is listening right now, which is
            // normal: the turn runs independently of any HTTP request.
            let _ = channel.tx.send(event);
        }
    }

    /// Mark a turn's stream closed. The channel is retained briefly so clients
    /// still draining the backlog see the terminal status event.
    pub async fn close(&self, turn_id: &str) {
        if let Some(channel) = self.channels.write().await.get_mut(turn_id) {
            channel.finished = true;
            channel.projection.clear();
        }
    }

    /// Drop a finished turn's channel entirely.
    pub async fn remove(&self, turn_id: &str) {
        self.channels.write().await.remove(turn_id);
    }

    /// Publish a session-scoped fact.
    ///
    /// Created on demand: unlike a turn, a session has no explicit open, and
    /// requiring one would mean a request raised before any client subscribed had
    /// nowhere to go.
    pub async fn publish_session(&self, session_id: &str, event: StreamEvent) {
        let sender = {
            let mut sessions = self.sessions.write().await;
            sessions
                .entry(session_id.to_string())
                .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                .clone()
        };
        // No listener is normal and not an error: these facts are journaled or
        // available as resources, and the stream is only the fast path.
        let _ = sender.send(event);
    }

    /// Subscribe to a session's facts.
    pub async fn attach_session(&self, session_id: &str) -> broadcast::Receiver<StreamEvent> {
        let mut sessions = self.sessions.write().await;
        sessions
            .entry(session_id.to_string())
            .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
            .subscribe()
    }

    /// Publish an event on the complete session trace. Stable events are replayed from
    /// storage; ephemeral events stay in this bounded projection until consolidation.
    pub async fn publish_trace(&self, session_id: &str, event: StreamEvent) {
        let mut traces = self.traces.write().await;
        let channel = traces.entry(session_id.to_string()).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
            TraceChannel {
                tx,
                projection: VecDeque::with_capacity(PROJECTION_CAPACITY),
            }
        });
        if !event.is_stable() {
            if channel.projection.len() == PROJECTION_CAPACITY {
                channel.projection.pop_front();
            }
            channel.projection.push_back(event.clone());
        }
        let _ = channel.tx.send(event);
    }

    /// Attach to a complete session trace. The projection snapshot and live receiver are
    /// created under one lock so an event cannot fall into the replay/live gap.
    pub async fn attach_trace(&self, session_id: &str, after_seq: i32) -> TraceAttachment {
        let mut traces = self.traces.write().await;
        let channel = traces.entry(session_id.to_string()).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
            TraceChannel {
                tx,
                projection: VecDeque::with_capacity(PROJECTION_CAPACITY),
            }
        });
        TraceAttachment {
            projection: channel
                .projection
                .iter()
                .filter(|event| event.seq > after_seq)
                .cloned()
                .collect(),
            live: channel.tx.subscribe(),
        }
    }

    /// Attach a client, replaying ephemeral events newer than `after_seq`.
    ///
    /// Subscription happens under the same lock as the projection read, so no
    /// event can slip between the replay snapshot and the live stream.
    pub async fn attach(&self, turn_id: &str, after_seq: i32) -> Attachment {
        let channels = self.channels.read().await;
        match channels.get(turn_id) {
            Some(channel) if !channel.finished => Attachment {
                replay: channel
                    .projection
                    .iter()
                    .filter(|event| event.seq > after_seq)
                    .cloned()
                    .collect(),
                live: Some(channel.tx.subscribe()),
                finished: false,
            },
            Some(_) => Attachment {
                replay: Vec::new(),
                live: None,
                finished: true,
            },
            None => Attachment {
                replay: Vec::new(),
                live: None,
                finished: true,
            },
        }
    }

    /// Whether a live channel exists for this turn.
    pub async fn is_live(&self, turn_id: &str) -> bool {
        self.channels
            .read()
            .await
            .get(turn_id)
            .is_some_and(|channel| !channel.finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trajectory_pointer_survives_the_wire_in_both_directions() {
        // Both halves matter and they fail differently: a parse miss silently
        // flattens the tree, and a serialize miss lets the server hold structure it
        // never sends — which looks like a frontend bug because the journal is
        // right and the client is empty.
        let mut source = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Assistant,
            Some(sylph_protocol::Phase::Completed),
            0,
        );
        source.stream_id = Some("b1".to_string());
        source.span = Some("s1".to_string());
        source.text = "hi".to_string();

        let parsed = StreamEvent::from_event(&source);
        assert_eq!(parsed.span.as_deref(), Some("s1"));

        let wire = serde_json::to_value(&parsed).expect("serializes");
        assert_eq!(wire.get("span").and_then(|v| v.as_str()), Some("s1"));

        // A root event says nothing rather than saying "".
        let mut empty = sylph_protocol::Event::new(sylph_protocol::EventKind::Assistant, None, 0);
        empty.span = Some(String::new());
        let root = StreamEvent::from_event(&empty);
        assert_eq!(root.span, None);
        assert!(serde_json::to_value(&root).unwrap().get("span").is_none());
    }

    /// A kind this build does not model is preserved rather than dropped.
    ///
    /// The JSON constructor this replaced answered `None` for anything it could not read a
    /// `kind` off, and its one call site logged and dropped the event. With a typed enum
    /// that state is unreachable: an unrecognized spelling lands in `EventKind::Other` and
    /// reaches the journal verbatim, which is what keeps the vocabulary additive.
    #[test]
    fn an_unmodelled_kind_survives_as_itself() {
        let source = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Other("plan".to_string()),
            Some(sylph_protocol::Phase::Completed),
            0,
        );
        let event = StreamEvent::from_event(&source);
        assert_eq!(event.kind, "plan");
        assert!(event.is_stable(), "a completed event reaches the journal");
    }

    #[test]
    fn a_trajectory_is_journaled_at_every_phase() {
        // `started` is where a trajectory's id, name, and parent live. Dropped as an
        // ephemeral projection, the journal keeps children pointing at a parent it
        // never recorded, and a reload flattens the subtree onto the root.
        use sylph_protocol::Phase;
        for phase in [Phase::Started, Phase::Delta, Phase::Completed] {
            let mut source =
                sylph_protocol::Event::new(sylph_protocol::EventKind::Subagent, Some(phase), 0);
            source.stream_id = Some("s1".to_string());
            let event = StreamEvent::from_event(&source);
            assert_eq!(event.stability, "stable", "subagent/{phase:?}");
            assert!(
                event.is_stable(),
                "subagent/{phase:?} must reach the journal"
            );
        }

        // The exception is scoped to structure: content still projects.
        let mut source =
            sylph_protocol::Event::new(sylph_protocol::EventKind::Assistant, Some(Phase::Delta), 0);
        source.stream_id = Some("b1".to_string());
        let delta = StreamEvent::from_event(&source);
        assert_eq!(delta.stability, "ephemeral");
    }

    fn event(seq: i32, kind: &str, phase: Option<&str>) -> StreamEvent {
        StreamEvent {
            seq,
            arrival_turn_id: None,
            kind: kind.to_string(),
            phase: phase.map(str::to_string),
            stream_id: None,
            span: None,
            interior_state: None,
            name: None,
            text: String::new(),
            data: None,
            is_error: false,
            stability: if matches!(phase, Some("completed") | None) {
                "stable".to_string()
            } else {
                "ephemeral".to_string()
            },
            source: "runner".to_string(),
            arrival_at_ms: 1_700_000_000_000,
            recorded_at_ms: None,
            created_at_ms: 1_700_000_000_000,
            started_at_ms: None,
            body_ref: None,
            tool_summary: None,
        }
    }

    #[tokio::test]
    async fn session_trace_keeps_ephemeral_tail_and_live_delivery() {
        let hub = EventHub::new();
        let mut first = hub.attach_trace("session", -1).await.live;
        let mut delta = event(1, "assistant", Some("delta"));
        delta.arrival_turn_id = Some("turn-1".to_string());
        hub.publish_trace("session", delta.clone()).await;
        assert_eq!(first.recv().await.unwrap().seq, 1);

        // A later subscriber can recover the projection even though the event was not
        // durable yet. This is the reconnect property a turn-scoped channel cannot give
        // a child that outlives its parent turn.
        let attachment = hub.attach_trace("session", -1).await;
        assert_eq!(attachment.projection.len(), 1);
        assert_eq!(
            attachment.projection[0].arrival_turn_id.as_deref(),
            Some("turn-1")
        );
    }

    /// A live adapter's `stability` is re-derived, never trusted.
    ///
    /// [`sylph_protocol::Event::stability`] is a public field, so an adapter can set it to
    /// anything after `Event::new` derived it — and the value decides which tier a row
    /// lands in, so a wrong one keeps a delta out of the journal or floods it with one.
    /// Written because the property had **no** coverage: replacing the `derive_stability`
    /// call in [`StreamEvent::from_event`] with `event.stability` left the whole workspace
    /// suite passing, so the rule its comment calls load-bearing could have been deleted
    /// silently. Verified by making that substitution and watching this test fail.
    ///
    /// [`StreamEvent::from_protocol`] deliberately does the opposite — a parsed native
    /// record has no author but this process — which is why this asserts on the live path
    /// only.
    #[test]
    fn a_live_adapter_cannot_declare_a_delta_stable() {
        let mut claimed = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Assistant,
            Some(sylph_protocol::Phase::Delta),
            1_700_000_000_000,
        );
        claimed.stability = sylph_protocol::Stability::Stable;

        let wire = StreamEvent::from_event(&claimed);
        assert!(
            !wire.is_stable(),
            "a delta must stay ephemeral however the adapter labelled it"
        );

        // The same value read as an adopted record keeps what the parser derived, so the
        // two paths are genuinely different rather than one being dead.
        assert!(StreamEvent::from_protocol(&claimed).is_stable());
    }

    #[test]
    fn live_and_native_events_expose_distinct_time_axes() {
        let live_source = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Assistant,
            Some(sylph_protocol::Phase::Completed),
            1_000,
        );
        let live = StreamEvent::from_event(&live_source);
        assert_eq!(live.recorded_at_ms, None);
        assert_eq!(live.created_at_ms, live.arrival_at_ms);

        let native_source = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Assistant,
            Some(sylph_protocol::Phase::Completed),
            1_000,
        );
        let native = StreamEvent::from_protocol(&native_source);
        assert_eq!(native.recorded_at_ms, Some(1_000));
        assert_eq!(native.created_at_ms, 1_000);
        assert!(native.arrival_at_ms >= native.created_at_ms);

        let untimed = sylph_protocol::Event::new(
            sylph_protocol::EventKind::Subagent,
            Some(sylph_protocol::Phase::Completed),
            0,
        );
        let untimed = StreamEvent::from_protocol(&untimed);
        assert_eq!(untimed.recorded_at_ms, None);
        assert_eq!(untimed.created_at_ms, untimed.arrival_at_ms);
    }

    #[test]
    fn completed_and_phaseless_events_are_stable() {
        assert!(event(1, "assistant", Some("completed")).is_stable());
        // Server-authored events carry no phase and are always durable.
        assert!(event(2, "status", None).is_stable());
        assert!(!event(3, "assistant", Some("delta")).is_stable());
        assert!(!event(4, "assistant", Some("started")).is_stable());
    }

    #[test]
    fn a_row_round_trips_through_the_wire_form() {
        let mut original = event(7, "tool_call", Some("completed"));
        original.stream_id = Some("blk-1".to_string());
        original.name = Some("Read".to_string());
        original.text = "reading a file".to_string();
        original.data = Some(serde_json::json!({"path": "/tmp/x"}));
        original.is_error = true;

        let restored = StreamEvent::from_row(
            &original.to_row(&crate::fixtures::test_session_id("sess-1"), "turn-1"),
        );

        assert_eq!(restored.seq, 7);
        assert_eq!(restored.kind, "tool_call");
        assert_eq!(restored.phase.as_deref(), Some("completed"));
        assert_eq!(restored.stream_id.as_deref(), Some("blk-1"));
        assert_eq!(restored.name.as_deref(), Some("Read"));
        assert_eq!(restored.text, "reading a file");
        assert_eq!(restored.data, Some(serde_json::json!({"path": "/tmp/x"})));
        assert!(restored.is_error);
        assert_eq!(restored.created_at_ms, 1_700_000_000_000);
        assert_eq!(restored.arrival_at_ms, 1_700_000_000_000);
        assert_eq!(restored.recorded_at_ms, None);
    }

    #[test]
    fn to_row_carries_the_session_and_turn_ids() {
        let row =
            event(1, "status", None).to_row(&crate::fixtures::test_session_id("sess-9"), "turn-9");
        assert_eq!(row.session_id.native_id.as_str(), "sess-9");
        assert_eq!(row.turn_id.as_deref(), Some("turn-9"));
        assert!(row.id.is_none(), "a new row has no assigned id yet");
    }

    #[test]
    fn absent_data_persists_as_an_empty_object_and_reads_back_as_none() {
        // The column is NOT NULL, so absent data is stored as "{}" rather than
        // an empty string. Both spellings must read back as None so a row
        // written by an older build does not resurface as Some({}).
        let row = event(1, "status", None).to_row(&crate::fixtures::test_session_id("s"), "t");
        assert_eq!(row.data_json, "{}");
        assert!(StreamEvent::from_row(&row).data.is_none());

        let mut legacy = row.clone();
        legacy.data_json = String::new();
        assert!(StreamEvent::from_row(&legacy).data.is_none());
    }

    #[test]
    fn malformed_stored_data_reads_back_as_none_rather_than_failing() {
        // A corrupt data column must not make an otherwise readable event
        // unrenderable; the text and kind still carry the useful content.
        let mut row = event(1, "assistant", Some("completed"))
            .to_row(&crate::fixtures::test_session_id("s"), "t");
        row.data_json = "{not json".to_string();
        assert!(StreamEvent::from_row(&row).data.is_none());
    }

    #[test]
    fn stream_id_is_exposed_as_stream_id_on_the_wire() {
        let mut e = event(1, "assistant", Some("delta"));
        e.stream_id = Some("blk-2".to_string());
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["streamId"], "blk-2");
        assert!(
            json.get("stream_id").is_none(),
            "snake_case must not leak to clients"
        );
    }

    #[test]
    fn empty_optional_fields_are_omitted_from_the_wire_form() {
        let json = serde_json::to_value(event(1, "status", None)).unwrap();
        for absent in ["phase", "streamId", "name", "text", "data", "isError"] {
            assert!(json.get(absent).is_none(), "{absent} should be omitted");
        }
        assert_eq!(json["seq"], 1);
        assert_eq!(json["kind"], "status");
    }

    #[tokio::test]
    async fn ephemeral_events_replay_from_the_projection_after_the_given_seq() {
        let hub = EventHub::new();
        hub.open("t1").await;
        for seq in 1..=5 {
            hub.publish("t1", event(seq, "assistant", Some("delta")))
                .await;
        }

        let attachment = hub.attach("t1", 3).await;
        let seqs: Vec<i32> = attachment.replay.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![4, 5], "only events newer than after_seq replay");
        assert!(attachment.live.is_some());
        assert!(!attachment.finished);
    }

    #[tokio::test]
    async fn stable_events_are_not_kept_in_the_projection() {
        // The journal is authoritative for stable events. Buffering them here
        // too would replay them twice on reconnect.
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.publish("t1", event(1, "assistant", Some("completed")))
            .await;
        hub.publish("t1", event(2, "status", None)).await;
        hub.publish("t1", event(3, "assistant", Some("delta")))
            .await;

        let replay = hub.attach("t1", 0).await.replay;
        let seqs: Vec<i32> = replay.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3]);
    }

    #[tokio::test]
    async fn the_projection_drops_its_oldest_events_past_capacity() {
        let hub = EventHub::new();
        hub.open("t1").await;
        let overflow = PROJECTION_CAPACITY as i32 + 10;
        for seq in 1..=overflow {
            hub.publish("t1", event(seq, "assistant", Some("delta")))
                .await;
        }

        let replay = hub.attach("t1", 0).await.replay;
        assert_eq!(replay.len(), PROJECTION_CAPACITY);
        assert_eq!(
            replay.first().unwrap().seq,
            11,
            "oldest events were dropped"
        );
        assert_eq!(replay.last().unwrap().seq, overflow);
    }

    #[tokio::test]
    async fn a_live_subscriber_receives_events_published_after_it_attaches() {
        let hub = EventHub::new();
        hub.open("t1").await;
        let mut live = hub.attach("t1", 0).await.live.expect("live channel");

        hub.publish("t1", event(1, "assistant", Some("delta")))
            .await;
        let received = live.recv().await.expect("event delivered");
        assert_eq!(received.seq, 1);
    }

    #[tokio::test]
    async fn publishing_without_a_listener_is_not_an_error() {
        // A turn runs independently of any HTTP request, so nobody listening is
        // the normal case rather than a failure.
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.publish("t1", event(1, "assistant", Some("delta")))
            .await;
        assert!(hub.is_live("t1").await);
    }

    #[tokio::test]
    async fn attaching_to_a_closed_turn_reports_finished_instead_of_hanging() {
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.publish("t1", event(1, "assistant", Some("delta")))
            .await;
        hub.close("t1").await;

        let attachment = hub.attach("t1", 0).await;
        assert!(attachment.finished);
        assert!(
            attachment.live.is_none(),
            "no live channel for a closed turn"
        );
        assert!(
            attachment.replay.is_empty(),
            "closing clears the projection"
        );
        assert!(!hub.is_live("t1").await);
    }

    #[tokio::test]
    async fn attaching_to_an_unknown_turn_reports_finished() {
        let hub = EventHub::new();
        let attachment = hub.attach("never-existed", 0).await;
        assert!(attachment.finished);
        assert!(attachment.live.is_none());
        assert!(!hub.is_live("never-existed").await);
    }

    #[tokio::test]
    async fn publishing_to_an_unopened_turn_is_dropped_silently() {
        let hub = EventHub::new();
        hub.publish("t1", event(1, "assistant", Some("delta")))
            .await;
        assert!(!hub.is_live("t1").await);
    }

    #[tokio::test]
    async fn removing_a_turn_drops_its_channel() {
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.remove("t1").await;
        assert!(!hub.is_live("t1").await);
        assert!(hub.attach("t1", 0).await.finished);
    }

    #[tokio::test]
    async fn channels_are_isolated_per_turn() {
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.open("t2").await;
        hub.publish("t1", event(1, "assistant", Some("delta")))
            .await;

        assert_eq!(hub.attach("t1", 0).await.replay.len(), 1);
        assert!(hub.attach("t2", 0).await.replay.is_empty());

        hub.close("t1").await;
        assert!(
            hub.is_live("t2").await,
            "closing one turn must not close another"
        );
    }
}
