//! Current environment and vendor control capability surfaces.
use crate::{
    error::{Error, Result},
    link_state::LinkState,
    middleware::auth::SubjectId,
    AppState,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
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
            authorization: None,
            control_block: None,
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
    match sylph_protocol::RejectCode::parse(code) {
        Some(
            sylph_protocol::RejectCode::InvalidValue
            | sylph_protocol::RejectCode::ConfigValueNotOffered
            | sylph_protocol::RejectCode::ConfigOptionUnknown,
        ) => Error::InvalidRequest(message),
        Some(sylph_protocol::RejectCode::UnsupportedField) => Error::Unsupported(message),
        Some(
            sylph_protocol::RejectCode::SessionNotHosted
            | sylph_protocol::RejectCode::AgentUnavailable
            | sylph_protocol::RejectCode::AgentStartFailed,
        ) => Error::LinkFailed(message),
        _ => Error::LinkFailed(format!("{code}: {message}")),
    }
}

fn remote_config_refusal(code: &str, message: String) -> Error {
    match sylph_protocol::RejectCode::parse(code) {
        Some(sylph_protocol::RejectCode::UnsupportedField) => Error::InvalidRequest(message),
        _ => remote_refusal(code, message),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedConfigRequest {
    pub id: String,
    pub value: serde_json::Value,
}

/// POST /api/v1/sessions/draft/{id}/config
pub async fn prepared_set_config(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(draft_id): Path<String>,
    Json(body): Json<PreparedConfigRequest>,
) -> Result<Json<LinkState>> {
    if let Ok(draft) = state
        .env_bridge
        .authorize_remote_draft(&draft_id, &subject.0, "control")
        .await
    {
        let outcome = state
            .env_bridge
            .remote_draft_set_config(&draft, body.id, body.value)
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        match outcome {
            sylph_protocol::CommandOutcome::Accepted { .. } => {}
            sylph_protocol::CommandOutcome::Rejected { code, message } => {
                return Err(remote_config_refusal(&code, message));
            }
            sylph_protocol::CommandOutcome::Unknown { message } => {
                return Err(Error::LinkFailed(format!(
                    "remote draft config outcome is unknown: {}",
                    message.unwrap_or_default()
                )));
            }
        }
        let refreshed = state
            .env_bridge
            .remote_draft_capabilities(&draft)
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        return Ok(Json(remote_link_state(draft.agent, refreshed)?));
    }
    // Draft handles are issued by an envagent. A handle unknown to the bridge
    // is gone; it must never fall through to an in-process execution path.
    Err(Error::SessionNotFound)
}

/// DELETE /api/v1/sessions/draft/{id}
pub async fn discard_prepared(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(draft_id): Path<String>,
) -> Result<StatusCode> {
    let _ = state
        .env_bridge
        .authorize_remote_draft(&draft_id, &subject.0, "control")
        .await
        .map_err(|_| Error::SessionNotFound)?;
    state.env_bridge.discard_remote_draft(&draft_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/sessions/draft/{id}/capabilities
pub async fn prepared_capabilities(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<LinkState>> {
    if state.env_bridge.remote_draft_promoting(&id).await {
        return Err(Error::SessionAlreadyRunning);
    }
    if let Ok(draft) = state
        .env_bridge
        .authorize_remote_draft(&id, &subject.0, "observe")
        .await
    {
        let value = state
            .env_bridge
            .remote_draft_capabilities(&draft)
            .await
            .map_err(|error| Error::LinkFailed(format!("{error:#}")))?;
        return Ok(Json(remote_link_state(draft.agent, value)?));
    }
    Err(Error::SessionNotFound)
}

pub async fn capabilities(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<LinkState>> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "observe").await?;
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    if !state
        .env_bridge
        .environment_connected(&id.environment)
        .await
    {
        return Ok(Json(LinkState::detached_remote()));
    }
    let outcome = state
        .env_bridge
        .remote_session_command(
            &id,
            sylph_protocol::EnvironmentCommand::Capabilities {
                session_id: id.clone(),
            },
            sylph_protocol::IdempotencyPolicy::Replayable,
        )
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let value = crate::env::EnvBridge::remote_outcome_value(outcome)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    let mut link = remote_link_state(id.agent.to_string(), value)?;
    link.authorization = state
        .store
        .authorization_status(&subject.0, id.environment.as_str(), "control")
        .await?
        .map(str::to_owned);
    if !state
        .store
        .has_capability(&subject.0, id.environment.as_str(), "control")
        .await?
    {
        link.control_block = Some(crate::link_state::ControlBlock {
            kind: "authorization".into(),
            reason: "control capability required".into(),
        });
    }
    Ok(Json(link))
}
pub async fn attach(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "control").await?;
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    let outcome = state
        .env_bridge
        .remote_attach(&session)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    Ok(Json(
        crate::env::EnvBridge::remote_outcome_value(outcome)
            .await
            .map_err(|e| Error::LinkFailed(e.to_string()))?,
    ))
}
pub async fn detach(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "control").await?;
    state
        .env_bridge
        .close_remote_session(&id)
        .await
        .map_err(|e| Error::LinkFailed(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn list_requests(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let id = crate::api::sessions::decode_id(&id)?;
    crate::api::sessions::authorize(&state, &subject.0, &id, "observe").await?;
    Ok(Json(
        serde_json::json!({"requests":state.store.current_requests(&id).await?}),
    ))
}
