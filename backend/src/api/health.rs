use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use axum::{extract::State, Json};
use serde::Serialize;

use crate::{error::Result, AppState};

/// Process start time, for uptime reporting.
///
/// Set on the first health or diagnostics call rather than at startup: a
/// `OnceLock` keeps the module self-contained, and the difference is the few
/// milliseconds before the first request.
static START: OnceLock<Instant> = OnceLock::new();

fn uptime_seconds() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_secs()
}

/// Call once during startup so uptime is measured from boot, not first request.
pub fn mark_start() {
    let _ = START.set(Instant::now());
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    ok: bool,
    api_version: String,
    schema_version: i64,
    tracedb: TraceDbHealth,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDbHealth {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

/// GET /api/v1/health
///
/// Unauthenticated (see the middleware's exempt list) so a supervisor or
/// LaunchAgent probe can check liveness without holding a cookie. It therefore
/// reports only availability, never counts or paths.
pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>> {
    let schema_version = state.store.schema_version().await.unwrap_or(0);
    let tracedb = state.tracedb.probe().await;

    Ok(Json(HealthResponse {
        ok: true,
        api_version: "1".to_string(),
        schema_version,
        tracedb: TraceDbHealth {
            available: tracedb.is_some(),
            version: tracedb,
        },
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsResponse {
    ok: bool,
    version: String,
    uptime_seconds: u64,
    sessions: HashMap<String, i64>,
    turns: HashMap<String, i64>,
    agents: Vec<String>,
    broken_runners: HashMap<String, String>,
    /// Turns with a live child process right now. May be lower than the journal's
    /// running count if a process died without the reaper having recorded it yet.
    active_processes: usize,
}

/// GET /api/v1/diagnostics
///
/// Authenticated: unlike `/health`, this exposes session and turn counts plus
/// runner failure messages, which can carry local filesystem paths.
pub async fn diagnostics(State(state): State<AppState>) -> Result<Json<DiagnosticsResponse>> {
    let sessions = state
        .store
        .count_sessions_by_status()
        .await?
        .into_iter()
        .collect();

    let turns = state
        .store
        .count_turns_by_status(None)
        .await?
        .into_iter()
        .collect();

    let (agents, broken_runners) = {
        let registry = state.registry.lock().await;
        let mut agents: Vec<String> = registry
            .list_agents()
            .iter()
            .map(|a| a.name.clone())
            .collect();
        // Discovery order follows directory iteration, which is not stable;
        // sort so the output does not churn between calls.
        agents.sort();
        (agents, registry.broken_runners().clone())
    };

    Ok(Json(DiagnosticsResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: uptime_seconds(),
        sessions,
        turns,
        agents,
        broken_runners,
        active_processes: state.supervisor.active_turn_count().await,
    }))
}
