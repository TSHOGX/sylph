use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub origin: String,
    pub identity_mode: String,
    pub lifecycle_mode: String,
    pub agent: String,
    #[serde(skip_serializing)]
    pub backend_session_id: Option<String>,
    /// TraceDB id this session was imported from, when it was.
    ///
    /// Serialized because the archive browser needs it: without it the client
    /// cannot tell that an archived row it is about to list is the same
    /// conversation as a live session, and would offer the user two ways into one
    /// history. `backend_session_id` stays hidden — that is runner-internal
    /// plumbing with no client use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_session_id: Option<String>,
    pub workdir: String,
    pub title: Option<String>,
    pub flags_json: String,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Session {
    pub fn flags(&self) -> serde_json::Value {
        serde_json::from_str(&self.flags_json).unwrap_or(serde_json::json!({}))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    pub flags: Option<serde_json::Value>,
}
