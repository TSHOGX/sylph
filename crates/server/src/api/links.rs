//! Endpoints for a session's live agent: its capabilities, and the questions it
//! is waiting on.
//!
//! These are the surfaces v1 could not have had. What a session can do is read
//! from a live link rather than from a static descriptor, and an approval is a
//! resource a client resolves rather than a message it happened to witness.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::{self as stream, wrappers::BroadcastStream};

use sylph_protocol::SessionId;

use crate::error::{Error, Result};
use crate::link_state::LinkState;
use crate::models::{PendingRequest, ResolveRequest};
use crate::AppState;

fn remote_link_state(agent: String, value: serde_json::Value) -> Result<LinkState> {
    let capabilities_value = value
        .get("capabilities")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let supports = value
        .get("supports")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let link: Option<sylph_protocol::LinkFacets> = match value.get("link") {
        Some(value) => Some(serde_json::from_value(value.clone()).map_err(|error| {
            Error::Internal(format!("remote link facets were malformed: {error}"))
        })?),
        None => None,
    };
    if link.as_ref().is_some_and(|link| !link.control.available) {
        return Ok(LinkState {
            link: link.unwrap(),
            agent: Some(agent),
            supports: Vec::new(),
            capabilities: None,
        });
    }
    let capabilities = serde_json::from_value(capabilities_value)
        .map_err(|error| Error::Internal(format!("remote capabilities were malformed: {error}")))?;
    Ok(LinkState::from_remote_capabilities_with_link(
        agent,
        capabilities,
        supports,
        link,
    ))
}

fn remote_refusal(code: &str, message: String) -> Error {
    match code {
        "INVALID_VALUE" => Error::InvalidRequest(message),
        "UNSUPPORTED_FIELD" => Error::Unsupported(message),
        "SESSION_NOT_HOSTED" | "NO_AGENT" => Error::LinkFailed(message),
        _ => Error::LinkFailed(format!("{code}: {message}")),
    }
}

fn remote_config_refusal(code: &str, message: String) -> Error {
    match code {
        "INVALID_VALUE" | "UNSUPPORTED_FIELD" => Error::InvalidRequest(message),
        _ => remote_refusal(code, message),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedConfigRequest {
    pub id: String,
    pub value: serde_json::Value,
}

#[derive(Deserialize)]
pub struct TraceEventsQuery {
    #[serde(default = "default_trace_after_seq")]
    pub after_seq: i32,
}

fn default_trace_after_seq() -> i32 {
    -1
}

/// POST /api/v1/sessions/draft/{id}/config
pub async fn prepared_set_config(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
    Json(body): Json<PreparedConfigRequest>,
) -> Result<Json<LinkState>> {
    if let Some(draft) = state.env_bridge.remote_draft(&draft_id).await {
        state
            .env_bridge
            .remote_draft_set_config(&draft, body.id, body.value)
            .await
            .map_err(|error| {
                let text = format!("{error:#}");
                if let Some((code, message)) = text.split_once(": ") {
                    remote_config_refusal(code, message.to_string())
                } else {
                    Error::LinkFailed(text)
                }
            })?;
        let refreshed = state
            .env_bridge
            .remote_draft_capabilities(&draft)
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        return Ok(Json(remote_link_state(draft.agent, refreshed)?));
    }
    // Draft handles are issued by an envagent. A handle unknown to the bridge
    // is gone; it must never fall through to an in-process LinkManager.
    Err(Error::SessionNotFound)
}

/// DELETE /api/v1/sessions/draft/{id}
pub async fn discard_prepared(
    State(state): State<AppState>,
    Path(draft_id): Path<String>,
) -> Result<StatusCode> {
    state.env_bridge.discard_remote_draft(&draft_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/sessions/draft/{id}/capabilities
pub async fn prepared_capabilities(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<LinkState>> {
    if state.env_bridge.remote_draft_promoting(&id).await {
        return Err(Error::SessionAlreadyRunning);
    }
    if let Some(draft) = state.env_bridge.remote_draft(&id).await {
        let value = state
            .env_bridge
            .remote_draft_capabilities(&draft)
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        return Ok(Json(remote_link_state(draft.agent, value)?));
    }
    Err(Error::SessionNotFound)
}

/// What this session's agent can do right now.
///
/// Establishes a link if there is none, because the answer is only knowable from
/// a live agent — the project's command list depends on files the CLI reads at
/// startup. A caller that merely wants to know whether a link exists reads the
/// session's `link` field instead of paying for one here.
pub async fn capabilities(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    let session_id = require_session(&state, &session_id).await?;

    if !state
        .env_bridge
        .environment_connected(&session_id.environment)
        .await
    {
        return Ok(Json(LinkState::failed(
            sylph_protocol::UnavailableReason::EnvironmentUnreachable,
        )));
    }
    let mut outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Capabilities {
                session_id: session_id.clone(),
            },
            sylph_protocol::IdempotencyPolicy::Replayable,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    if matches!(&outcome, sylph_protocol::CommandOutcome::Rejected { code, .. } if code == "SESSION_NOT_HOSTED")
    {
        let attach_outcome = match state
            .env_bridge
            .remote_attach(
                &state
                    .store
                    .get_session(&session_id)
                    .await?
                    .ok_or(Error::SessionNotFound)?,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let text = format!("{error:#}");
                if text.contains("not installed")
                    || text.contains("PREPARE_FAILED")
                    || text.contains("NO_AGENT")
                {
                    return Ok(Json(LinkState::failed(
                        sylph_protocol::UnavailableReason::NoAgent,
                    )));
                }
                return Err(Error::LinkFailed(text));
            }
        };
        if let sylph_protocol::CommandOutcome::Rejected { code, message } = attach_outcome {
            if code == "NO_AGENT" {
                return Ok(Json(LinkState::failed(
                    sylph_protocol::UnavailableReason::NoAgent,
                )));
            }
            return Err(Error::LinkFailed(format!("{code}: {message}")));
        }
        outcome = state
            .env_bridge
            .remote_session_command(
                &session_id,
                sylph_protocol::EnvironmentCommand::Capabilities {
                    session_id: session_id.clone(),
                },
                sylph_protocol::IdempotencyPolicy::Replayable,
            )
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    }
    match outcome {
        sylph_protocol::CommandOutcome::Accepted {
            result: Some(value),
        } => Ok(Json(remote_link_state(
            session_id.agent.to_string(),
            value,
        )?)),
        sylph_protocol::CommandOutcome::Accepted { result: None } => {
            Ok(Json(LinkState::detached_remote()))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(remote_config_refusal(&code, message))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote capabilities outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

/// Establish a link eagerly.
///
/// Worth having as its own operation because establishment is slow — an agent CLI
/// loads a project — and a UI that knows a user is about to type can pay that
/// cost before they finish rather than after they send.
pub async fn attach(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    let session_id = require_session(&state, &session_id).await?;

    let outcome = state
        .env_bridge
        .remote_attach(
            &state
                .store
                .get_session(&session_id)
                .await?
                .ok_or(Error::SessionNotFound)?,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => {
            for request in state.store.abandon_open_requests(&session_id).await? {
                state
                    .hub
                    .publish_session(
                        &session_id.encode(),
                        crate::events::StreamEvent {
                            seq: 0,
                            arrival_turn_id: None,
                            kind: "request".into(),
                            phase: None,
                            stream_id: None,
                            span: None,
                            interior_state: None,
                            name: None,
                            text: String::new(),
                            data: Some(serde_json::to_value(request).unwrap_or_default()),
                            is_error: false,
                            stability: "stable".into(),
                            source: "server".into(),
                            arrival_at_ms: crate::models::now_ms(),
                            recorded_at_ms: None,
                            created_at_ms: crate::models::now_ms(),
                            started_at_ms: None,
                        },
                    )
                    .await;
            }
            Ok(Json(LinkState::detached_remote()))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(remote_config_refusal(&code, message))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote attach outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

/// Tear a link down without ending the session.
pub async fn detach(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<LinkState>> {
    let session_id = require_session(&state, &session_id).await?;
    let abandoned = state.store.abandon_open_requests(&session_id).await?;
    for request in abandoned {
        state
            .hub
            .publish_session(
                &session_id.encode(),
                crate::events::StreamEvent {
                    seq: 0,
                    arrival_turn_id: None,
                    kind: "request".into(),
                    phase: None,
                    stream_id: None,
                    span: None,
                    interior_state: None,
                    name: None,
                    text: String::new(),
                    data: Some(serde_json::to_value(request).unwrap_or_default()),
                    is_error: false,
                    stability: "stable".into(),
                    source: "server".into(),
                    arrival_at_ms: crate::models::now_ms(),
                    recorded_at_ms: None,
                    created_at_ms: crate::models::now_ms(),
                    started_at_ms: None,
                },
            )
            .await;
    }
    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Close {
                session_id: Some(session_id.clone()),
            },
            sylph_protocol::IdempotencyPolicy::Replayable,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => Ok(Json(LinkState::detached_remote())),
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(Error::LinkFailed(format!("{code}: {message}")))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote detach outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

/// What this session is waiting on.
///
/// Read from the journal rather than from the link, so it answers after a client
/// reconnects, from a different frontend, and for a session whose link has since
/// died — the last of which is exactly when a user needs to know why nothing is
/// happening.
pub async fn list_requests(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<PendingRequest>>> {
    let session_id = require_session(&state, &session_id).await?;
    Ok(Json(state.store.open_requests(&session_id).await?))
}

/// Answer a request an agent raised.
pub async fn resolve(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
    Json(body): Json<ResolveRequest>,
) -> Result<Json<PendingRequest>> {
    let request = state
        .store
        .get_request(&request_id)
        .await?
        .ok_or(Error::RequestNotFound)?;

    // The option has to be one the agent offered. A client inventing one would
    // reach the adapter as an unrecognized string, and an adapter's handling of
    // that is undefined — for an approval, "unrecognized" must never be able to
    // mean "allow".
    let offered = request.options();
    let known = offered
        .as_array()
        .map(|options| {
            options.iter().any(|option| {
                option.get("id").and_then(|id| id.as_str()) == Some(body.option.as_str())
            })
        })
        .unwrap_or(false);
    if !known {
        return Err(Error::InvalidRequest(format!(
            "'{}' is not an option this request offered",
            body.option
        )));
    }

    let input_json = body.input.as_ref().map(|value| value.to_string());

    // And when the request declared a form, the answer has to fit it. Checked here,
    // before the claim below, for a reason the ordering makes load-bearing: a
    // malformed answer that reached the UPDATE would consume the single-winner claim
    // and mark the request resolved, leaving the agent blocked on a question nobody
    // can answer any more. Refusing first leaves it open and answerable.
    //
    // This is the same rule that governs `option` above and a `set_config` value: the
    // client renders whatever an agent it has never seen reported, so the server is
    // what keeps a malformed answer from reaching a vendor CLI whose handling of one
    // is undefined.
    if let Some(form) = request.form() {
        // Only when the answer is being submitted. Declining carries no values, and
        // requiring a complete form in order to say "don't answer" would make a
        // required field into a trap the user cannot leave.
        let declining = body.option.starts_with("deny") || body.option == "cancel";
        if !declining {
            form.validate(body.input.as_ref())
                .map_err(Error::InvalidRequest)?;
        }
    }

    // Claimed in one statement, so two clients answering together produce one
    // winner rather than two forwarded answers.
    let resolved = state
        .store
        .resolve_request(&request_id, &body.option, input_json.as_deref(), None)
        .await?
        .ok_or(Error::RequestAlreadyResolved)?;

    state
        .hub
        .publish_session(
            &request.session_id.encode(),
            crate::events::StreamEvent {
                seq: 0,
                arrival_turn_id: None,
                kind: "request".into(),
                phase: None,
                stream_id: None,
                span: None,
                interior_state: None,
                name: None,
                text: String::new(),
                data: Some(serde_json::to_value(&resolved).unwrap_or_default()),
                is_error: false,
                stability: "stable".into(),
                source: "server".into(),
                arrival_at_ms: crate::models::now_ms(),
                recorded_at_ms: None,
                created_at_ms: crate::models::now_ms(),
                started_at_ms: None,
            },
        )
        .await;

    // Announced before the forward, and deliberately so. The claim above is what makes
    // this request answered — the row is committed and no second answer can win — so
    // every other client may stop showing it from this moment, whether or not the
    // adapter is still there to hear the answer. Announcing after the forward would
    // tie a *display* concern to a *delivery* one, and the `LinkGone` path below
    // returns early: a request answered on a phone against a dead link would then stay
    // on screen on every other client, which is the bug this fixes, surviving in the
    // one case where the user most needs to be told what happened.
    //
    // This is the counterpart of `absorb_request`'s announcement. Arrival was published
    // and departure was not, so a client's view of what a session is blocked on could
    // only ever grow; it shrank on a 20-second poll that a backgrounded tab does not
    // run, which is why answering on one device left the dialog standing on another
    // until a manual reload.
    // Forward the typed resolution to the envagent that owns the vendor process,
    // while the durable claim above remains the server's single-winner authority.
    let outcome = state
        .env_bridge
        .remote_session_command(
            &request.session_id,
            sylph_protocol::EnvironmentCommand::Resolve {
                session_id: request.session_id.clone(),
                resolution: serde_json::json!({
                    "requestId": request.adapter_request_id.clone(),
                    "option": body.option.clone(),
                    "input": body.input.clone(),
                }),
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => Ok(Json(resolved)),
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(Error::LinkFailed(format!("{code}: {message}")))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote resolution outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

/// Interrupt the running turn, keeping the session alive.
///
/// Deliberately not `stop`. Stop signals the process group and ends the agent;
/// interrupt asks the agent to abandon this turn and stay loaded, so the next
/// message continues the same conversation. They are different intentions — "that
/// is not what I meant" against "stop everything" — and offering only one means
/// the other is unavailable.
pub async fn interrupt(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let session_id = require_session(&state, &session_id).await?;

    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Interrupt {
                session_id: session_id.clone(),
                expected_turn_id: None,
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => {
            Ok(Json(serde_json::json!({"interrupted": true})))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => Err(refusal_or(
            anyhow::anyhow!("{code}: {message}"),
            Error::Unsupported,
        )),
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote interrupt outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

/// Add text to a turn already in flight.
///
/// `turnId` is a precondition rather than a routing hint. A steer that races a turn
/// boundary would otherwise be delivered into the *next* turn — the request
/// succeeds, nothing errors, and the text lands in the wrong conversation. Naming
/// the turn the client believed was running turns that into a refusal.
pub async fn steer(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<SteerRequest>,
) -> Result<Json<serde_json::Value>> {
    let session_id = require_session(&state, &session_id).await?;

    if body.text.trim().is_empty() {
        return Err(Error::InvalidRequest(
            "Steering text cannot be empty".into(),
        ));
    }

    // Checked here as well as by the adapter: the server knows which turn is
    // running, so a stale client can be told so without waking the agent.
    let running = state.store.running_turn_id(&session_id).await?;
    if let Some(expected) = body.turn_id.as_deref() {
        if running.as_deref() != Some(expected) {
            return Err(Error::TurnMoved {
                expected: expected.to_string(),
                running: running.clone(),
            });
        }
    }

    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Steer {
                session_id: session_id.clone(),
                text: body.text,
                expected_turn_id: body.turn_id,
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => {
            Ok(Json(serde_json::json!({"steered": true})))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(Error::LinkFailed(format!("{code}: {message}")))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote steer outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerRequest {
    pub text: String,
    /// The turn the client believes is running. Optional, because a client with no
    /// turn in view is steering "whatever is running", which is a coherent ask.
    #[serde(default)]
    pub turn_id: Option<String>,
}

/// Add text to run **after** the turn in flight.
///
/// # Why this is not `steer` with a flag
///
/// The two differ by *when the agent reads the text*, and that decides what the request
/// means rather than how it is delivered. A steer is a correction, so it carries a turn
/// precondition: text that misses its turn lands in the wrong conversation, and naming the
/// turn is what turns that into a refusal. A queued follow-up has **no such precondition** —
/// it is *for* after the turn, so a turn boundary crossing underneath it changes nothing, and
/// queueing with nothing running is a coherent ask that becomes the next turn's opening text.
///
/// Collapsing them behind one endpoint would have meant a `turnId` that is required for one
/// meaning and meaningless for the other, which is the shape that let one vendor declare the
/// wrong word for what it does: `claude` claimed `Capability::Steer` while its only mid-turn
/// channel is read after the turn ends, so a user's correction arrived after the thing it was
/// meant to prevent with every layer reporting success.
///
/// The refusal path matters as much as the success one. An agent without
/// [`sylph_protocol::Capability::Queue`] answers `UNSUPPORTED_FIELD`, which a client renders
/// by retiring the control rather than leaving it clickable — and that is the code `claude`'s
/// `steer` now returns, so a client asking the wrong one of these is told which.
pub async fn queue(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<QueueRequest>,
) -> Result<Json<serde_json::Value>> {
    let session_id = require_session(&state, &session_id).await?;

    if body.text.trim().is_empty() {
        return Err(Error::InvalidRequest("Queued text cannot be empty".into()));
    }

    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::Queue {
                session_id: session_id.clone(),
                text: body.text,
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted { .. } => {
            Ok(Json(serde_json::json!({"queued": true})))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(Error::LinkFailed(format!("{code}: {message}")))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote queue outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRequest {
    pub text: String,
}

/// Set one typed config option on a live session.
///
/// The response is the refreshed capability surface rather than an acknowledgement,
/// because setting one option can change another's choices — `pi`'s thinking levels
/// depend on the selected model, and `claude` carries per-model effort levels. A
/// client that assumed its own optimistic value would drift from the agent.
pub async fn set_config(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(body): Json<SetConfigRequest>,
) -> Result<Json<LinkState>> {
    let session_id = require_session(&state, &session_id).await?;

    if body.config_id.trim().is_empty() {
        return Err(Error::InvalidRequest("A config id is required".into()));
    }

    let outcome = state
        .env_bridge
        .remote_session_command(
            &session_id,
            sylph_protocol::EnvironmentCommand::SetConfig {
                session_id: session_id.clone(),
                id: body.config_id,
                value: body.value,
            },
            sylph_protocol::IdempotencyPolicy::AtMostOnce,
        )
        .await
        .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
    match outcome {
        sylph_protocol::CommandOutcome::Accepted {
            result: Some(value),
        } => {
            if let Some(flag) = value.get("persistedFlag") {
                if let (Some(key), Some(value)) = (
                    flag.get("key").and_then(|v| v.as_str()),
                    flag.get("value").and_then(|v| v.as_str()),
                ) {
                    state
                        .store
                        .set_session_flag(&session_id, key, value)
                        .await?;
                }
            }
            Ok(Json(remote_link_state(
                session_id.agent.to_string(),
                value,
            )?))
        }
        sylph_protocol::CommandOutcome::Accepted { result: None } => {
            Ok(Json(LinkState::detached_remote()))
        }
        sylph_protocol::CommandOutcome::Rejected { code, message } => {
            Err(remote_config_refusal(&code, message))
        }
        sylph_protocol::CommandOutcome::Unknown { message } => Err(Error::LinkFailed(format!(
            "remote config outcome is unknown: {}",
            message.unwrap_or_default()
        ))),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConfigRequest {
    pub config_id: String,
    pub value: serde_json::Value,
}

/// Classify an adapter's refusal, falling back to `otherwise` for anything else.
///
/// # The two codes are not interchangeable, and one status erases the difference
///
/// `UNSUPPORTED_FIELD` means "no such option" and justifies **disabling** a control;
/// `INVALID_VALUE` means "this value, right now" and must leave it usable, since the other
/// five values may well be fine. Both adapters already drew that line and this module used
/// to flatten every adapter error into [`Error::Unsupported`] — whose own doc comment says
/// a client may disable the control over it — so one refused permission mode would have
/// retired a picker most of whose values work.
///
/// # Recovered by `downcast_ref`, never by parsing the message
///
/// The text is the adapter's and may be reworded; the code is declared protocol. That
/// distinction is why the error travels as a typed [`sylph_protocol::AdapterError`] through
/// `anyhow` rather than as a `"{code}: {message}"` string — and with adapters linked in it
/// is constructed as that type at the source, so the transitional `refusal()` that mapped
/// a v3 wire string back into it is deleted rather than moved.
///
/// One helper rather than three copies of the match, because `interrupt`, `steer`, and
/// `set_config` are all refusable and a rule written three times is one that will diverge.
fn refusal_or(error: anyhow::Error, otherwise: fn(String) -> Error) -> Error {
    match error.downcast_ref::<sylph_protocol::AdapterError>() {
        Some(refusal) if refusal.code == sylph_protocol::RefusalCode::InvalidValue => {
            Error::InvalidRequest(refusal.message.clone())
        }
        Some(refusal) => otherwise(refusal.message.clone()),
        None => otherwise(format!("{error:#}")),
    }
}

/// Session-scoped facts, as SSE.
///
/// Separate from the turn stream because links, capability changes, and requests
/// are properties of a session: they happen with no turn running, and a client
/// watching only turns would miss an approval raised while it was idle.
///
/// Carries no replay. These are notifications that state changed, and the state
/// itself is a resource — `/requests` and `/capabilities` — so a client that
/// reconnects re-reads rather than replaying. That is what keeps a missed frame
/// from stranding an approval.
pub async fn events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<impl axum::response::IntoResponse> {
    let session_id = require_session(&state, &session_id).await?;

    let live = state.hub.attach_session(&session_id.encode()).await;
    let stream = tokio_stream::wrappers::BroadcastStream::new(live)
        .filter_map(|result| result.ok())
        .map(|event| {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok::<axum::response::sse::Event, std::convert::Infallible>(
                axum::response::sse::Event::default()
                    .event(&event.kind)
                    .data(data),
            )
        });

    Ok((
        // `no-transform` for the reason recorded in AGENTS.md: a proxy that gzips
        // an SSE body buffers it invisibly, and the symptom reads as a rendering
        // bug rather than a transport one.
        [
            ("Cache-Control", "no-cache, no-store, no-transform"),
            ("X-Accel-Buffering", "no"),
        ],
        axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()),
    ))
}

/// GET /api/v1/sessions/{id}/trace/events — one reconnectable stream for the whole session.
///
/// Unlike the older `/sessions/{id}/events` notification stream, this carries normalized
/// turn events as well. That distinction matters when a child trajectory reports after its
/// parent turn has settled: a turn-scoped subscriber is gone, while a session trace can
/// route the event back to the existing trajectory without inventing an `agent` turn.
pub async fn trace_events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<TraceEventsQuery>,
) -> Result<impl IntoResponse> {
    let session = require_session(&state, &session_id).await?;
    let encoded = session.encode();
    let after_seq = query.after_seq;

    // Subscribe before reading either durable tier. The live receiver buffers anything
    // emitted while the history snapshot is assembled, closing the replay/live gap.
    let attachment = state.hub.attach_trace(&encoded, after_seq).await;

    let mut by_seq = std::collections::BTreeMap::<i32, crate::events::StreamEvent>::new();
    for block in state
        .history
        .blocks(&encoded, after_seq)
        .await
        .unwrap_or_default()
    {
        by_seq
            .entry(block.seq)
            .or_insert_with(|| crate::api::sessions::block_to_event(&block));
    }
    for row in state
        .store
        .session_events_after(&session, after_seq)
        .await?
    {
        by_seq
            .entry(row.seq)
            .or_insert_with(|| crate::events::StreamEvent::from_row(&row));
    }
    for event in attachment.projection {
        by_seq.entry(event.seq).or_insert(event);
    }

    // Keep the sequence ids that are about to be replayed. The receiver was
    // subscribed before reading storage, so a stable event can legitimately be
    // present in both the snapshot and the broadcast buffer when it lands between
    // those reads. Filtering by the replay set removes that duplicate without using
    // a high-water mark: an ephemeral event may have a lower seq than a later stable
    // event and still only exist in the live projection.
    let replay_seq = by_seq
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let replay = by_seq.into_values().map(sse_trace_event);
    let live = BroadcastStream::new(attachment.live)
        .filter_map(|result| result.ok())
        .filter(move |event| event.seq > after_seq && !replay_seq.contains(&event.seq))
        .map(sse_trace_event);

    Ok((
        [
            ("Cache-Control", "no-cache, no-store, no-transform"),
            ("X-Accel-Buffering", "no"),
        ],
        Sse::new(stream::iter(replay).chain(live)).keep_alive(KeepAlive::default()),
    ))
}

fn sse_trace_event(event: crate::events::StreamEvent) -> std::result::Result<Event, Infallible> {
    let data = serde_json::to_string(&event).unwrap_or_default();
    Ok(Event::default().event(&event.kind).data(data))
}

/// 404 for an unknown session before anything expensive happens.
///
/// Without this, asking for capabilities on a mistyped id would try to establish
/// a link, fail on a missing session deeper in, and report that as an agent
/// problem rather than as a bad id.
/// Decode a path segment and confirm the session exists, answering with its id.
///
/// Returns the typed [`SessionId`] rather than `()` so every handler gets the decoded
/// value from the same call that validated it. Decoding separately would mean two
/// places that know how a path segment becomes an identity, and the second one is where
/// a hand-assembled string creeps in.
///
/// Two distinct failures, deliberately not collapsed: a malformed triple is a **400**
/// because the request is unintelligible, while a well-formed id naming nothing is a
/// **404**. Answering 404 for both would tell a client with a corrupted id that its
/// session had been deleted.
async fn require_session(state: &AppState, encoded: &str) -> Result<SessionId> {
    let session_id = crate::api::sessions::decode_id(encoded)?;
    state
        .store
        .get_session(&session_id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(session_id)
}
