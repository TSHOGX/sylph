use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// One agent-native session behind a product session.
///
/// A Sylph session accumulates incarnations as its lifecycle policy resets the
/// agent context (24h idle, the 04:00 cutoff, or an explicit user reset). A
/// native session has exactly one and never resets — see
/// `docs/architecture.md`, "Backend Incarnations".
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Incarnation {
    pub id: String,
    pub session_id: String,
    pub agent: String,
    /// Opaque agent-native session id. Withheld from API responses: the
    /// OpenAPI contract states backend identifiers are never exposed.
    #[serde(skip_serializing)]
    pub backend_session_id: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub reset_reason: Option<String>,
    #[serde(skip_serializing)]
    pub source_trace_session_id: Option<String>,
    #[serde(skip_serializing)]
    pub resulting_trace_session_id: Option<String>,
    pub status: String,
}
