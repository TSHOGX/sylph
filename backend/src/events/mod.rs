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

/// How many ephemeral events to retain per turn for reconnect replay.
///
/// Deltas arrive at token rate, so this is a tail window rather than a full
/// history: a client that reconnects promptly resumes seamlessly, and one that
/// reconnects late still gets every stable event from the journal.
const PROJECTION_CAPACITY: usize = 512;

/// Broadcast capacity per turn. Slow SSE consumers that fall behind receive a
/// lag error and resubscribe from the journal rather than blocking the runner.
const BROADCAST_CAPACITY: usize = 1024;

/// A normalized event as it appears on the wire, both from runners
/// (`sylph.runner.v1` NDJSON lines) and to browsers (SSE `data:` payloads).
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
}

impl StreamEvent {
    /// True when this event belongs in the durable journal.
    ///
    /// `completed` events carry the whole stable value for their block, so they
    /// are the journal's unit of record. Events with no phase at all are
    /// server-authored (status, error, the user's own request) and are always
    /// stable.
    pub fn is_stable(&self) -> bool {
        matches!(self.phase.as_deref(), Some("completed") | None)
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

    /// Rebuild the wire form from a journal row.
    pub fn from_row(row: &Event) -> Self {
        Self {
            seq: row.seq,
            kind: row.kind.clone(),
            phase: row.phase.clone(),
            stream_id: row.stream_id.clone(),
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
        }
    }
}

/// The `event` object inside a `sylph.runner.v1` NDJSON line.
///
/// Only `kind` and `phase` are required by the protocol; everything else is
/// optional and must survive being absent. Unknown fields are tolerated so a
/// newer runner can add them without breaking this consumer.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerEvent {
    pub kind: String,
    pub phase: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    /// The protocol spells this `isError` and every shipped runner emits that
    /// form; the snake_case alias is kept for hand-written or older runners.
    #[serde(default, alias = "isError")]
    pub is_error: Option<bool>,
}

/// A full `sylph.runner.v1` NDJSON line.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerStreamLine {
    pub stream: String,
    pub event: RunnerEvent,
}

/// The runner protocol's final envelope: the last non-empty stdout line.
///
/// Authoritative for the turn outcome, the assistant reply, and the session id
/// to store — the protocol says to trust this over the exit code.
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerEnvelope {
    pub ok: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
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

/// Registry of live event channels, keyed by turn id.
#[derive(Clone, Default)]
pub struct EventHub {
    channels: Arc<RwLock<HashMap<String, TurnChannel>>>,
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

    fn event(seq: i32, kind: &str, phase: Option<&str>) -> StreamEvent {
        StreamEvent {
            seq,
            kind: kind.to_string(),
            phase: phase.map(str::to_string),
            stream_id: None,
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
