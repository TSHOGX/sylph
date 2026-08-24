//! Replaying a session's stream as it originally arrived.
//!
//! Server-side rather than per frontend, and `docs/storage.md` argues why:
//! reconstruction is subtle — UTF-8 boundaries mid-codepoint, interleaving two
//! stores by sequence number, timing accumulation — and a rule implemented once
//! per frontend is one that will differ per frontend. Discord, a terminal, and the
//! browser would then disagree about what happened, which is the exact failure a
//! single authoritative record exists to prevent.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::events::StreamEvent;
use crate::storage::HistoryStore;
use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayQuery {
    #[serde(default = "default_from_seq")]
    from_seq: i32,
    /// Playback rate. `1` is original pacing; `0` emits everything at once, which
    /// is what a timeline render wants.
    #[serde(default = "default_speed")]
    speed: f32,
}

fn default_from_seq() -> i32 {
    -1
}

fn default_speed() -> f32 {
    0.0
}

/// The longest this endpoint will sleep between two events.
///
/// A user who steps away mid-session leaves a minutes-long gap in the record.
/// Replaying that faithfully would hold a connection open doing nothing, so gaps
/// are compressed to this — the pacing is meant to convey how the work unfolded,
/// not to reproduce the coffee break.
const MAX_GAP: Duration = Duration::from_secs(2);

/// GET /api/v1/sessions/{id}/replay
///
/// Emits the session's stable events, and every delta that built them, in original
/// order with original relative timing.
pub async fn replay(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<ReplayQuery>,
) -> Result<impl IntoResponse> {
    state
        .store
        .get_session(&session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    // Both tiers, merged by sequence number. A session mid-turn has its tail in
    // the journal and everything before it in history, and `seq` is what orders
    // the two — which is why one allocator hands them out for the whole session.
    let mut frames: Vec<Frame> = Vec::new();

    for block in state
        .history
        .blocks(&session_id, query.from_seq)
        .await
        .unwrap_or_default()
    {
        let deltas = HistoryStore::replay_block(&block);
        let event = crate::api::sessions::block_to_event(&block);

        // A block that streamed replays as its deltas and then its settled value,
        // which is exactly the sequence a live client saw. One that never streamed
        // has only the settled value.
        if block.folded.is_some() {
            for delta in deltas {
                frames.push(Frame {
                    at_ms: delta.at_ms,
                    seq: block.seq,
                    event: StreamEvent {
                        phase: Some("delta".to_string()),
                        text: delta.text,
                        stability: "ephemeral".to_string(),
                        ..event.clone()
                    },
                });
            }
        }
        frames.push(Frame { at_ms: block.created_at_ms, seq: block.seq, event });
    }

    for row in state
        .store
        .session_events_after(&session_id, query.from_seq)
        .await?
    {
        let event = StreamEvent::from_row(&row);
        frames.push(Frame { at_ms: event.created_at_ms, seq: event.seq, event });
    }

    // By sequence first: it is the authority on order, and two events can share a
    // millisecond. Arrival time only breaks ties within one block's deltas, which
    // all carry their block's seq.
    frames.sort_by_key(|frame| (frame.seq, frame.at_ms));

    let speed = query.speed.max(0.0);
    let stream = async_stream::stream! {
        let mut previous: Option<i64> = None;
        for frame in frames {
            if speed > 0.0 {
                if let Some(previous) = previous {
                    let gap = frame.at_ms.saturating_sub(previous).max(0) as f64;
                    let scaled = Duration::from_secs_f64((gap / speed as f64) / 1000.0);
                    tokio::time::sleep(scaled.min(MAX_GAP)).await;
                }
                previous = Some(frame.at_ms);
            }

            let data = serde_json::to_string(&frame.event).unwrap_or_default();
            yield Ok::<Event, Infallible>(Event::default().event(&frame.event.kind).data(data));
        }
    };

    Ok((
        // `no-transform` for the same reason the turn stream carries it: a proxy
        // that gzips an SSE body buffers it invisibly, and the whole replay then
        // lands at once while `curl` sees perfect pacing.
        [
            ("Cache-Control", "no-cache, no-store, no-transform"),
            ("X-Accel-Buffering", "no"),
        ],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    ))
}

/// One event plus when it arrived, before pacing is applied.
struct Frame {
    at_ms: i64,
    seq: i32,
    event: StreamEvent,
}
