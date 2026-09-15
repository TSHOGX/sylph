//! The user control ledger and server-owned pending follow-ups.

use crate::{
    error::{Error, Result},
    middleware::auth::SubjectId,
    models::{now_ms, ControlRequest},
    AppState,
};
use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde::Deserialize;
use sylph_protocol::{
    Command, CommandOutcome, CommandRequestId, EnvironmentCommand, IdempotencyPolicy,
};

pub async fn publish(state: &AppState, request: &ControlRequest) {
    let now = now_ms();
    state.hub.publish_session(&request.session_id.encode(), crate::events::StreamEvent {
        seq: 0, arrival_turn_id: None, kind: "control_changed".into(), phase: None,
        stream_id: Some(request.request_id.clone()), span: None, interior_state: None, name: None,
        text: String::new(), data: Some(serde_json::json!({"requestId": request.request_id, "revision": request.revision})),
        is_error: false, stability: "stable".into(), source: "server".into(), arrival_at_ms: now,
        recorded_at_ms: None, created_at_ms: now, started_at_ms: None, body_ref: None,
            tool_summary: None,
    }).await;
}

pub async fn transition(
    state: &AppState,
    request: &ControlRequest,
    status: &str,
    receipt: Option<&str>,
    error: Option<&str>,
) -> Result<ControlRequest> {
    let updated = state
        .store
        .update_control_request(&request.request_id, status, receipt, error)
        .await?
        .ok_or(Error::RequestNotFound)?;
    publish(state, &updated).await;
    Ok(updated)
}

/// The durable request identity also names the at-most-once envagent command.
pub async fn dispatch(
    state: &AppState,
    request: &ControlRequest,
    command: EnvironmentCommand,
) -> Result<ControlRequest> {
    transition(state, request, "dispatched", None, None).await?;
    let id = CommandRequestId::parse(format!("control-{}", request.request_id))
        .map_err(|error| Error::Internal(error.to_string()))?;
    let outcome = state
        .env_bridge
        .dispatch_and_wait(
            &request.session_id.environment,
            Command::new(id, IdempotencyPolicy::AtMostOnce, command),
            std::time::Duration::from_secs(30),
        )
        .await;
    match outcome {
        Ok(outcome @ CommandOutcome::Accepted { .. }) => {
            if matches!(request.kind.as_str(), "steer" | "queue") {
                if let Err(error) = confirm_turn_receipt(
                    &outcome,
                    request.target_turn_id.as_deref().unwrap_or_default(),
                ) {
                    transition(
                        state,
                        request,
                        "unconfirmed",
                        None,
                        Some(&error.to_string()),
                    )
                    .await?;
                    return Err(error);
                }
            }
            let receipt =
                serde_json::to_string(&outcome).map_err(|e| Error::Internal(e.to_string()))?;
            let confirmed = transition(state, request, "confirmed", Some(&receipt), None).await?;
            if matches!(request.kind.as_str(), "steer" | "queue") && !request.text.is_empty() {
                let target = request
                    .target_turn_id
                    .as_deref()
                    .ok_or_else(|| Error::Internal("Steer receipt has no target".into()))?;
                journal_message(state, request, target).await?;
            }
            Ok(confirmed)
        }
        Ok(CommandOutcome::Rejected { code, message }) => {
            transition(
                state,
                request,
                "rejected",
                None,
                Some(&format!("{code}: {message}")),
            )
            .await?;
            match code.as_str() {
                "TURN_MOVED" => Err(Error::TurnMoved {
                    expected: request.target_turn_id.clone().unwrap_or_default(),
                    running: None,
                }),
                "UNSUPPORTED_FIELD" => Err(Error::Unsupported(message)),
                "SESSION_NOT_HOSTED" => Err(Error::LinkFailed(message)),
                _ => Err(Error::InvalidRequest(format!("{code}: {message}"))),
            }
        }
        other => {
            let message = match other {
                Ok(CommandOutcome::Unknown { message }) => {
                    format!("Delivery is unconfirmed: {}", message.unwrap_or_default())
                }
                Err(error) => format!("Delivery is unconfirmed: {error:#}"),
                _ => unreachable!(),
            };
            transition(state, request, "unconfirmed", None, Some(&message)).await?;
            Err(Error::LinkFailed(message))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmendRequest {
    pub revision: i64,
    pub text: Option<String>,
    pub turn_id: Option<String>,
}

async fn authorized(state: &AppState, subject: &str, id: &str) -> Result<ControlRequest> {
    let request = state
        .store
        .control_request(id)
        .await?
        .ok_or(Error::RequestNotFound)?;
    if !state
        .store
        .has_capability(subject, request.session_id.environment.as_str(), "control")
        .await?
    {
        return Err(Error::RequestNotFound);
    }
    Ok(request)
}

pub async fn edit(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(body): Json<AmendRequest>,
) -> Result<Json<serde_json::Value>> {
    let request = authorized(&state, &subject.0, &id).await?;
    let _guard = state
        .env_bridge
        .session_command_lock(&request.session_id)
        .await
        .lock_owned()
        .await;
    let text = body
        .text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| Error::InvalidRequest("Queued text cannot be empty".into()))?;
    let updated = state
        .store
        .amend_control_request(&id, body.revision, Some(text), "queued")
        .await?
        .ok_or_else(|| {
            Error::InvalidRequest(
                "Queue item changed or was already dispatched; refresh before editing".into(),
            )
        })?;
    publish(&state, &updated).await;
    Ok(Json(serde_json::json!({"request": updated})))
}

pub async fn cancel(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(body): Json<AmendRequest>,
) -> Result<Json<serde_json::Value>> {
    let request = authorized(&state, &subject.0, &id).await?;
    let _guard = state
        .env_bridge
        .session_command_lock(&request.session_id)
        .await
        .lock_owned()
        .await;
    let updated = state
        .store
        .amend_control_request(&id, body.revision, None, "cancelled")
        .await?
        .ok_or_else(|| {
            Error::InvalidRequest(
                "Queue item changed or was already dispatched; refresh before cancelling".into(),
            )
        })?;
    publish(&state, &updated).await;
    Ok(Json(serde_json::json!({"request": updated})))
}

pub async fn steer(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(body): Json<AmendRequest>,
) -> Result<Json<serde_json::Value>> {
    let request = authorized(&state, &subject.0, &id).await?;
    let _guard = state
        .env_bridge
        .session_command_lock(&request.session_id)
        .await
        .lock_owned()
        .await;
    let running = state.store.running_turn_id(&request.session_id).await?;
    if running.is_none()
        || body
            .turn_id
            .as_ref()
            .is_some_and(|expected| Some(expected) != running.as_ref())
    {
        return Err(Error::TurnMoved {
            expected: body.turn_id.unwrap_or_default(),
            running,
        });
    }
    let mut claimed = state
        .store
        .amend_control_request(&id, body.revision, None, "accepted")
        .await?
        .ok_or_else(|| {
            Error::InvalidRequest("Queue item changed or was already dispatched".into())
        })?;
    state
        .store
        .set_control_target(&id, running.as_deref().unwrap())
        .await?;
    claimed.target_turn_id = running.clone();
    let updated = dispatch(
        &state,
        &claimed,
        EnvironmentCommand::Steer {
            session_id: request.session_id,
            text: request.text,
            expected_turn_id: running,
        },
    )
    .await?;
    Ok(Json(serde_json::json!({"request": updated})))
}

pub async fn journal_message(
    state: &AppState,
    request: &ControlRequest,
    turn_id: &str,
) -> Result<()> {
    state.env_bridge.emit_server_event(&request.session_id, turn_id, "user", request.text.clone(),
        Some(serde_json::json!({"controlRequestId": request.request_id, "originSubject": request.origin_subject})))
        .await.map_err(|error| Error::Internal(format!("Could not journal user intent: {error:#}")))
}

pub fn spawn_dispatcher(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            interval.tick().await;
            let pending = match state.store.queued_controls().await {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::error!(%error, "Could not read control queue");
                    continue;
                }
            };
            for request in pending {
                if !state
                    .env_bridge
                    .environment_connected(&request.session_id.environment)
                    .await
                {
                    continue;
                }
                let result = super::turns::create(
                    State(state.clone()),
                    Extension(SubjectId(request.origin_subject.clone())),
                    Path(request.session_id.encode()),
                    axum::http::HeaderMap::new(),
                    Json(crate::models::CreateTurnRequest {
                        text: Some(request.text.clone()),
                        idempotency_key: request.idempotency_key.clone(),
                        attachments: None,
                        command: None,
                        args: None,
                    }),
                )
                .await;
                if let Err(error) = result {
                    if !matches!(error, Error::SessionAlreadyRunning) {
                        let _guard = state
                            .env_bridge
                            .session_command_lock(&request.session_id)
                            .await
                            .lock_owned()
                            .await;
                        if let Ok(Some(current)) =
                            state.store.control_request(&request.request_id).await
                        {
                            // The pass sampled this item before the session lock. An edit,
                            // cancellation, or another dispatcher may already have won.
                            if current.state == "queued" && current.revision == request.revision {
                                let _ = transition(
                                    &state,
                                    &current,
                                    "failed",
                                    None,
                                    Some(&error.to_string()),
                                )
                                .await;
                            }
                        }
                    }
                }
            }
        }
    });
}

pub fn confirm_turn_receipt(outcome: &CommandOutcome, turn_id: &str) -> Result<()> {
    let receipt = match outcome {
        CommandOutcome::Accepted {
            result: Some(value),
        } => serde_json::from_value::<sylph_protocol::ControlReceipt>(value.clone()).ok(),
        _ => None,
    };
    if matches!(receipt, Some(sylph_protocol::ControlReceipt::ActiveTurn { lease }) if lease.turn_id == turn_id)
    {
        Ok(())
    } else {
        Err(Error::LinkFailed(
            "Vendor receipt did not confirm the targeted turn lease; delivery is unconfirmed"
                .into(),
        ))
    }
}
