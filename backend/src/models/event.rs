use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: Option<i64>,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub seq: i32,
    pub kind: String,
    pub phase: Option<String>,
    pub stream_id: Option<String>,
    pub name: Option<String>,
    pub text: String,
    pub data_json: String,
    pub is_error: bool,
    pub stability: String,
    pub source: String,
    pub created_at_ms: i64,
    pub reconciled_at_ms: Option<i64>,
}

impl Event {
    pub fn data(&self) -> serde_json::Value {
        serde_json::from_str(&self.data_json).unwrap_or(serde_json::json!({}))
    }
}
