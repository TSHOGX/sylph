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
    /// Whether this build can run any agent at all.
    ///
    /// A server with zero agents answers every other probe successfully while no
    /// session can run and no command can be offered, so liveness alone reports healthy
    /// for a process that cannot do its job — and `AGENTS.md` records what that cost:
    /// the symptom was a composer ignoring the `/` key, three layers from the cause.
    ///
    /// A boolean rather than a count, to keep this endpoint free of anything an
    /// unauthenticated caller should not learn. **Which** agent is missing on **which**
    /// machine is `GET /environments`, and that is the better question — it distinguishes
    /// a missing CLI from a sleeping laptop from an exhausted sandbox node.
    ///
    /// Whether any agent is usable, which is the one health fact a client cannot infer.
    /// Zero agents is a silent total failure three layers from its symptom, so it is
    /// reported here explicitly (`AGENTS.md` § Rules that keep costing time).
    agents: AgentsHealth,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsHealth {
    available: bool,
}

/// GET /api/v1/health
///
/// Unauthenticated (see the middleware's exempt list) so a supervisor or
/// LaunchAgent probe can check liveness without holding a cookie. It therefore
/// reports only availability, never counts or paths.
pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>> {
    let available = state
        .env_bridge
        .list_environments()
        .await
        .unwrap_or_default()
        .into_iter()
        .any(|environment| environment.connected && !environment.agents.is_empty());

    Ok(Json(HealthResponse {
        ok: true,
        api_version: "1".to_string(),
        agents: AgentsHealth { available },
    }))
}

pub async fn diagnostics(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    let sessions = state.store.count_sessions_by_status().await?;
    let evidence = state
        .trace
        .evidence()
        .evidence_cut()
        .await
        .map_err(|e| crate::error::Error::Internal(e.to_string()))?;
    let outbox = state
        .store
        .progress(crate::models::now_ms())
        .await
        .map_err(|e| crate::error::Error::Internal(e.to_string()))?;
    Ok(Json(
        serde_json::json!({"ok":true,"version":env!("CARGO_PKG_VERSION"),"uptimeSeconds":uptime_seconds(),"sessions":sessions,"evidenceCut":evidence,"pendingOutbox":outbox.pending_count,"oldestPendingOutboxAgeMs":outbox.oldest_pending_age_ms}),
    ))
}
