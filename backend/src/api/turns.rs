use tokio_stream::wrappers::BroadcastStream;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{sse::{Event, KeepAlive}, Sse},
    Json,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{self as stream, StreamExt};

use crate::{
    attachments,
    error::{Error, Result},
    events::{Attachment, StreamEvent},
    models::{CreateTurnRequest, Incarnation, Turn},
    supervisor::TurnRequest,
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
    if state.supervisor.session_busy(&session_id).await {
        return Err(Error::SessionAlreadyRunning);
    }

    let text = req.text.unwrap_or_default();
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

    let incarnation = match state.store.active_incarnation(&session_id).await? {
        Some(inc) => inc,
        None => {
            let inc = Incarnation {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                agent: session.agent.clone(),
                backend_session_id: None,
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                ended_at_ms: None,
                reset_reason: None,
                source_trace_session_id: session.trace_session_id.clone(),
                resulting_trace_session_id: None,
                status: "active".to_string(),
            };
            state.store.create_incarnation(&inc).await?
        }
    };

    let now = chrono::Utc::now().timestamp_millis();
    let turn = Turn {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        incarnation_id: Some(incarnation.id.clone()),
        status: "queued".to_string(),
        request_text: text.clone(),
        command_invocation_id: req.command.clone(),
        idempotency_key: idempotency_key.clone(),
        error_code: None,
        error_message: None,
        runner_exit_code: None,
        created_at_ms: now,
        started_at_ms: None,
        completed_at_ms: None,
        stopped_at_ms: None,
    };

    let created = state.store.create_turn(&turn).await?;

    let inject_soul = session.identity_mode == "sylph";

    state
        .supervisor
        .spawn(TurnRequest {
            session_id: session_id.clone(),
            turn_id: created.id.clone(),
            incarnation_id: incarnation.id.clone(),
            agent: session.agent.clone(),
            workdir: session.workdir.clone(),
            backend_session_id: incarnation.backend_session_id.clone(),
            prompt: text,
            flags,
            attachments: staged_attachments,
            command: req.command,
            command_args: req.args.unwrap_or_default(),
            inject_soul,
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
) -> Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    // Existence check only: a stream for an unknown turn must 404 rather
    // than hang on an empty subscription.
    let _turn = state
        .store
        .get_turn(&turn_id)
        .await?
        .ok_or(Error::TurnNotFound)?;

    let after_seq = query.after_seq;

    // Replay stable events from the journal first.
    let journal_events: Vec<StreamEvent> = state
        .store
        .turn_events_after(&turn_id, after_seq)
        .await?
        .into_iter()
        .map(|row| StreamEvent::from_row(&row))
        .collect();

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

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
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

    if !state.supervisor.stop(&turn_id).await? {
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
