use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use sylph_protocol::SessionId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    pub request_id: String,
    #[serde(with = "crate::models::session_id_string")]
    pub session_id: SessionId,
    pub origin_subject: String,
    pub kind: String,
    pub text: String,
    pub target_turn_id: Option<String>,
    pub server_seq: i64,
    pub state: String,
    pub idempotency_key: Option<String>,
    pub vendor_receipt: Option<String>,
    pub error_message: Option<String>,
    pub created_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
    pub failed_at_ms: Option<i64>,
    pub revision: i64,
}

impl FromRow<'_, sqlx::sqlite::SqliteRow> for ControlRequest {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            request_id: row.try_get("request_id")?,
            session_id: crate::models::session::triple_from_row(
                row,
                "session_env",
                "session_agent",
                "session_native",
            )?,
            origin_subject: row.try_get("origin_subject")?,
            kind: row.try_get("kind")?,
            text: row.try_get("text")?,
            target_turn_id: row.try_get("target_turn_id")?,
            server_seq: row.try_get("server_seq")?,
            state: row.try_get("state")?,
            idempotency_key: row.try_get("idempotency_key")?,
            vendor_receipt: row.try_get("vendor_receipt")?,
            error_message: row.try_get("error_message")?,
            created_at_ms: row.try_get("created_at_ms")?,
            consumed_at_ms: row.try_get("consumed_at_ms")?,
            failed_at_ms: row.try_get("failed_at_ms")?,
            revision: row.try_get("revision")?,
        })
    }
}
