use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    pub incarnation_id: Option<String>,
    pub status: String,
    pub request_text: String,
    pub command_invocation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub runner_exit_code: Option<i32>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub stopped_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTurnRequest {
    pub text: Option<String>,
    pub attachments: Option<Vec<AttachmentData>>,
}

#[derive(Debug, Deserialize)]
pub struct AttachmentData {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub data: String, // base64
}
