use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use sylph_protocol::SessionId;

/// One journaled event.
///
/// `reconciled_at_ms` is gone: `docs/roadmap.md` § Audit measured it vestigial, and a
/// rewrite is when a vestigial column goes rather than being carried into a new
/// schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: Option<i64>,
    #[serde(with = "crate::models::session_id_string")]
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub seq: i32,
    pub kind: String,
    pub phase: Option<String>,
    pub stream_id: Option<String>,
    /// The delegated trajectory this event happened in; `None` is the root turn.
    ///
    /// See `docs/session-protocol.md` § Delegated trajectories. A `subagent` event's
    /// `stream_id` *is* a trajectory id, so a nested delegation is one whose `span`
    /// names another — which is the whole of how arbitrary depth and parallel workflow
    /// phases are represented.
    pub span: Option<String>,
    pub name: Option<String>,
    pub text: String,
    pub data_json: String,
    pub is_error: bool,
    pub stability: String,
    pub source: String,
    pub created_at_ms: i64,
}

impl FromRow<'_, sqlx::sqlite::SqliteRow> for Event {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            session_id: crate::models::session::triple_from_row(
                row,
                "session_env",
                "session_agent",
                "session_native",
            )?,
            turn_id: row.try_get("turn_id")?,
            seq: row.try_get("seq")?,
            kind: row.try_get("kind")?,
            phase: row.try_get("phase")?,
            stream_id: row.try_get("stream_id")?,
            span: row.try_get("span")?,
            name: row.try_get("name")?,
            text: row.try_get("text")?,
            data_json: row.try_get("data_json")?,
            is_error: row.try_get("is_error")?,
            stability: row.try_get("stability")?,
            source: row.try_get("source")?,
            created_at_ms: row.try_get("created_at_ms")?,
        })
    }
}

impl Event {
    pub fn data(&self) -> serde_json::Value {
        serde_json::from_str(&self.data_json).unwrap_or(serde_json::json!({}))
    }
}
