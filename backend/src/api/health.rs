use axum::{extract::State, Json};
use serde::Serialize;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    ok: bool,
    api_version: String,
    schema_version: String,
}

pub async fn health(State(_state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        api_version: "1".to_string(),
        schema_version: "1".to_string(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsResponse {
    ok: bool,
    version: String,
    uptime_seconds: u64,
}

pub async fn diagnostics(State(_state): State<AppState>) -> Json<DiagnosticsResponse> {
    Json(DiagnosticsResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: 0, // TODO: track startup time
    })
}
