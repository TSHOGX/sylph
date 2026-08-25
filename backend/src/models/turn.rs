use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    pub incarnation_id: Option<String>,
    pub status: String,
    /// `user` or `agent`. See `migrations/006_turn_origin.sql`.
    ///
    /// An `agent` turn is one nobody asked for: the agent spoke after its previous
    /// turn had settled, which `claude` does to report a backgrounded delegate
    /// finishing. It is a real turn in every mechanical sense — it consolidates, it
    /// orders by sequence, its events journal normally — and this only records that
    /// its prompt did not come from the user, which a reader needs and an empty
    /// `request_text` cannot say on its own.
    #[serde(default = "default_origin")]
    pub origin: String,
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

fn default_origin() -> String {
    "user".to_string()
}

impl Turn {
    /// Whether this turn was started by the agent rather than by the user.
    pub fn is_agent_initiated(&self) -> bool {
        self.origin == "agent"
    }

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
