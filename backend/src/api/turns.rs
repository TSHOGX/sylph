use tokio_stream::wrappers::BroadcastStream;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::{Event, KeepAlive}, Sse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{self as stream, StreamExt};
use crate::models::now_ms;

use crate::{
    attachments,
    error::{Error, Result},
    events::{Attachment, StreamEvent},
    models::{CreateTurnRequest, Turn},
    AppState,
};

#[derive(Serialize)]
pub struct TurnResponse {
    turn: Turn,
}

/// POST /api/v1/sessions/{id}/turns
///
/// Returns 202 Accepted immediately; the runner executes detached from this
/// request, and clients stream progress via SSE.
pub async fn create(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateTurnRequest>,
) -> Result<(StatusCode, Json<TurnResponse>)> {
    let session_id = crate::api::sessions::decode_id(&session_id)?;
    let session = state
        .store
        .get_session(&session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    // Idempotency key may arrive in the header or the body; header wins.
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or(req.idempotency_key.clone());

    if let Some(ref key) = idempotency_key {
        if let Some(existing) = state
            .store
            .find_turn_by_idempotency_key(&session_id, key)
            .await?
        {
            // Retried request; return the original turn.
            return Ok((StatusCode::OK, Json(TurnResponse { turn: existing })));
        }
    }

    // One turn at a time per session.
    if state.turns.session_busy(&session_id).await {
        return Err(Error::SessionAlreadyRunning);
    }

    let text = req.text.unwrap_or_default();
    if session.title.as_deref().is_none_or(|title| title.trim().is_empty()) {
        // The opening ask is the one title source available before TraceDB
        // ingestion. Persisting it here keeps a live row recognisable and makes
        // its later archived counterpart use the same label.
        state.store.set_session_title_if_empty(&session_id, &text).await?;
    }
    let attachments = req.attachments.unwrap_or_default();
    let staged_attachments = attachments::stage(
        &state.config.data_dir,
        &session_id,
        &uuid::Uuid::new_v4().to_string(),
        &attachments,
    )
    .await?;

    // Session flags may contain a model or effort choice, but `--describe`'s
    // `flags` schema is the source of truth: unknown keys are silently dropped.
    let flags = serde_json::from_str::<serde_json::Value>(&session.flags_json)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect();

    let now = now_ms();
    let turn = Turn {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        status: "queued".to_string(),
        // This endpoint is the definition of a user turn: it exists because someone
        // posted. An agent's own turn is opened by the link manager instead.
        origin: "user".to_string(),
        request_text: text.clone(),
        command_invocation_id: req.command.clone(),
        idempotency_key: idempotency_key.clone(),
        error_code: None,
        error_message: None,
        created_at_ms: now,
        started_at_ms: None,
        completed_at_ms: None,
        stopped_at_ms: None,
    };

    let created = state.store.create_turn(&turn).await?;

    state
        .turns
        .spawn(crate::supervisor::turn::TurnRequest {
            session_id: session_id.clone(),
            turn_id: created.id.clone(),
            prompt: text,
            flags,
            attachments: staged_attachments
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            command: req.command,
            command_args: req.args.unwrap_or_default(),
        })
        .await;

    Ok((StatusCode::ACCEPTED, Json(TurnResponse { turn: created })))
}

/// GET /api/v1/turns/{id}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TurnResponse>> {
    let turn = state
        .store
        .get_turn(&id)
        .await?
        .ok_or(Error::TurnNotFound)?;
    Ok(Json(TurnResponse { turn }))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default = "default_after_seq")]
    after_seq: i32,
}

fn default_after_seq() -> i32 {
    -1
}

/// GET /api/v1/turns/{id}/events — SSE stream with reconnection from journal.
pub async fn events(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
    query: axum::extract::Query<EventsQuery>,
) -> Result<impl axum::response::IntoResponse> {
    // Existence check only: a stream for an unknown turn must 404 rather
    // than hang on an empty subscription.
    let _turn = state
        .store
        .get_turn(&turn_id)
        .await?
        .ok_or(Error::TurnNotFound)?;

    let after_seq = query.after_seq;

    // Replay stable events, from whichever tier holds them. A finished turn that
    // has already been consolidated is gone from the journal, so a client
    // reconnecting to it would otherwise get an empty stream.
    let journalled = state.store.turn_events_after(&turn_id, after_seq).await?;
    let journal_events: Vec<StreamEvent> = if journalled.is_empty() {
        state
            .history
            .turn_blocks(&turn_id)
            .await
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block.seq > after_seq)
                    .map(crate::api::sessions::block_to_event)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        journalled
            .into_iter()
            .map(|row| StreamEvent::from_row(&row))
            .collect()
    };

    // One attach() call: it snapshots the ephemeral projection and subscribes
    // under a single lock. Calling it twice (once for `live`, once for `replay`)
    // reintroduces the gap it exists to close — an event published between the
    // two calls is both replayed and broadcast, so the client sees it twice.
    let Attachment { replay: projection, live, .. } = state.hub.attach(&turn_id, after_seq).await;

    let stream = if let Some(attachment) = live {
        // BroadcastStream wraps errors; unwrap them because this adapter is
        // infallible — a lagged receiver is disconnected, not surfaced.
        let live = BroadcastStream::new(attachment).filter_map(|r| r.ok());

        boxed_local(
            stream::iter(journal_events)
                .chain(stream::iter(projection))
                .chain(live)
                .map(sse_event)
        )
    } else {
        // Turn already finished; only journal replay is available.
        boxed_local(stream::iter(journal_events).map(sse_event))
    };

    let sse = Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)));

    // `no-transform` is what actually makes this stream incremental in a browser,
    // and it is not cosmetic: the Next.js dev/production server proxies
    // `/api/v1/*` through the standard `compression` middleware, which gzips any
    // compressible content type. A gzip stream cannot be decoded until its block
    // is flushed, so every event sat in the compressor until the turn ended and
    // the whole transcript then appeared at once — while `curl` (which sends no
    // `Accept-Encoding`) saw perfectly paced frames, making the bug look like a
    // client problem.
    //
    // RFC 9111 §5.2.2.6 is the lever: `no-transform` forbids an intermediary from
    // altering the payload, and `compression`'s `shouldTransform` honors it.
    // Declaring it here rather than disabling compression in `next.config.ts`
    // keeps the guarantee attached to the response that needs it, so it survives
    // any proxy in front of this endpoint — a tunnel or nginx included — instead
    // of depending on one particular front end staying configured correctly.
    //
    // Axum hard-codes `cache-control: no-cache` on `Sse`, so the header is
    // replaced rather than appended: two `cache-control` headers would leave the
    // directive's application up to whichever the intermediary reads first.
    Ok((
        [(
            axum::http::header::CACHE_CONTROL,
            "no-cache, no-store, no-transform",
        )],
        sse,
    ))
}

fn sse_event(event: StreamEvent) -> std::result::Result<Event, Infallible> {
    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Ok(Event::default()
        .event(event.kind.as_str())
        .id(event.seq.to_string())
        .data(data))
}

/// POST /api/v1/turns/{id}/stop
pub async fn stop(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
) -> Result<(StatusCode, Json<TurnResponse>)> {
    let turn = state
        .store
        .get_turn(&turn_id)
        .await?
        .ok_or(Error::TurnNotFound)?;

    if turn.is_terminal() {
        return Err(Error::StopAlreadyCompleted);
    }

    if !state.turns.stop(&turn_id).await? {
        // No live process; the turn finished between the is_terminal check and
        // now, which is a benign race with the same meaning for the client.
        return Err(Error::StopAlreadyCompleted);
    }

    state.store.mark_turn_stopping(&turn_id).await?;
    let updated = state
        .store
        .get_turn(&turn_id)
        .await?
        .ok_or(Error::TurnNotFound)?;

    Ok((StatusCode::ACCEPTED, Json(TurnResponse { turn: updated })))
}

/// Fully-qualified wrapper for [`futures::StreamExt::boxed`] to disambiguate it
/// from the tokio-stream extension of the same name.
fn boxed_local<S>(stream: S) -> futures::stream::BoxStream<'static, S::Item>
where
    S: futures::Stream + Send + 'static,
{
    use futures::StreamExt as _;
    stream.boxed()
}
