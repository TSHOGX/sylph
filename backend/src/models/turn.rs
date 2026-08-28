use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use sylph_protocol::SessionId;

/// One turn on a session.
///
/// Keyed by its own id, but *owned* by the identity triple — three columns rather
/// than a surrogate, for the reason `docs/database-schema.md` § turns gives: a
/// surrogate key on `sessions` would have to be generated, which reintroduces the
/// second id space the triple deletes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    /// The session this turn belongs to. Serialized as the encoded triple.
    #[serde(with = "crate::models::session_id_string")]
    pub session_id: SessionId,
    pub status: String,
    /// `user` or `agent`, and **not the same concept as the deleted session
    /// `origin`.**
    ///
    /// An `agent` turn is one nobody asked for: the agent spoke after its previous
    /// turn had settled, which `claude` does to report a backgrounded delegate
    /// finishing. It is a real turn in every mechanical sense — it consolidates, it
    /// orders by sequence, its events journal normally.
    ///
    /// Load-bearing in [`crate::links::LinkManager::settle_turn`]: an agent turn
    /// never had a waiter, so this is what tells "no waiter, because there never was
    /// one" from "no waiter, because a deliberate stop already claimed it".
    /// Conflating them reported a user's stop as `LINK_LOST`, which
    /// `stopping_a_running_turn_cancels_it` catches.
    #[serde(default = "default_origin")]
    pub origin: String,
    pub request_text: String,
    pub command_invocation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    pub stopped_at_ms: Option<i64>,
}

fn default_origin() -> String {
    "user".to_string()
}

impl FromRow<'_, sqlx::sqlite::SqliteRow> for Turn {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            session_id: crate::models::session::triple_from_row(
                row,
                "session_env",
                "session_agent",
                "session_native",
            )?,
            status: row.try_get("status")?,
            origin: row.try_get("origin")?,
            request_text: row.try_get("request_text")?,
            command_invocation_id: row.try_get("command_invocation_id")?,
            idempotency_key: row.try_get("idempotency_key")?,
            error_code: row.try_get("error_code")?,
            error_message: row.try_get("error_message")?,
            created_at_ms: row.try_get("created_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            completed_at_ms: row.try_get("completed_at_ms")?,
            stopped_at_ms: row.try_get("stopped_at_ms")?,
        })
    }
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
    /// Named command from the agent's declared surface, invoked instead of a normal
    /// turn.
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
