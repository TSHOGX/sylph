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

impl Turn {
    /// Whether the turn has reached a state from which it cannot progress.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTurnRequest {
    pub text: Option<String>,
    pub attachments: Option<Vec<AttachmentData>>,
    /// Client-generated key making retries safe. May also arrive as the
    /// `Idempotency-Key` header; the header wins when both are present.
    pub idempotency_key: Option<String>,
    /// Named command from the agent's `--describe` output, invoked instead of
    /// a normal turn.
    pub command: Option<String>,
    /// Arguments for `command`.
    pub args: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct AttachmentData {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub data: String, // base64
}
