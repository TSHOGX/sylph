use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use std::collections::HashMap;

use crate::{error::Result, agents::AgentDescriptor, AppState};

#[derive(Serialize)]
pub struct AgentsResponse {
    agents: Vec<AgentDescriptor>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    broken: HashMap<String, String>,
}

/// GET /api/v1/agents - List discovered agents
pub async fn list(State(state): State<AppState>) -> Result<Json<AgentsResponse>> {
    let registry = state.registry.lock().await;
    let agents = registry.list_agents();
    let broken = registry.broken_adapters().clone();

    Ok(Json(AgentsResponse { agents, broken }))
}

/// POST /api/v1/agents/refresh - Refresh runner discovery
pub async fn refresh(State(state): State<AppState>) -> Result<(StatusCode, Json<AgentsResponse>)> {
    let mut registry = state.registry.lock().await;
    registry
        .refresh()
        .await
        .map_err(|e| crate::error::Error::Internal(format!("Runner refresh failed: {}", e)))?;

    let agents = registry.list_agents();
    let broken = registry.broken_adapters().clone();

    Ok((StatusCode::OK, Json(AgentsResponse { agents, broken })))
}
