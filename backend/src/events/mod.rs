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
    #[serde(default, alias = "is_error")]
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
