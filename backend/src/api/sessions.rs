use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::{
    error::{Error, Result},
    models::{CreateSessionRequest, Session},
    AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct SessionListResponse {
    sessions: Vec<Session>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionListResponse>> {
    let sessions = state.store.list_sessions(query.limit).await?;
    Ok(Json(SessionListResponse { sessions }))
}

#[derive(Serialize)]
pub struct SessionResponse {
    session: Session,
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionResponse>> {
    let session = state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;
    Ok(Json(SessionResponse { session }))
}

pub async fn create(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>)> {
    // Validate agent exists
    let registry = state.registry.lock().await;
    if !registry.has_agent(&req.agent) {
        return Err(Error::InvalidRequest(format!(
            "Unknown agent: {}. Available agents: {:?}",
            req.agent,
            registry
                .list_agents()
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
        )));
    }
    drop(registry);

    // Validate workdir exists
    if !std::path::Path::new(&req.workdir).exists() {
        return Err(Error::InvalidRequest(
            "Workdir does not exist".to_string(),
        ));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        origin: "sylph".to_string(),
        identity_mode: "sylph".to_string(),
        lifecycle_mode: "sylph".to_string(),
        agent: req.agent.clone(),
        backend_session_id: None,
        trace_session_id: None,
        workdir: req.workdir.clone(),
        title: req.title.clone(),
        flags_json: req
            .flags
            .map(|f| serde_json::to_string(&f).unwrap())
            .unwrap_or_else(|| "{}".to_string()),
        status: "idle".to_string(),
        created_at_ms: now,
        updated_at_ms: now,
    };

    let created = state.store.create_session(&session).await?;
    Ok((StatusCode::CREATED, Json(SessionResponse { session: created })))
}

