use crate::events::StreamEvent;
use crate::models::now_ms;
pub type TurnWaiters = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<Outcome>>>,
>;

#[derive(Debug, Clone)]
pub enum Outcome {
    Completed { stop_reason: Option<String> },
    Failed { message: String },
    Interrupted,
    LinkLost { detail: Option<String> },
}

impl Outcome {
    pub fn as_status(&self) -> (&'static str, Option<&'static str>, String) {
        match self {
            Self::Completed { .. } => ("completed", None, String::new()),
            Self::Failed { message } => ("failed", Some("TURN_FAILED"), message.clone()),
            Self::Interrupted => (
                "cancelled",
                Some("TURN_INTERRUPTED"),
                "Interrupted by user".into(),
            ),
            Self::LinkLost { detail } => (
                "failed",
                Some("LINK_LOST"),
                detail
                    .as_ref()
                    .map(|d| format!("The agent stopped during this turn: {d}"))
                    .unwrap_or_else(|| "The agent stopped during this turn".into()),
            ),
        }
    }
}

pub fn server_event(kind: &str, text: String, data: Option<serde_json::Value>) -> StreamEvent {
    StreamEvent {
        seq: 0,
        arrival_turn_id: None,
        kind: kind.into(),
        phase: None,
        stream_id: None,
        span: None,
        interior_state: None,
        name: None,
        text,
        data,
        is_error: false,
        stability: "stable".into(),
        source: "server".into(),
        arrival_at_ms: now_ms(),
        recorded_at_ms: None,
        body_ref: None,
        tool_summary: None,
        created_at_ms: now_ms(),
        started_at_ms: None,
    }
}
