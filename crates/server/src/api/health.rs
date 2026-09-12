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
    /// Per-environment connectivity and inventory summary. Detailed paths and
    /// session counts remain authenticated on diagnostics/environments.
    environments: Vec<serde_json::Value>,
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
    let schema_version = state.store.schema_version().await.unwrap_or(0);
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
        schema_version,
        agents: AgentsHealth { available },
        environments: Vec::new(),
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
    /// Which agents this build can run.
    ///
    /// `broken_adapters` and `agent_roots` are gone with discovery. Both existed because
    /// an adapter was a *directory to be found*, so "no agents" needed to name where it
    /// looked — a missing install and a mistyped path gave the same empty list and
    /// needed opposite fixes. An adapter is now linked into the binary, so there is no
    /// path to get wrong and no broken-but-present state to report.
    agents: Vec<String>,
    /// Turns the supervisor is running right now. May be lower than the journal's
    /// running count if a link died without the reaper having recorded it yet.
    ///
    /// # Why this is not called `activeProcesses`
    ///
    /// It was, and the value has always been `active_turn_count()` — a turn count
    /// published under a process name. That was harmless only while every process
    /// was one Sylph spawned, so the two numbers differed by nothing a reader could
    /// observe. It stops being harmless with adopted sessions, where a process is
    /// alive and is not ours: "active processes" would then be read as a count of
    /// agents on this machine, which this number has never been and cannot become
    /// without asking every vendor. Name the thing that is counted.
    active_turns: usize,
    /// The last retention pass, or `null` before the first one completes.
    ///
    /// Reported because retention's correct behaviour is usually "nothing was
    /// dropped", which is indistinguishable from a broken pass without the counts.
    /// `withheld` beside `terminal` is what separates the two: a terminal session
    /// is *supposed* to keep everything forever, while a delegated session that
    /// keeps withholding means the archive is not confirming it — and an
    /// all-`unchanged` pass means the ingestion daemon has stopped advancing.
    retention: Option<crate::storage::PassReport>,
    /// Raw native records that failed to reach durable history since startup.
    ///
    /// A timeline can still look healthy after this counter increments, but its
    /// re-projection source is incomplete; exposing the count makes that loss actionable.
    native_record_failures: u64,
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

    Ok(Json(DiagnosticsResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: uptime_seconds(),
        sessions,
        turns,
        agents: state
            .env_bridge
            .list_environments()
            .await
            .unwrap_or_default()
            .into_iter()
            .flat_map(|environment| {
                environment
                    .agents
                    .into_iter()
                    .filter(|agent| agent.available)
                    .map(|agent| agent.name)
            })
            .collect(),
        active_turns: state.store.active_turn_count().await?,
        retention: state.retention.last_pass().await,
        native_record_failures: state.history.native_record_failures(),
    }))
}
