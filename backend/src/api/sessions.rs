use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::{
    db::store::SessionFilter,
    error::{Error, Result},
    events::StreamEvent,
    models::{CreateSessionRequest, Session, Turn},
    AppState,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    agent: Option<String>,
    origin: Option<String>,
    workdir: Option<String>,
    updated_after_ms: Option<i64>,
    updated_before_ms: Option<i64>,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct SessionListResponse {
    sessions: Vec<Session>,
}

/// Blank text in a query string is the natural encoding of a cleared filter
/// input, so it means "unfiltered" rather than "match the empty string".
fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<SessionListResponse>> {
    let origin = non_blank(query.origin);
    if let Some(origin) = &origin {
        // Origin is a closed set that drives identity and lifecycle policy;
        // a typo must not silently return an empty list.
        if origin != "sylph" && origin != "native" {
            return Err(Error::InvalidRequest(format!(
                "Unknown origin: {origin}. Expected \"sylph\" or \"native\"."
            )));
        }
    }

    let filter = SessionFilter {
        limit: query.limit.clamp(1, 200),
        agent: non_blank(query.agent),
        origin,
        workdir: non_blank(query.workdir),
        updated_after_ms: query.updated_after_ms,
        updated_before_ms: query.updated_before_ms,
    };

    let sessions = state.store.list_sessions(&filter).await?;
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

/// One turn together with its durable events, oldest event first.
#[derive(Serialize)]
pub struct TurnWithEvents {
    turn: Turn,
    events: Vec<StreamEvent>,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    turns: Vec<TurnWithEvents>,
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    limit: i64,
}

fn default_history_limit() -> i64 {
    100
}

/// GET /api/v1/sessions/{id}/turns — durable conversation history.
///
/// The SSE endpoint covers a single turn, which is enough to watch work happen
/// but loses everything on reload. This returns the whole session so the
/// conversation survives a refresh.
///
/// Only journal rows are read, never the ephemeral projection: `delta` events
/// are transient by protocol and their text is already contained in the
/// matching `completed` event, so replaying both would duplicate every token.
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>> {
    // 404 rather than an empty list, so a mistyped id is distinguishable from
    // a session that genuinely has no turns yet.
    state
        .store
        .get_session(&id)
        .await?
        .ok_or(Error::SessionNotFound)?;

    let limit = query.limit.clamp(1, 500);

    // `list_turns` orders newest-first for the session list; a transcript reads
    // oldest-first.
    let mut turns = state.store.list_turns(&id, limit).await?;
    turns.reverse();

    let mut out = Vec::with_capacity(turns.len());
    for turn in turns {
        let events = state
            .store
            .turn_events_after(&turn.id, -1)
            .await?
            .iter()
            .map(StreamEvent::from_row)
            .collect();
        out.push(TurnWithEvents { turn, events });
    }

    Ok(Json(HistoryResponse { turns: out }))
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

