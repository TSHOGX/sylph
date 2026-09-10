use crate::models::now_ms;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive},
        Sse,
    },
    Json,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{self as stream, StreamExt};

use crate::{
    error::{Error, Result},
    events::{Attachment, StreamEvent},
    models::{CreateTurnRequest, Turn},
    AppState,
};

#[derive(Serialize)]
pub struct TurnResponse {
    turn: Turn,
}

/// POST /api/v1/sessions/draft/{id}/turns
///
/// Sends the first user turn to a prepared link and promotes it when the adapter has
/// supplied a native identity.  The regular turn endpoint remains unchanged for
/// already-committed sessions.
pub async fn create_prepared(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    Json(req): Json<CreateTurnRequest>,
) -> Result<(StatusCode, Json<crate::api::sessions::SessionResponse>)> {
    let text = req.text.unwrap_or_default();
    let attachments = req.attachments.unwrap_or_default();
    // A first turn is real work even when it has no prose: a declared prompt
    // command or an attachment is enough to promote the prepared link. Only a
    // completely empty request is still a draft, and must not leave a durable row.
    if text.trim().is_empty() && req.command.is_none() && attachments.is_empty() {
        return Err(Error::InvalidRequest(
            "A first turn needs text, a command, or an attachment".to_string(),
        ));
    }

    // A draft already owns a live envagent session.  Send its first turn through the
    // wire, create the canonical session/turn rows, and only then activate the envagent
    // event pump.
    //
    // That order is the requirement: the pump must not be able to deliver an event for a
    // session the store has no row for, because the event would arrive needing a
    // sequence number against a session that does not exist yet.  Attachment bytes stay
    // server-owned and are never written to the envagent's filesystem.
    // A client retry with an idempotency key must reuse the same first-turn
    // identity; without it, a second promotion is intentionally a conflict.
    let proposed_turn_id = req
        .idempotency_key
        .as_deref()
        .map(|key| format!("remote-turn-{}", blake3::hash(key.as_bytes()).to_hex()))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // A remote promotion is serialized per draft.  The envagent command is
    // non-idempotent (it may create a native session and consume the first
    // prompt), so two HTTP retries must never execute it concurrently.
    if let Some(previous) = state.env_bridge.remote_promotion_result(&draft_id).await {
        if previous.turn_id != proposed_turn_id {
            return Err(Error::SessionAlreadyRunning);
        }
        let session = state
            .store
            .get_session(&previous.session_id)
            .await?
            .ok_or(Error::SessionNotFound)?;
        return Ok((
            StatusCode::CREATED,
            Json(crate::api::sessions::SessionResponse { session }),
        ));
    }
    let _promotion_guard = if state.env_bridge.remote_draft_known(&draft_id).await {
        Some(
            state
                .env_bridge
                .remote_promotion_lock(&draft_id)
                .await
                .lock_owned()
                .await,
        )
    } else {
        None
    };
    if let Some(previous) = state.env_bridge.remote_promotion_result(&draft_id).await {
        if previous.turn_id != proposed_turn_id {
            return Err(Error::SessionAlreadyRunning);
        }
        let session = state
            .store
            .get_session(&previous.session_id)
            .await?
            .ok_or(Error::SessionNotFound)?;
        return Ok((
            StatusCode::CREATED,
            Json(crate::api::sessions::SessionResponse { session }),
        ));
    }
    match state
        .env_bridge
        .claim_remote_draft(&draft_id, proposed_turn_id.clone())
        .await
    {
        crate::env::bridge::RemoteDraftClaim::Claimed { draft, turn_id } => {
            if turn_id != proposed_turn_id {
                return Err(Error::SessionAlreadyRunning);
            }
            if !attachments.is_empty() {
                state.env_bridge.release_remote_draft_claim(&draft_id).await;
                return Err(Error::AttachmentInvalid(
                "attachments for a remote draft must be uploaded through its envagent files capability".into(),
            ));
            }
            let (session_id, resolved_workdir) = match state
                .env_bridge
                .run_remote_initial(
                    &draft,
                    &turn_id,
                    sylph_protocol::WireTurnInput {
                        text: text.clone(),
                        attachments: Vec::new(),
                        command: req.command.clone(),
                        args: req.args.clone().unwrap_or_default().into_iter().collect(),
                        config: Default::default(),
                    },
                )
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    state.env_bridge.discard_remote_draft(&draft_id).await;
                    return Err(Error::LinkFailed(format!("{error:#}")));
                }
            };

            let now = now_ms();
            let opening_title = if text.trim().is_empty() {
                req.command.as_ref().map(|command| format!("/{command}"))
            } else {
                Some(crate::db::labels::summarize_line(&text))
            };
            let session = crate::models::Session {
                id: session_id.clone(),
                workdir: resolved_workdir,
                title: draft
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .or(opening_title),
                inject_identity: draft.inject_identity,
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: match serde_json::to_string(&draft.flags) {
                    Ok(value) => value,
                    Err(error) => {
                        state.env_bridge.discard_remote_draft(&draft_id).await;
                        return Err(Error::Internal(format!("could not encode flags: {error}")));
                    }
                },
                status: "running".into(),
                created_at_ms: now,
                updated_at_ms: now,
            };
            let turn = Turn {
                id: turn_id.clone(),
                session_id: session_id.clone(),
                status: "running".into(),
                origin: "user".into(),
                request_text: text.clone(),
                command_invocation_id: req.command,
                idempotency_key: req.idempotency_key,
                error_code: None,
                error_message: None,
                created_at_ms: now,
                started_at_ms: Some(now),
                completed_at_ms: None,
                stopped_at_ms: None,
            };
            let (stored_session, _stored_turn) =
                match state.store.create_session_and_turn(&session, &turn).await {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = state.env_bridge.close_remote_session(&session_id).await;
                        state.env_bridge.discard_remote_draft(&draft_id).await;
                        return Err(error);
                    }
                };
            // Promotion bypasses the local supervisor, so write the same
            // server-authored transcript events before releasing the envagent
            // event pump. Otherwise the user input exists only in
            // `turns.request_text`, and the live timeline/replay tree has no
            // user row to render.
            if !text.is_empty() {
                state
                    .env_bridge
                    .emit_server_event(&session_id, &turn_id, "user", text.clone(), None)
                    .await
                    .map_err(|error| {
                        Error::Internal(format!("could not journal user event: {error:#}"))
                    })?;
            }
            state
                .env_bridge
                .emit_server_event(
                    &session_id,
                    &turn_id,
                    "status",
                    String::new(),
                    Some(serde_json::json!({"state": "starting"})),
                )
                .await
                .map_err(|error| {
                    Error::Internal(format!("could not journal status event: {error:#}"))
                })?;
            if let Err(error) = state.env_bridge.activate_remote(&session_id).await {
                let _ = state.env_bridge.close_remote_session(&session_id).await;
                state.env_bridge.discard_remote_draft(&draft_id).await;
                return Err(Error::LinkFailed(format!("{error:#}")));
            }
            state
                .env_bridge
                .remember_remote_promotion(
                    &draft_id,
                    crate::env::bridge::RemotePromotionResult {
                        turn_id: turn_id.clone(),
                        session_id: stored_session.id.clone(),
                    },
                )
                .await;
            state.env_bridge.forget_remote_draft(&draft_id).await;
            return Ok((
                StatusCode::CREATED,
                Json(crate::api::sessions::SessionResponse {
                    session: stored_session,
                }),
            ));
        }
        crate::env::bridge::RemoteDraftClaim::Conflict { .. }
        | crate::env::bridge::RemoteDraftClaim::Busy => return Err(Error::SessionAlreadyRunning),
        crate::env::bridge::RemoteDraftClaim::Missing => {
            // A remote handle must never fall through to the local
            // local execution path. A stale remote draft is a missing resource, not a
            // local draft with a coincidentally similar spelling.
            if draft_id.starts_with("remote-draft-") {
                return Err(Error::SessionNotFound);
            }
        }
    }
    Err(Error::SessionNotFound)
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
    let _session = state
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

    let _session_command_guard = state
        .env_bridge
        .session_command_lock(&session_id)
        .await
        .lock_owned()
        .await;
    if let Some(key) = idempotency_key.as_deref() {
        if let Some(existing) = state
            .store
            .find_turn_by_idempotency_key(&session_id, key)
            .await?
        {
            return Ok((StatusCode::OK, Json(TurnResponse { turn: existing })));
        }
    }
    let attachments = req.attachments.unwrap_or_default();
    if !attachments.is_empty() {
        let total: usize = attachments.iter().map(|a| a.data.len()).sum();
        return Err(Error::AttachmentInvalid(format!(
            "attachments total {total} bytes must be uploaded through the envagent capability"
        )));
    }
    let text = req.text.unwrap_or_default();
    let now = now_ms();
    let turn = Turn {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        status: "queued".into(),
        origin: "user".into(),
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
    let created = match state.store.create_turn_claiming_slot(&turn).await? {
        Some(created) => created,
        None => {
            // A concurrent request with the same idempotency key may have
            // won the database CAS just before this one. Return that
            // canonical turn rather than reporting a misleading active
            // conflict; a different request still receives the conflict.
            if let Some(key) = idempotency_key.as_deref() {
                if let Some(existing) = state
                    .store
                    .find_turn_by_idempotency_key(&session_id, key)
                    .await?
                {
                    return Ok((StatusCode::OK, Json(TurnResponse { turn: existing })));
                }
            }
            return Err(Error::SessionAlreadyRunning);
        }
    };
    if !text.is_empty() {
        state
            .env_bridge
            .emit_server_event(&session_id, &created.id, "user", text.clone(), None)
            .await
            .map_err(|error| Error::Internal(format!("could not journal user event: {error:#}")))?;
    }
    state
        .env_bridge
        .emit_server_event(
            &session_id,
            &created.id,
            "status",
            String::new(),
            Some(serde_json::json!({"state": "starting"})),
        )
        .await
        .map_err(|error| Error::Internal(format!("could not journal status event: {error:#}")))?;
    let _ = state
        .store
        .set_session_title_if_empty(&session_id, &text)
        .await;
    let request_id = sylph_protocol::CommandRequestId::parse(
        idempotency_key
            .as_deref()
            .map(|key| format!("send-{}", blake3::hash(key.as_bytes()).to_hex()))
            .unwrap_or_else(|| format!("send-{}", uuid::Uuid::new_v4().simple())),
    )
    .map_err(|error| Error::InvalidRequest(error.to_string()))?;
    let outcome = match state
        .env_bridge
        .dispatch_and_wait(
            &session_id.environment,
            sylph_protocol::Command::new(
                request_id,
                sylph_protocol::IdempotencyPolicy::AtMostOnce,
                sylph_protocol::EnvironmentCommand::Send {
                    session_id: session_id.clone(),
                    turn_id: created.id.clone(),
                    input: sylph_protocol::WireTurnInput {
                        text,
                        attachments: Vec::new(),
                        command: req.command,
                        args: req.args.unwrap_or_default().into_iter().collect(),
                        config: Default::default(),
                    },
                },
            ),
            Duration::from_secs(30),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = state
                .store
                .finish_turn_at(
                    &created.id,
                    "failed",
                    Some("LINK_LOST"),
                    Some(&format!("{error:#}")),
                    now_ms(),
                )
                .await;
            let _ = state.store.update_session_status(&session_id, "idle").await;
            return Err(Error::LinkFailed(format!("{error:#}")));
        }
    };
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => {
            let _ = state.store.mark_turn_running(&created.id).await?;
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            let _ = state
                .store
                .finish_turn_at(&created.id, "failed", Some(&code), Some(&message), now_ms())
                .await;
            let _ = state.store.update_session_status(&session_id, "idle").await;
            return Err(Error::LinkFailed(format!("{code}: {message}")));
        }
        sylph_protocol::CommandOutcome::Unknown { message } => {
            let _ = state
                .store
                .finish_turn_at(
                    &created.id,
                    "failed",
                    Some("LINK_LOST"),
                    message.as_deref(),
                    now_ms(),
                )
                .await;
            let _ = state.store.update_session_status(&session_id, "idle").await;
            return Err(Error::LinkFailed(format!(
                "remote turn outcome is unknown: {}",
                message.unwrap_or_default()
            )));
        }
    }
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

    // One attach() call: it snapshots the ephemeral projection and subscribes
    // under a single lock. Calling it twice (once for `live`, once for `replay`)
    // reintroduces the gap it exists to close — an event published between the
    // two calls is both replayed and broadcast, so the client sees it twice.
    let Attachment {
        replay: projection,
        live,
        ..
    } = state.hub.attach(&turn_id, after_seq).await;

    // Read durable events *after* attaching. If the turn settles between a storage
    // read and `attach`, the terminal status is already in the journal while the
    // finished channel has no live receiver; reading first would therefore omit the
    // status frame and leave a browser waiting on a turn that is already complete.
    // Attaching first closes that interval: events published after the snapshot arrive
    // through `live`, while everything committed before the read is replayed below.
    // A finished turn that has already been consolidated is gone from the journal, so
    // the history tier remains the fallback for that case.
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
    // A stable event can be published after the attach snapshot but before the
    // durable read. It then appears in both `journal_events` and the live receiver;
    // suppress only those exact sequence numbers. A high-water filter is wrong here:
    // ephemeral deltas intentionally leave gaps before a later stable event.
    let durable_seqs = journal_events
        .iter()
        .map(|event| event.seq)
        .collect::<std::collections::HashSet<_>>();

    let stream = if let Some(attachment) = live {
        // BroadcastStream wraps errors; unwrap them because this adapter is
        // infallible — a lagged receiver is disconnected, not surfaced.
        let live = BroadcastStream::new(attachment)
            .filter_map(move |r| r.ok().filter(|event| !durable_seqs.contains(&event.seq)));

        boxed_local(
            stream::iter(journal_events)
                .chain(stream::iter(projection))
                .chain(live)
                .map(sse_event),
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

    // Remote turns have no in-process waiter, so `LinkedSupervisor::stop`
    // cannot see or settle them.  Claim the interrupted outcome through the
    // shared closer first (the same ordering as local stop), then forward a
    // turn-guarded interrupt to the envagent.  The guard prevents a delayed
    // command from stopping a newer turn on the same long-lived session.

    let _session_command_guard = state
        .env_bridge
        .session_command_lock(&turn.session_id)
        .await
        .lock_owned()
        .await;
    let settled = state
        .env_bridge
        .interrupt_turn(&turn.session_id, &turn.id)
        .await;
    if !settled {
        return Err(Error::StopAlreadyCompleted);
    }
    let outcome = state
        .env_bridge
        .remote_session_command(
            &turn.session_id,
            sylph_protocol::EnvironmentCommand::Interrupt {
                session_id: turn.session_id.clone(),
                expected_turn_id: Some(turn.id.clone()),
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await;
    if let Err(error) = outcome {
        tracing::warn!(
            turn = %turn.id,
            "remote interrupt could not be delivered after cancellation: {error:#}"
        );
    }
    let updated = state
        .store
        .get_turn(&turn.id)
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
