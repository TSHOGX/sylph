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
    #[serde(rename = "nativeSessionId", skip_serializing_if = "Option::is_none")]
    pub backend_session_id: Option<String>,
    /// TraceDB id this session was imported from, when it was.
    ///
    /// Serialized because the archive browser needs it: without it the client
    /// cannot tell that an archived row it is about to list is the same
    /// conversation as a live session, and would offer the user two ways into one
    /// history. The Web UUID remains the route id; this field is the durable
    /// archive identity used by the originating agent.
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
    /// Whether this session gets Sylph's identity. `sylph` injects `SOUL.md`,
    /// `preserve` runs the agent as it comes. Defaults to `sylph`, so an older
    /// client that does not send the field keeps the behaviour it had.
    ///
    /// This is the *creating* client's choice, and it is only a choice for a
    /// Sylph-origin session. An imported native session is `preserve` because
    /// its identity is already fixed by the history it resumes — see
    /// [`crate::api::sessions::import`], which derives the field rather than
    /// reading it.
    ///
    /// It is accepted here and nowhere else. Identity decides what the stored
    /// transcript was produced by, so it is fixed at creation: `PATCH /sessions`
    /// deliberately refuses it, and converting an existing session is a separate
    /// explicit operation.
    pub identity_mode: Option<IdentityMode>,
}

/// Identity policy for a session.
///
/// A closed enum rather than a string, so an unrecognized value is a 422 from
/// serde with the field named, instead of a row in the journal carrying a
/// spelling nothing downstream matches — which would silently read as "not
/// sylph" and disable injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum IdentityMode {
    /// Inject `SOUL.md`: this is a Sylph session.
    #[default]
    Sylph,
    /// Run the agent with its own identity and no injection.
    Preserve,
}

impl IdentityMode {
    /// Journal spelling. These strings are already in the schema and in
    /// `docs/database-schema.md`, so they are pinned here rather than derived
    /// from the variant names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sylph => "sylph",
            Self::Preserve => "preserve",
        }
    }
}
