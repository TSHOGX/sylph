//! Normalized event plumbing between runner processes and SSE clients.
//!
//! Two storage tiers back a turn's event stream, mirroring the split described
//! in `docs/architecture.md`:
//!
//! * **Stable events** (`phase == "completed"`, plus server-authored status and
//!   error events) are persisted to `live.db` and replayed on reconnect.
//! * **Ephemeral events** (`started` / `delta`) are transient projections. They
//!   are broadcast to attached clients and kept in a bounded in-memory ring so
//!   a client that reconnects mid-stream can still catch the tail, but they are
//!   never written to the journal.
//!
//! Sequence numbers come from one monotonic per-session counter, so stable and
//! ephemeral events share a single ordering. The journal therefore contains
//! gaps where ephemeral events were dropped, which is expected and allowed by
//! the `UNIQUE(session_id, seq)` constraint.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

use crate::models::Event;

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

/// A normalized event as it appears on the wire, both from adapters
/// (`sylph.agent.v3` event messages) and to browsers (SSE `data:` payloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    pub seq: i32,
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
    pub fn to_row(&self, session_id: &str, turn_id: &str) -> Event {
        Event {
            id: None,
            session_id: session_id.to_string(),
            turn_id: Some(turn_id.to_string()),
            seq: self.seq,
            kind: self.kind.clone(),
            phase: self.phase.clone(),
            stream_id: self.stream_id.clone(),
            span: self.span.clone(),
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
            created_at_ms: self.created_at_ms,
            reconciled_at_ms: None,
        }
    }

    /// Build from a `sylph.agent.v3` event message.
    ///
    /// `seq` is left at zero for the caller to assign, because ordering is the
    /// server's to decide: an adapter numbering its own events could not
    /// interleave correctly with the server's own (the user's message, status),
    /// which share one stream.
    ///
    /// Returns `None` only when `kind` is missing. Every other field is optional
    /// by protocol, so a sparse event is valid rather than malformed.
    pub fn from_adapter(body: &serde_json::Value) -> Option<Self> {
        let kind = body.get("kind")?.as_str()?.to_string();
        let phase = body
            .get("phase")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());

        // Stability follows the phase, unchanged from v1: a `completed` event
        // carries its block's whole stable value, and an event with no phase is
        // server-authored and always stable.
        //
        // `subagent` is the documented exception, at every phase. It records
        // *structure*, not content: its `started` phase is where a trajectory's
        // id, name, and parent live. Dropped as an ephemeral projection, the
        // journal would keep every child event pointing at a parent it never
        // recorded, and a reload would flatten the whole delegated subtree into
        // its parent's own work — the exact bug `span` exists to fix, reappearing
        // via the storage tier instead of the protocol.
        let stability = match phase.as_deref() {
            Some("completed") | None => "stable",
            _ if kind == "subagent" => "stable",
            _ => "ephemeral",
        };

        Some(Self {
            seq: 0,
            kind,
            phase,
            stream_id: body
                .get("streamId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Empty is normalized to absent: an adapter emitting `"span":""` for a
            // root event means the root, and a client comparing against a
            // trajectory id would otherwise look for one named "".
            span: body
                .get("span")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            name: body
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            text: body
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            data: body.get("data").cloned().filter(|d| !d.is_null()),
            is_error: body
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            stability: stability.to_string(),
            source: "agent".to_string(),
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            // Straight from an adapter envelope: one arrival.
            started_at_ms: None,
        })
    }

    /// Rebuild the wire form from a journal row.
    ///
    /// No `started_at_ms`: a journal row is one arrival, so its `created_at_ms` is
    /// already the moment it happened. The field exists only for a consolidated
    /// block, which is many arrivals folded into one row.
    pub fn from_row(row: &Event) -> Self {
        Self {
            seq: row.seq,
            kind: row.kind.clone(),
            phase: row.phase.clone(),
            stream_id: row.stream_id.clone(),
            span: row.span.clone(),
            name: row.name.clone(),
            text: row.text.clone(),
            data: match row.data_json.as_str() {
                "" | "{}" => None,
                raw => serde_json::from_str(raw).ok(),
            },
            is_error: row.is_error,
            stability: row.stability.clone(),
            source: row.source.clone(),
            created_at_ms: row.created_at_ms,
            started_at_ms: None,
        }
    }
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
}

/// What an SSE handler needs to serve one client: the backlog to replay first,
/// then a live subscription (absent when the turn has already finished).
pub struct Attachment {
    pub replay: Vec<StreamEvent>,
    pub live: Option<broadcast::Receiver<StreamEvent>>,
    pub finished: bool,
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
        let parsed = StreamEvent::from_adapter(&serde_json::json!({
            "kind": "assistant", "phase": "completed",
            "streamId": "b1", "span": "s1", "text": "hi"
        }))
        .expect("parses");
        assert_eq!(parsed.span.as_deref(), Some("s1"));

        let wire = serde_json::to_value(&parsed).expect("serializes");
        assert_eq!(wire.get("span").and_then(|v| v.as_str()), Some("s1"));

        // A root event says nothing rather than saying "".
        let root = StreamEvent::from_adapter(&serde_json::json!({
            "kind": "assistant", "span": ""
        }))
        .expect("parses");
        assert_eq!(root.span, None);
        assert!(serde_json::to_value(&root).unwrap().get("span").is_none());
    }

    #[test]
    fn a_trajectory_is_journaled_at_every_phase() {
        // `started` is where a trajectory's id, name, and parent live. Dropped as an
        // ephemeral projection, the journal keeps children pointing at a parent it
        // never recorded, and a reload flattens the subtree onto the root.
        for phase in ["started", "delta", "completed"] {
            let event = StreamEvent::from_adapter(&serde_json::json!({
                "kind": "subagent", "phase": phase, "streamId": "s1"
            }))
            .expect("parses");
            assert_eq!(event.stability, "stable", "subagent/{phase}");
            assert!(event.is_stable(), "subagent/{phase} must reach the journal");
        }

        // The exception is scoped to structure: content still projects.
        let delta = StreamEvent::from_adapter(&serde_json::json!({
            "kind": "assistant", "phase": "delta", "streamId": "b1"
        }))
        .expect("parses");
        assert_eq!(delta.stability, "ephemeral");
    }

    fn event(seq: i32, kind: &str, phase: Option<&str>) -> StreamEvent {
        StreamEvent {
            seq,
            kind: kind.to_string(),
            phase: phase.map(str::to_string),
            stream_id: None,
            span: None,
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
            created_at_ms: 1_700_000_000_000,
            started_at_ms: None,
        }
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

        let restored = StreamEvent::from_row(&original.to_row("sess-1", "turn-1"));

        assert_eq!(restored.seq, 7);
        assert_eq!(restored.kind, "tool_call");
        assert_eq!(restored.phase.as_deref(), Some("completed"));
        assert_eq!(restored.stream_id.as_deref(), Some("blk-1"));
        assert_eq!(restored.name.as_deref(), Some("Read"));
        assert_eq!(restored.text, "reading a file");
        assert_eq!(restored.data, Some(serde_json::json!({"path": "/tmp/x"})));
        assert!(restored.is_error);
        assert_eq!(restored.created_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn to_row_carries_the_session_and_turn_ids() {
        let row = event(1, "status", None).to_row("sess-9", "turn-9");
        assert_eq!(row.session_id, "sess-9");
        assert_eq!(row.turn_id.as_deref(), Some("turn-9"));
        assert!(row.id.is_none(), "a new row has no assigned id yet");
        assert!(row.reconciled_at_ms.is_none());
    }

    #[test]
    fn absent_data_persists_as_an_empty_object_and_reads_back_as_none() {
        // The column is NOT NULL, so absent data is stored as "{}" rather than
        // an empty string. Both spellings must read back as None so a row
        // written by an older build does not resurface as Some({}).
        let row = event(1, "status", None).to_row("s", "t");
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
        let mut row = event(1, "assistant", Some("completed")).to_row("s", "t");
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
            hub.publish("t1", event(seq, "assistant", Some("delta"))).await;
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
        hub.publish("t1", event(1, "assistant", Some("completed"))).await;
        hub.publish("t1", event(2, "status", None)).await;
        hub.publish("t1", event(3, "assistant", Some("delta"))).await;

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
            hub.publish("t1", event(seq, "assistant", Some("delta"))).await;
        }

        let replay = hub.attach("t1", 0).await.replay;
        assert_eq!(replay.len(), PROJECTION_CAPACITY);
        assert_eq!(replay.first().unwrap().seq, 11, "oldest events were dropped");
        assert_eq!(replay.last().unwrap().seq, overflow);
    }

    #[tokio::test]
    async fn a_live_subscriber_receives_events_published_after_it_attaches() {
        let hub = EventHub::new();
        hub.open("t1").await;
        let mut live = hub.attach("t1", 0).await.live.expect("live channel");

        hub.publish("t1", event(1, "assistant", Some("delta"))).await;
        let received = live.recv().await.expect("event delivered");
        assert_eq!(received.seq, 1);
    }

    #[tokio::test]
    async fn publishing_without_a_listener_is_not_an_error() {
        // A turn runs independently of any HTTP request, so nobody listening is
        // the normal case rather than a failure.
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.publish("t1", event(1, "assistant", Some("delta"))).await;
        assert!(hub.is_live("t1").await);
    }

    #[tokio::test]
    async fn attaching_to_a_closed_turn_reports_finished_instead_of_hanging() {
        let hub = EventHub::new();
        hub.open("t1").await;
        hub.publish("t1", event(1, "assistant", Some("delta"))).await;
        hub.close("t1").await;

        let attachment = hub.attach("t1", 0).await;
        assert!(attachment.finished);
        assert!(attachment.live.is_none(), "no live channel for a closed turn");
        assert!(attachment.replay.is_empty(), "closing clears the projection");
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
        hub.publish("t1", event(1, "assistant", Some("delta"))).await;
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
        hub.publish("t1", event(1, "assistant", Some("delta"))).await;

        assert_eq!(hub.attach("t1", 0).await.replay.len(), 1);
        assert!(hub.attach("t2", 0).await.replay.is_empty());

        hub.close("t1").await;
        assert!(hub.is_live("t2").await, "closing one turn must not close another");
    }
}
