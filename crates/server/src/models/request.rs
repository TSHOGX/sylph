use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use sylph_protocol::SessionId;

/// A question an agent raised that a human has not answered.
///
/// A resource rather than a stream message. The stream notifies an attached
/// client instantly, but the agent is blocked until someone answers, so the
/// answer has to be possible from a frontend that was not attached when it was
/// raised — including one that connects an hour later.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub id: String,
    #[serde(with = "crate::models::session_id_string")]
    pub session_id: SessionId,
    pub turn_id: String,
    /// The adapter's own id, which a resolve must echo back to it.
    ///
    /// Not serialized: it is scoped to one link and means nothing to a client,
    /// which addresses the request by `id`. Exposing it would invite a client to
    /// send it, and a client-supplied adapter id is a way to answer a request
    /// that was never raised.
    #[serde(skip)]
    pub adapter_request_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    pub title: String,
    pub detail_json: String,
    pub options_json: String,
    /// The input this request declared it wants collected, as JSON.
    ///
    /// `None` is an approval: its subject is one proposed tool input, which a client
    /// renders from `detail_json`. A request that declares a form is answered against
    /// that form instead, and the server validates the answer against it — see
    /// `docs/session-protocol.md` § Request forms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_option: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    /// Whether a lasting grant may be offered for this ask.
    ///
    /// `false` means the client must omit "always allow" entirely: the agent has
    /// said that granting always would write a rule broader than what is being
    /// asked here. Persisted rather than held on the link, because the dialog may
    /// be rendered by a client that attached long after the request was raised.
    pub allow_persistent: bool,
    /// When the agent stops waiting, if it will. Advisory; nothing here acts on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_option: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_input_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    pub created_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<i64>,
}

impl FromRow<'_, sqlx::sqlite::SqliteRow> for PendingRequest {
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
            adapter_request_id: row.try_get("adapter_request_id")?,
            kind: row.try_get("kind")?,
            tool: row.try_get("tool")?,
            tool_use_id: row.try_get("tool_use_id")?,
            title: row.try_get("title")?,
            detail_json: row.try_get("detail_json")?,
            options_json: row.try_get("options_json")?,
            form_json: row.try_get("form_json")?,
            default_option: row.try_get("default_option")?,
            risk: row.try_get("risk")?,
            allow_persistent: row.try_get("allow_persistent")?,
            expires_at_ms: row.try_get("expires_at_ms")?,
            status: row.try_get("status")?,
            chosen_option: row.try_get("chosen_option")?,
            resolved_input_json: row.try_get("resolved_input_json")?,
            resolved_by: row.try_get("resolved_by")?,
            created_at_ms: row.try_get("created_at_ms")?,
            resolved_at_ms: row.try_get("resolved_at_ms")?,
        })
    }
}

impl PendingRequest {
    /// The proposed input, parsed. `{}` when the adapter sent none.
    pub fn detail(&self) -> serde_json::Value {
        serde_json::from_str(&self.detail_json).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub fn options(&self) -> serde_json::Value {
        serde_json::from_str(&self.options_json).unwrap_or_else(|_| serde_json::json!([]))
    }

    /// The declared form, when this request has one.
    ///
    /// A malformed stored form is treated as absent rather than as an error: the
    /// request is still answerable through its options, and refusing to render it at
    /// all would strand the agent over a display concern.
    pub fn form(&self) -> Option<sylph_protocol::Form> {
        serde_json::from_str(self.form_json.as_deref()?).ok()
    }

    pub fn is_open(&self) -> bool {
        self.status == "pending"
    }
}

/// A client's answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRequest {
    pub option: String,
    /// An edited version of what the agent proposed.
    ///
    /// This is what makes "allow, but change the command" work — a real terminal
    /// affordance that a yes/no dialog drops. Absent means "as proposed".
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}
