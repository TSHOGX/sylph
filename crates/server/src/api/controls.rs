//! Immutable user commands, fresh authorization and envagent dispatch.
use crate::{
    db::control::{AcceptedCommand, CommandState},
    error::{Error, Result},
    middleware::auth::SubjectId,
    models::{now_ms, AttachmentData, Session},
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sylph_protocol::trace::{
    AgentRef, AuthorizationReceipt, CommandAcceptance, CommandKind, CommandTarget, DeliveryFact,
    IngestionScope, ReadCommandReceipt, ReadCommandRequest, ReadCommandState, ReadControlAction,
    ReadControlTarget, RetryPolicy, SubmittedRevision,
};
use sylph_protocol::{
    Command, CommandOutcome, CommandRequestId, EnvironmentCommand, IdempotencyPolicy, SessionId,
    WireAttachment, WireTurnInput,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputRequest {
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentData>,
    pub idempotency_key: Option<String>,
}
#[derive(Serialize)]
pub struct CommandResponse {
    pub command: AcceptedCommand,
    pub session: Option<Session>,
}
fn internal(error: impl std::fmt::Display) -> Error {
    Error::Internal(error.to_string())
}

pub async fn get(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<CommandResponse>> {
    let command = state
        .store
        .command(&id)
        .await
        .map_err(internal)?
        .ok_or(Error::RequestNotFound)?;
    ensure_visible(state.trace.evidence(), &command).await?;
    let target_scope = command.acceptance.target.ingestion_scope();
    let environment = target_scope.environment();
    if command.acceptance.authorization.subject != subject.0
        || !state
            .store
            .has_capability(&subject.0, environment.as_str(), "observe")
            .await?
    {
        return Err(Error::Forbidden(
            "command belongs to another Subject".into(),
        ));
    }
    let session = match command.agent() {
        Some(AgentRef::Session(id)) => state.store.get_session(id).await?,
        _ => None,
    };
    Ok(Json(CommandResponse { command, session }))
}
pub async fn list(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "observe").await?;
    let commands = state
        .store
        .commands_for(
            &subject.0,
            &CommandTarget::Agent {
                agent: AgentRef::Session(id),
            },
            64,
        )
        .await
        .map_err(internal)?;
    let mut visible = vec![];
    for command in commands {
        match ensure_visible(state.trace.evidence(), &command).await {
            Ok(()) => visible.push(command),
            Err(Error::RequestNotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Json(serde_json::json!({"commands":visible})))
}
pub async fn create_prepared(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(input): Json<InputRequest>,
) -> Result<(StatusCode, Json<CommandResponse>)> {
    let lock = state.env_bridge.remote_promotion_lock(&id).await;
    let _guard = lock.lock().await;
    let draft = state
        .env_bridge
        .authorize_remote_draft(&id, &subject.0, "control")
        .await
        .map_err(|e| Error::Forbidden(e.to_string()))?;
    let target = match &draft.capture_authorization.scope {
        IngestionScope::Inventory(inventory) => CommandTarget::Launch {
            inventory: inventory.clone(),
        },
        _ => return Err(Error::Internal("draft has no capture inventory".into())),
    };
    let accepted = accept_input(
        &state,
        &subject.0,
        target,
        draft.capture_authorization.epoch.clone(),
        CommandKind::Send,
        &input,
    )
    .await?;
    if accepted.state != CommandState::Accepted {
        return response(&state, accepted).await;
    }
    let draft = state
        .env_bridge
        .mark_draft_command(&id, &accepted.acceptance.command_id)
        .await
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    let command = EnvironmentCommand::Run {
        capture_authorization: draft.capture_authorization.clone(),
        session_id: None,
        draft_id: Some(draft.draft_id.clone()),
        agent: draft.agent.clone(),
        workdir: draft.workdir.clone(),
        flags: draft.flags.clone().into_iter().collect(),
        inject_identity: draft.inject_identity,
        identity_preamble: draft.identity_preamble.clone(),
        permission_mode: draft.permission_mode.clone(),
        initial_input: Some(wire_input(&accepted.acceptance.input)?),
        initial_turn_id: Some(accepted.acceptance.command_id.clone()),
    };
    let outcome = dispatch(&state, &subject.0, &accepted, command).await?;
    if let CommandOutcome::Accepted {
        result: Some(value),
    } = &outcome
    {
        let native = value
            .get("sessionId")
            .cloned()
            .ok_or_else(|| Error::LinkFailed("launch returned no native identity".into()))?;
        let session_id: SessionId = serde_json::from_value(native).map_err(internal)?;
        if session_id.environment != draft.environment || session_id.agent.as_str() != draft.agent {
            return Err(Error::LinkFailed(
                "launch returned a foreign native identity".into(),
            ));
        }
        let evidence =
            serde_json::from_value(value.get("sourceEvidence").cloned().ok_or_else(|| {
                Error::LinkFailed("launch identity has no retained source evidence".into())
            })?)
            .map_err(internal)?;
        state
            .store
            .bind_identity(
                &accepted.acceptance.command_id,
                AgentRef::Session(session_id.clone()),
                evidence,
                now_ms(),
            )
            .await
            .map_err(internal)?;
        let session = Session {
            id: session_id,
            workdir: Some(
                value
                    .get("workdir")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&draft.workdir)
                    .into(),
            ),
            title: draft.title.clone(),
            inject_identity: draft.inject_identity,
            fidelity: sylph_protocol::Fidelity::Stream,
            flags_json: serde_json::to_string(&draft.flags).map_err(internal)?,
            status: "idle".into(),
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        if state.store.get_session(&session.id).await?.is_none() {
            state.store.create_session(&session).await?;
        }
    }
    flush(&state).await?;
    response(
        &state,
        state
            .store
            .command(&accepted.acceptance.command_id)
            .await
            .map_err(internal)?
            .ok_or(Error::RequestNotFound)?,
    )
    .await
}
pub async fn create(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(input): Json<InputRequest>,
) -> Result<(StatusCode, Json<CommandResponse>)> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "control").await?;
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    let lock = state.env_bridge.session_command_lock(&id).await;
    let _guard = lock.lock().await;
    let scope = IngestionScope::Agent(AgentRef::Session(id.clone()));
    let epoch = state
        .trace
        .evidence()
        .authorize_scope(&scope, &[AgentRef::Session(id.clone())])
        .await
        .map_err(internal)?;
    let accepted = accept_input(
        &state,
        &subject.0,
        CommandTarget::Agent {
            agent: AgentRef::Session(id.clone()),
        },
        epoch,
        CommandKind::Send,
        &input,
    )
    .await?;
    if accepted.state == CommandState::Accepted {
        let attached = state
            .env_bridge
            .remote_attach(&session)
            .await
            .map_err(|e| Error::LinkFailed(e.to_string()))?;
        if !matches!(attached, CommandOutcome::Accepted { .. }) {
            return Err(Error::LinkFailed("session control could not attach".into()));
        }
        dispatch(
            &state,
            &subject.0,
            &accepted,
            EnvironmentCommand::Send {
                session_id: id,
                turn_id: accepted.acceptance.command_id.clone(),
                input: wire_input(&accepted.acceptance.input)?,
            },
        )
        .await?;
    }
    response(
        &state,
        state
            .store
            .command(&accepted.acceptance.command_id)
            .await
            .map_err(internal)?
            .ok_or(Error::RequestNotFound)?,
    )
    .await
}
async fn response(
    state: &AppState,
    command: AcceptedCommand,
) -> Result<(StatusCode, Json<CommandResponse>)> {
    let session = match command.agent() {
        Some(AgentRef::Session(id)) => state.store.get_session(id).await?,
        _ => None,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(CommandResponse { command, session }),
    ))
}
async fn accept_input(
    state: &AppState,
    subject: &str,
    target: CommandTarget,
    epoch: sylph_protocol::trace::IngestionEpoch,
    kind: CommandKind,
    input: &InputRequest,
) -> Result<AcceptedCommand> {
    validate_input(input)?;
    let submitted = submitted_revision(input)?;
    let key = input
        .idempotency_key
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let command_id = command_identity(subject, &target, &key)?;
    let accepted = state
        .store
        .accept(CommandAcceptance {
            command_id,
            target: target.clone(),
            kind,
            input: submitted,
            authorization: authorization(subject, None),
            ingestion_authorization_epoch: epoch,
            retry_policy: RetryPolicy::AtMostOnce,
            idempotency_key: Some(key),
            accepted_at_ms: now_ms(),
        })
        .await
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    flush(state).await?;
    Ok(accepted)
}
fn submitted_revision(input: &InputRequest) -> Result<SubmittedRevision> {
    let (bytes, media_type) = if input.attachments.is_empty() {
        (
            input.text.clone().unwrap_or_default().into_bytes(),
            "text/plain",
        )
    } else {
        (serde_json::to_vec(&serde_json::json!({"text":input.text.clone().unwrap_or_default(),"attachments":input.attachments})).map_err(internal)?, "application/vnd.sylph.input+json")
    };
    Ok(SubmittedRevision {
        revision: format!("input-{}", blake3::hash(&bytes).to_hex()),
        media_type: media_type.into(),
        bytes,
    })
}

fn command_identity(subject: &str, target: &CommandTarget, key: &str) -> Result<String> {
    Ok(format!(
        "command-{}",
        blake3::hash(&serde_json::to_vec(&(subject, target, key)).map_err(internal)?).to_hex()
    ))
}

async fn replay_trace_input(
    state: &AppState,
    subject: &str,
    target: &CommandTarget,
    kind: CommandKind,
    input: &InputRequest,
) -> Result<Option<ReadCommandReceipt>> {
    validate_input(input)?;
    let key = input
        .idempotency_key
        .as_deref()
        .ok_or_else(|| Error::InvalidRequest("idempotency key is required".into()))?;
    let command_id = command_identity(subject, target, key)?;
    let Some(command) = state.store.command(&command_id).await.map_err(internal)? else {
        return Ok(None);
    };
    if command.acceptance.target != *target
        || command.acceptance.kind != kind
        || command.acceptance.input != submitted_revision(input)?
    {
        return Err(Error::InvalidRequest(
            "idempotency key already names another command input".into(),
        ));
    }
    ensure_visible(state.trace.evidence(), &command).await?;
    Ok(Some(command_receipt(state, command).await?))
}

fn validate_resolution(
    pending: &sylph_protocol::Request,
    answer: &sylph_protocol::Resolution,
) -> Result<()> {
    let option = pending
        .options
        .iter()
        .find(|option| option.id == answer.option)
        .ok_or_else(|| Error::InvalidRequest("request option was not offered".into()))?;
    if option.persistent && !pending.allow_persistent {
        return Err(Error::Forbidden(
            "request disallows persistent permission".into(),
        ));
    }
    if let Some(form) = &pending.form {
        form.validate(answer.input.as_ref())
            .map_err(|error| Error::InvalidRequest(error.to_string()))?;
    }
    Ok(())
}

fn authorization(subject: &str, lease: Option<String>) -> AuthorizationReceipt {
    AuthorizationReceipt {
        subject: subject.into(),
        decision_ref: uuid::Uuid::new_v4().to_string(),
        lease_ref: lease,
        evaluated_at_ms: now_ms(),
    }
}
async fn flush(state: &AppState) -> Result<()> {
    state.exporter.run_once().await.map_err(internal)?;
    state.trace.notify_admission();
    Ok(())
}
async fn dispatch(
    state: &AppState,
    subject: &str,
    accepted: &AcceptedCommand,
    mut command: EnvironmentCommand,
) -> Result<CommandOutcome> {
    ensure_visible(state.trace.evidence(), accepted).await?;
    let target_scope = accepted.acceptance.target.ingestion_scope();
    let environment = target_scope.environment();
    if !state
        .store
        .has_capability(subject, environment.as_str(), "control")
        .await?
    {
        return Err(Error::Forbidden(
            "control capability was revoked before dispatch".into(),
        ));
    }
    let permit = if let EnvironmentCommand::Resolve {
        session_id,
        resolution,
    } = &command
    {
        let answer: sylph_protocol::Resolution =
            serde_json::from_value(resolution.clone()).map_err(internal)?;
        let pending = state
            .store
            .current_request(session_id, &answer.request_id)
            .await?
            .ok_or(Error::RequestNotFound)?;
        validate_resolution(&pending, &answer)?;
        state
            .store
            .claim_request_dispatch(
                &accepted.acceptance.command_id,
                authorization(subject, None),
                now_ms(),
                session_id,
                &pending,
            )
            .await
    } else {
        state
            .store
            .claim_dispatch(
                &accepted.acceptance.command_id,
                authorization(subject, None),
                now_ms(),
            )
            .await
    }
    .map_err(|error| Error::InvalidRequest(error.to_string()))?;
    match &mut command {
        EnvironmentCommand::Send { input, .. } => *input = wire_input(&permit.command.input)?,
        EnvironmentCommand::Run {
            initial_input: Some(input),
            ..
        } => *input = wire_input(&permit.command.input)?,
        _ => {}
    }
    flush(state).await?;
    let request_id =
        CommandRequestId::parse(accepted.acceptance.command_id.clone()).map_err(internal)?;
    let outcome = state
        .env_bridge
        .dispatch_and_wait(
            environment,
            Command::new(request_id, IdempotencyPolicy::AtMostOnce, command),
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap_or_else(|e| CommandOutcome::Unknown {
            message: Some(e.to_string()),
        });
    let fact = match &outcome {
        CommandOutcome::Accepted { .. } => DeliveryFact::ReceiverAccepted {
            receipt: serde_json::to_vec(&outcome).map_err(internal)?,
        },
        CommandOutcome::Rejected { .. } => DeliveryFact::Rejected {
            receipt: serde_json::to_vec(&outcome).map_err(internal)?,
        },
        CommandOutcome::Unknown { message } => DeliveryFact::OutcomeUnknown {
            reason: message
                .clone()
                .unwrap_or_else(|| "vendor effect is unknown".into()),
        },
    };
    state
        .store
        .record_delivery(
            &accepted.acceptance.command_id,
            "dispatch-result",
            fact,
            now_ms(),
        )
        .await
        .map_err(internal)?;
    flush(state).await?;
    Ok(outcome)
}
fn validate_input(input: &InputRequest) -> Result<()> {
    if input.text.as_deref().unwrap_or("").trim().is_empty() && input.attachments.is_empty() {
        return Err(Error::InvalidRequest(
            "a message needs text or an attachment".into(),
        ));
    }
    if input.attachments.len() > 5 {
        return Err(Error::AttachmentInvalid(
            "at most five attachments are accepted".into(),
        ));
    }
    let mut total = 0;
    for attachment in &input.attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&attachment.data)
            .map_err(|_| Error::AttachmentInvalid("invalid base64 attachment".into()))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::AttachmentInvalid("attachment exceeds 8 MiB".into()));
        }
        total += bytes.len();
    }
    if total > 10 * 1024 * 1024 {
        return Err(Error::AttachmentInvalid("attachments exceed 10 MiB".into()));
    }
    Ok(())
}
fn wire_input(input: &SubmittedRevision) -> Result<WireTurnInput> {
    let (text, attachments) = if input.media_type == "text/plain" {
        (
            String::from_utf8(input.bytes.clone()).map_err(internal)?,
            vec![],
        )
    } else {
        let value: InputRequest = serde_json::from_slice(&input.bytes).map_err(internal)?;
        (value.text.unwrap_or_default(), value.attachments)
    };
    Ok(WireTurnInput {
        text,
        attachments: attachments
            .into_iter()
            .map(|a| WireAttachment {
                name: a.name,
                mime_type: a.mime_type,
                data: a.data,
            })
            .collect(),
        command: None,
        args: Default::default(),
        config: Default::default(),
    })
}

pub async fn submit_trace(
    state: &AppState,
    subject: &str,
    request: ReadCommandRequest,
) -> Result<ReadCommandReceipt> {
    let id = match &request.target {
        ReadControlTarget::Session { agent, session_id } => {
            let id = crate::api::sessions::decode_id(session_id)?;
            if agent.agent() != AgentRef::Session(id.clone()) {
                return Err(Error::InvalidRequest(
                    "control target identity mismatch".into(),
                ));
            }
            id
        }
        ReadControlTarget::Embedded { .. } => {
            return Err(Error::Unsupported(
                "embedded agent has no evidenced independent control route".into(),
            ))
        }
    };
    crate::api::sessions::authorize(state, subject, &id, "control").await?;
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    let input = InputRequest {
        text: request.text.clone(),
        attachments: request
            .attachments
            .into_iter()
            .map(|a| AttachmentData {
                name: a.name,
                mime_type: a.r#type,
                data: a.data,
            })
            .collect(),
        idempotency_key: Some(request.idempotency_key.clone()),
    };
    let kind = match request.action {
        ReadControlAction::Send => CommandKind::Send,
        ReadControlAction::Steer => CommandKind::Steer,
        ReadControlAction::Queue => CommandKind::Queue,
        ReadControlAction::NativeSteer => CommandKind::NativeSteer,
        ReadControlAction::NativeQueue => CommandKind::NativeQueue,
        ReadControlAction::Stop | ReadControlAction::Interrupt => CommandKind::Stop,
        ReadControlAction::Approval => CommandKind::Approval,
        ReadControlAction::Configure => CommandKind::Configure,
    };
    let mut content = input.clone();
    if !matches!(
        kind,
        CommandKind::Send
            | CommandKind::Steer
            | CommandKind::Queue
            | CommandKind::NativeSteer
            | CommandKind::NativeQueue
    ) {
        content.text = Some(
            serde_json::to_string(&(
                request.action,
                &request.lease,
                &request.affected_execution,
                &request.config,
                &request.answer,
            ))
            .map_err(internal)?,
        );
    }
    let target = CommandTarget::Agent {
        agent: AgentRef::Session(id.clone()),
    };
    if let Some(receipt) = replay_trace_input(state, subject, &target, kind, &content).await? {
        return Ok(receipt);
    }
    let lock = state.env_bridge.session_command_lock(&id).await;
    let _guard = lock.lock().await;
    if let Some(receipt) = replay_trace_input(state, subject, &target, kind, &content).await? {
        return Ok(receipt);
    }
    let availability = control_availability(state, subject, &AgentRef::Session(id.clone())).await?;
    let sylph_protocol::trace::ReadControlAvailability::Available { actions, .. } = availability
    else {
        return Err(Error::Unsupported("session control is unavailable".into()));
    };
    if !actions.contains(&request.action) {
        return Err(Error::Unsupported(
            "this vendor does not offer the requested control action".into(),
        ));
    }
    if matches!(
        request.action,
        ReadControlAction::Stop | ReadControlAction::Interrupt | ReadControlAction::Steer
    ) {
        let current = state
            .store
            .control_state(&id)
            .await?
            .ok_or_else(|| Error::InvalidRequest("no current execution lease".into()))?;
        if !current.1 || request.lease.is_none() || current.0 != request.lease {
            return Err(Error::InvalidRequest(
                "execution lease changed before dispatch".into(),
            ));
        }
    }
    if let Some(answer) = request.answer.as_ref() {
        let pending = state
            .store
            .current_request(&id, &answer.request_id)
            .await?
            .ok_or(Error::RequestNotFound)?;
        validate_resolution(
            &pending,
            &sylph_protocol::Resolution {
                request_id: answer.request_id.clone(),
                option: answer.option_id.clone(),
                input: answer.input.clone(),
            },
        )?;
    }
    if request.action == ReadControlAction::Approval && request.answer.is_none() {
        return Err(Error::InvalidRequest("request answer required".into()));
    }
    if request.action == ReadControlAction::Configure && request.config.is_none() {
        return Err(Error::InvalidRequest("config change required".into()));
    }
    let epoch = state
        .trace
        .evidence()
        .authorize_scope(
            &IngestionScope::Agent(AgentRef::Session(id.clone())),
            &[AgentRef::Session(id.clone())],
        )
        .await
        .map_err(internal)?;
    let accepted = accept_input(
        state,
        subject,
        CommandTarget::Agent {
            agent: AgentRef::Session(id.clone()),
        },
        epoch,
        kind,
        &content,
    )
    .await?;
    if accepted.state == CommandState::Accepted && kind != CommandKind::Queue {
        let wire = match request.action {
            ReadControlAction::Send => EnvironmentCommand::Send {
                session_id: id.clone(),
                turn_id: accepted.acceptance.command_id.clone(),
                input: wire_input(&accepted.acceptance.input)?,
            },
            ReadControlAction::Stop | ReadControlAction::Interrupt => {
                EnvironmentCommand::Interrupt {
                    session_id: id.clone(),
                    expected_turn_id: request.lease,
                }
            }
            ReadControlAction::NativeSteer => EnvironmentCommand::QueueSteering {
                session_id: id.clone(),
                text: input.text.unwrap_or_default(),
            },
            ReadControlAction::Steer => EnvironmentCommand::Steer {
                session_id: id.clone(),
                text: input.text.unwrap_or_default(),
                expected_turn_id: request.lease,
            },
            ReadControlAction::Queue | ReadControlAction::NativeQueue => {
                EnvironmentCommand::Queue {
                    session_id: id.clone(),
                    text: input.text.unwrap_or_default(),
                }
            }
            ReadControlAction::Configure => {
                let config = request
                    .config
                    .ok_or_else(|| Error::InvalidRequest("config change required".into()))?;
                EnvironmentCommand::SetConfig {
                    session_id: id.clone(),
                    id: config.id,
                    value: config.value,
                }
            }
            ReadControlAction::Approval => {
                let answer = request
                    .answer
                    .ok_or_else(|| Error::InvalidRequest("request answer required".into()))?;
                EnvironmentCommand::Resolve {
                    session_id: id.clone(),
                    resolution: serde_json::json!({"requestId":answer.request_id,"option":answer.option_id,"input":answer.input}),
                }
            }
        };
        dispatch(state, subject, &accepted, wire).await?;
    }
    let command = state
        .store
        .command(&accepted.acceptance.command_id)
        .await
        .map_err(internal)?
        .ok_or(Error::RequestNotFound)?;
    command_receipt(state, command).await
}

pub async fn control_availability(
    state: &AppState,
    subject: &str,
    agent: &AgentRef,
) -> Result<sylph_protocol::trace::ReadControlAvailability> {
    use sylph_protocol::trace::{AgentAddress, ReadControlAvailability};
    let AgentRef::Session(id) = agent else {
        return Ok(ReadControlAvailability::Unavailable {
            reason: "embedded agent has no evidenced independent control route".into(),
        });
    };
    if !state
        .store
        .has_capability(subject, id.environment.as_str(), "control")
        .await?
    {
        return Ok(ReadControlAvailability::Unavailable {
            reason: "control capability required".into(),
        });
    }
    if !state
        .env_bridge
        .environment_connected(&id.environment)
        .await
    {
        return Ok(ReadControlAvailability::Unavailable {
            reason: "environment unreachable".into(),
        });
    }
    let outcome = state
        .env_bridge
        .remote_session_command(
            id,
            EnvironmentCommand::Capabilities {
                session_id: id.clone(),
            },
            IdempotencyPolicy::Replayable,
        )
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let value = match outcome {
        CommandOutcome::Accepted {
            result: Some(value),
        } => value,
        _ => {
            return Ok(ReadControlAvailability::Unavailable {
                reason: "vendor control unavailable".into(),
            })
        }
    };
    if value
        .pointer("/link/control/available")
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Ok(ReadControlAvailability::Unavailable {
            reason: value
                .pointer("/link/control/reason")
                .map(ToString::to_string)
                .unwrap_or_else(|| "vendor control unavailable".into()),
        });
    }
    let supports: Vec<sylph_protocol::Capability> = serde_json::from_value(
        value
            .get("supports")
            .cloned()
            .ok_or_else(|| Error::Internal("capability reply lacks supports".into()))?,
    )
    .map_err(internal)?;
    let mut actions = vec![ReadControlAction::Send, ReadControlAction::Queue];
    for supported in supports {
        actions.push(match supported {
            sylph_protocol::Capability::Interrupt => ReadControlAction::Stop,
            sylph_protocol::Capability::Steer => ReadControlAction::Steer,
            sylph_protocol::Capability::Queue => ReadControlAction::NativeQueue,
            sylph_protocol::Capability::QueueSteering => ReadControlAction::NativeSteer,
            sylph_protocol::Capability::SetConfig => ReadControlAction::Configure,
        });
    }
    let lease = state
        .store
        .control_state(id)
        .await?
        .and_then(|(lease, active)| active.then_some(lease).flatten());
    if lease.is_none() {
        actions
            .retain(|action| !matches!(action, ReadControlAction::Stop | ReadControlAction::Steer));
    } else {
        // Actions describe what can be dispatched now, not just vendor support.
        // A held execution accepts steer/queue separately; another Send is refused.
        actions.retain(|action| *action != ReadControlAction::Send);
    }
    if !state.store.current_requests(id).await?.is_empty() {
        actions.push(ReadControlAction::Approval);
    }
    Ok(ReadControlAvailability::Available {
        target: ReadControlTarget::Session {
            agent: AgentAddress::of(agent),
            session_id: id.encode(),
        },
        actions,
        lease,
        affected_execution: None,
    })
}

pub async fn trace_command_list(
    state: &AppState,
    subject: &str,
    agent: &AgentRef,
) -> Result<Vec<ReadCommandReceipt>> {
    let AgentRef::Session(id) = agent else {
        return Ok(vec![]);
    };
    crate::api::sessions::authorize(state, subject, id, "observe").await?;
    let commands = state
        .store
        .commands_for_agent(subject, agent, 64)
        .await
        .map_err(internal)?;
    let mut visible = vec![];
    for command in commands {
        match ensure_visible(state.trace.evidence(), &command).await {
            Ok(()) => visible.push(command),
            Err(Error::RequestNotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let commands = visible;
    let mut receipts = Vec::with_capacity(commands.len());
    for command in commands {
        receipts.push(command_receipt(state, command).await?);
    }
    Ok(receipts)
}

async fn ensure_visible(
    evidence: &crate::trace::evidence::EvidenceStore,
    command: &AcceptedCommand,
) -> Result<()> {
    let scope = command.acceptance.target.ingestion_scope();
    if evidence
        .current_scope_epoch(&scope)
        .await
        .map_err(internal)?
        .as_ref()
        != Some(&command.acceptance.ingestion_authorization_epoch)
    {
        return Err(Error::RequestNotFound);
    }
    Ok(())
}

async fn command_receipt(state: &AppState, command: AcceptedCommand) -> Result<ReadCommandReceipt> {
    let error = match state.store.dispatch_result(&command.acceptance.command_id).await.map_err(internal)? {
        Some(DeliveryFact::Rejected { receipt }) => {
            match serde_json::from_slice::<CommandOutcome>(&receipt).map_err(internal)? {
                CommandOutcome::Rejected { message, .. } => Some(message),
                _ => return Err(internal("rejected delivery has no rejection receipt")),
            }
        }
        Some(DeliveryFact::OutcomeUnknown { reason }) => Some(reason),
        _ => None,
    };
    let editable =
        command.acceptance.kind == CommandKind::Queue && command.state == CommandState::Accepted;
    let text = if command.current_input.media_type == "text/plain" {
        String::from_utf8(command.current_input.bytes.clone()).ok()
    } else {
        serde_json::from_slice::<serde_json::Value>(&command.current_input.bytes)
            .ok()
            .and_then(|v| v.get("text").and_then(|v| v.as_str()).map(str::to_owned))
    };
    Ok(ReadCommandReceipt {
        command_id: command.acceptance.command_id,
        observation_ref: command.accepted_observation,
        state: match command.state {
            CommandState::Accepted if editable => ReadCommandState::Queued,
            CommandState::Accepted => ReadCommandState::Accepted,
            CommandState::Dispatching | CommandState::Unknown => ReadCommandState::Unconfirmed,
            CommandState::ReceiverAccepted => ReadCommandState::ReceiverAccepted,
            CommandState::Consumed => ReadCommandState::Admitted,
            CommandState::Rejected => ReadCommandState::Rejected,
            CommandState::Cancelled => ReadCommandState::Cancelled,
            CommandState::Conflicting => ReadCommandState::Failed,
        },
        revision: command.current_revision,
        text,
        error,
        editable,
        cancellable: editable,
    })
}

#[derive(Deserialize)]
pub struct EditCommand {
    pub revision: u64,
    pub text: String,
}
#[derive(Deserialize)]
pub struct CancelCommand {
    pub revision: u64,
}
pub async fn edit(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Json(edit): Json<EditCommand>,
) -> Result<Json<ReadCommandReceipt>> {
    let command = state
        .store
        .command(&id)
        .await
        .map_err(internal)?
        .ok_or(Error::RequestNotFound)?;
    ensure_visible(state.trace.evidence(), &command).await?;
    let scope = command.acceptance.target.ingestion_scope();
    if !state
        .store
        .has_capability(&subject.0, scope.environment().as_str(), "control")
        .await?
    {
        return Err(Error::Forbidden("control capability required".into()));
    }
    let mut input = command.current_input.clone();
    input.revision = format!("revision-{}", uuid::Uuid::new_v4());
    if input.media_type == "text/plain" {
        input.bytes = edit.text.into_bytes();
    } else {
        let mut value: serde_json::Value =
            serde_json::from_slice(&input.bytes).map_err(internal)?;
        value["text"] = serde_json::Value::String(edit.text);
        input.bytes = serde_json::to_vec(&value).map_err(internal)?;
    }
    let command = state
        .store
        .revise_queued(&id, edit.revision, input, authorization(&subject.0, None))
        .await
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    flush(&state).await?;
    Ok(Json(command_receipt(&state, command).await?))
}
pub async fn cancel(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
    Query(cancel): Query<CancelCommand>,
) -> Result<Json<ReadCommandReceipt>> {
    let command = state
        .store
        .command(&id)
        .await
        .map_err(internal)?
        .ok_or(Error::RequestNotFound)?;
    ensure_visible(state.trace.evidence(), &command).await?;
    let scope = command.acceptance.target.ingestion_scope();
    if !state
        .store
        .has_capability(&subject.0, scope.environment().as_str(), "control")
        .await?
    {
        return Err(Error::Forbidden("control capability required".into()));
    }
    let command = state
        .store
        .cancel_queued(&id, cancel.revision, &subject.0)
        .await
        .map_err(|e| Error::InvalidRequest(e.to_string()))?;
    flush(&state).await?;
    Ok(Json(command_receipt(&state, command).await?))
}

pub fn spawn_dispatcher(state: AppState) {
    let state = state.downgrade();
    tokio::spawn(async move {
        let mut cursor = 0;
        let mut failed = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let Some(state) = state.upgrade() else {
                break;
            };
            match dispatch_queued(&state, &mut cursor).await {
                Ok(()) => failed = false,
                Err(error) => {
                    if failed {
                        tracing::debug!(%error,"queued command dispatch remains stalled");
                    } else {
                        tracing::error!(%error,"queued command dispatch failed");
                    }
                    failed = true;
                }
            }
        }
    });
}
async fn dispatch_queued(state: &AppState, cursor: &mut u64) -> Result<()> {
    // One earliest accepted input per target preserves local FIFO. Rotating the
    // bounded candidate window prevents busy/offline targets from starving others.
    let commands = state
        .store
        .queued_commands(*cursor, 32)
        .await
        .map_err(internal)?;
    let exhausted = commands.len() < 32;
    for command in commands {
        *cursor = command.accepted_observation.producer_seq;
        let Some(AgentRef::Session(id)) = command.agent() else {
            continue;
        };
        if !state
            .env_bridge
            .environment_connected(&id.environment)
            .await
        {
            continue;
        }
        let lock = state.env_bridge.session_command_lock(id).await;
        let Ok(_guard) = lock.try_lock() else {
            continue;
        };
        if state
            .store
            .control_state(id)
            .await?
            .is_some_and(|(_, active)| active)
        {
            continue;
        }
        let Some(session) = state.store.get_session(id).await? else {
            continue;
        };
        if ensure_visible(state.trace.evidence(), &command)
            .await
            .is_err()
        {
            continue;
        }
        let subject = command.acceptance.authorization.subject.clone();
        if !state
            .store
            .has_capability(&subject, id.environment.as_str(), "control")
            .await?
        {
            continue;
        }
        let attached = state
            .env_bridge
            .remote_attach(&session)
            .await
            .map_err(|e| Error::LinkFailed(e.to_string()))?;
        if !matches!(attached, CommandOutcome::Accepted { .. }) {
            continue;
        }
        let wire = EnvironmentCommand::Send {
            session_id: id.clone(),
            turn_id: command.acceptance.command_id.clone(),
            input: wire_input(&command.current_input)?,
        };
        dispatch(state, &subject, &command, wire).await?;
    }
    if exhausted {
        *cursor = 0;
    }
    Ok(())
}
