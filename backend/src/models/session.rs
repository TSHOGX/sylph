//! A session row, keyed by the identity triple.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use sylph_protocol::{AgentName, EnvironmentId, Fidelity, NativeId, SessionId};

/// Compose a [`SessionId`] out of three columns of one row.
///
/// # Why a decode error rather than a lossy fallback
///
/// [`EnvironmentId::parse`] and [`AgentName::parse`] can both fail, and a failure
/// here means the journal holds a row whose key is not a well-formed identity. That
/// is a corrupt store, so it surfaces as a `sqlx` decode error naming the column —
/// loudly, at the one moment someone is looking.
///
/// The alternative was tempting and wrong: substituting a placeholder id would make
/// the row readable and make every comparison against it silently false, which is
/// precisely the failure mode the triple exists to delete (`docs/architecture.md`
/// § Identity). A row that cannot be identified must not be handed out as if it could.
pub(crate) fn triple_from_row(
    row: &sqlx::sqlite::SqliteRow,
    environment: &str,
    agent: &str,
    native: &str,
) -> Result<SessionId, sqlx::Error> {
    let decode = |column: &str, error: sylph_protocol::IdentityError| sqlx::Error::ColumnDecode {
        index: column.to_string(),
        source: Box::new(error),
    };

    Ok(SessionId::new(
        EnvironmentId::parse(row.try_get::<String, _>(environment)?)
            .map_err(|error| decode(environment, error))?,
        AgentName::parse(row.try_get::<String, _>(agent)?).map_err(|error| decode(agent, error))?,
        NativeId::new(row.try_get::<String, _>(native)?),
    ))
}

/// Read `sessions.fidelity` back into the typed enum.
///
/// The column carries a CHECK constraint, so an unrecognized value means the schema
/// and this code disagree — a decode error rather than a default, for the same reason
/// as the triple above.
fn fidelity_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Fidelity, sqlx::Error> {
    let raw: String = row.try_get("fidelity")?;
    match raw.as_str() {
        "stream" => Ok(Fidelity::Stream),
        "blocks" => Ok(Fidelity::Blocks),
        "messages" => Ok(Fidelity::Messages),
        other => Err(sqlx::Error::ColumnDecode {
            index: "fidelity".to_string(),
            source: format!("'{other}' is not a fidelity this build knows").into(),
        }),
    }
}

/// One session.
///
/// **There is no Sylph-minted id** (`docs/architecture.md` § Identity): [`Self::id`]
/// *is* the identity, and a row exists only once an agent has assigned one. Nothing here
/// records who started the session — there is one class of session — so the only
/// behavioural flag is [`Self::inject_identity`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The identity triple. Serialized three ways at once, which is deliberate: a
    /// client echoes `id` back as a path segment and reads `environment`, `agent`,
    /// and `nativeId` as fields, **so it never parses the id** to learn which agent
    /// a session belongs to (`docs/api/README.md` § A session id in a path).
    #[serde(flatten, with = "session_id_fields")]
    pub id: SessionId,
    pub workdir: String,
    pub title: Option<String>,
    /// Whether `SOUL.md` is appended. Settable at creation and refused by `PATCH`,
    /// because a transcript records what produced it.
    pub inject_identity: bool,
    /// How much resolution this record has. A renderer reads it before drawing a
    /// duration.
    pub fidelity: Fidelity,
    /// A JSON *string*, not an object. Requests take `flags` as an object; a response
    /// reports the stored column and the client parses it — this document showed
    /// `flags` here for a release and a client written from it read `undefined`.
    pub flags_json: String,
    pub status: String,
    /// The delegation parent or fork origin, as an encoded triple.
    ///
    /// One column for two relationships, which the row it points at disambiguates
    /// (`crate::db::labels::lineage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Serialize a [`SessionId`] as the four fields a client reads.
///
/// `id` is the encoded form for a path segment; the three parts are given
/// separately so nothing downstream has to split the string.
mod session_id_fields {
    use super::*;
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(id: &SessionId, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("id", &id.encode())?;
        map.serialize_entry("environment", id.environment.as_str())?;
        map.serialize_entry("agent", id.agent.as_str())?;
        map.serialize_entry("nativeId", id.native_id.as_str())?;
        map.end()
    }

    /// Read back from the encoded `id` alone, since that is the one field guaranteed
    /// to round-trip and the three parts are derivable from it.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SessionId, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            id: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        SessionId::decode(&wire.id).map_err(serde::de::Error::custom)
    }
}

impl FromRow<'_, sqlx::sqlite::SqliteRow> for Session {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: triple_from_row(row, "environment", "agent", "native_id")?,
            workdir: row.try_get("workdir")?,
            title: row.try_get("title")?,
            inject_identity: row.try_get("inject_identity")?,
            fidelity: fidelity_from_row(row)?,
            flags_json: row.try_get("flags_json")?,
            status: row.try_get("status")?,
            parent_session: row.try_get("parent_session")?,
            created_at_ms: row.try_get("created_at_ms")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
        })
    }
}

impl Session {
    pub fn flags(&self) -> serde_json::Value {
        serde_json::from_str(&self.flags_json).unwrap_or(serde_json::json!({}))
    }
}

/// What a client asks for when starting a session.
///
/// **This starts an agent** (`docs/api/sessions.md` § `POST /api/v1/sessions`). There is
/// no id to supply, because a session has no identity until an agent has given it
/// one — which is why `nativeId` is absent here and present on the adopt request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    /// Which environment runs it. Required: an agent's availability is decided per
    /// machine by what is on that machine's `PATH`, so there is no global default
    /// that would be true anywhere.
    pub environment: String,
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    /// Whether to append `SOUL.md`. Defaults to **true** for a session Sylph starts.
    ///
    /// Accepted here and nowhere else: `PATCH` refuses it, because a transcript records
    /// what produced it and rewriting the flag would rewrite what the stored history
    /// means. **A default, not a policy.**
    #[serde(default = "inject_identity_default")]
    pub inject_identity: bool,
    pub flags: Option<serde_json::Value>,
}

fn inject_identity_default() -> bool {
    true
}

/// What a client asks for when adopting a session an agent already has.
///
/// Replaces `POST /sessions/import`, and the difference is where the id comes from:
/// an import named a row in a separate archive, while this names a session in the
/// **vendor's own native store** on a specific machine.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptSessionRequest {
    pub environment: String,
    pub agent: String,
    pub native_id: String,
    /// Follow the store as it grows, so a session running in a terminal right now
    /// appears and keeps updating. `false` parses one pass of what is recorded.
    #[serde(default)]
    pub watch: bool,
    /// Overrides the title derived from the opening ask.
    pub title: Option<String>,
    /// Defaults to **false** here, because the transcript so far was produced without
    /// it — but settable, since adopting a terminal session and injecting from that
    /// point on is a coherent thing to want.
    #[serde(default)]
    pub inject_identity: bool,
}
