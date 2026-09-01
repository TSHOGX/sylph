//! Session identity, and how much resolution a record has.
//!
//! `docs/architecture.md` § Identity is `(environment, agent, native_id)` is
//! authoritative; `docs/api-design.md` § A session id in a path is the identity
//! triple owns the encoding.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The delimiter between the three parts of an encoded session id.
///
/// **Not `/`, and this is measured rather than stylistic.** Most of `pi`'s model ids
/// contain a slash (`anthropic/claude-opus-5`, `stealth/ox-alpha`), so splitting on the
/// first or last one mis-parses the majority of a real catalogue. A native id is a
/// vendor's value and is under the same suspicion.
pub const ID_DELIMITER: char = ':';

/// A vendor's own id for a session, unchanged and unwrapped.
///
/// A newtype rather than a `String` alias so it cannot be passed where an
/// environment id or an encoded triple is expected. The historical failure this
/// prevents is worth naming: two id spaces for one conversation compare as
/// silently always false, with no error at any layer — `AGENTS.md` records the
/// subsystem that shipped fully written and tested and pruned nothing.
///
/// **Never synthesized.** A vendor either dictates an id it was handed or reports
/// one it assigned; an adapter that invents one has written a lie the journal makes
/// permanent (`docs/session-protocol.md` § Failure).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NativeId(String);

impl NativeId {
    /// Wrap what a vendor reported.
    ///
    /// Deliberately infallible and unvalidated. A native id is the vendor's, and
    /// refusing one for containing a character Sylph finds inconvenient would make
    /// a real session unrepresentable. Containment lives in the *encoding*
    /// ([`SessionId::encode`]), which is the only place the character matters.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NativeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An environment's opaque id, minted at pairing.
///
/// **Never a hostname and never a user-supplied name** (`docs/environments.md`
/// § Pairing): this value is part of a session's primary key, so a rename would
/// destroy identity. Display names are mutable metadata held elsewhere.
///
/// Restricted to base32 (`a-z`, `2-7`) so it can never contain the delimiter, a
/// `/`, or anything a URL path segment would have to escape. That restriction is
/// what makes [`SessionId::decode`] able to split on the *first* delimiter without
/// ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentId(String);

/// Why a string is not a well-formed identity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("an environment id must be non-empty base32 (a-z, 2-7); got '{0}'")]
    EnvironmentNotBase32(String),
    #[error("an agent name must be non-empty and free of '{ID_DELIMITER}'; got '{0}'")]
    AgentName(String),
    #[error("a native id must not be empty")]
    EmptyNativeId,
    #[error("'{0}' is not an encoded session id (<environment>{ID_DELIMITER}<agent>{ID_DELIMITER}<native_id>)")]
    Malformed(String),
}

impl EnvironmentId {
    pub fn parse(id: impl Into<String>) -> Result<Self, IdentityError> {
        let id = id.into();
        let base32 = !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte));
        if base32 {
            Ok(Self(id))
        } else {
            Err(IdentityError::EnvironmentNotBase32(id))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which vendor CLI a session belongs to.
///
/// Not an enum over the three supported agents, even though there are exactly
/// three (`AGENTS.md` § Product Invariants). Two reasons, and the second is the
/// load-bearing one: this value is a *stored primary-key component*, so a journal
/// written under a build that knew four agents must stay readable under one that
/// knows three, and an enum makes that a parse failure on a row nobody can fix.
/// And an agent's availability is decided per environment by what is on its
/// `PATH`, which no compile-time list can know.
///
/// What is validated is only what the encoding needs: non-empty and free of the
/// delimiter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentName(String);

impl AgentName {
    pub fn parse(name: impl Into<String>) -> Result<Self, IdentityError> {
        let name = name.into();
        if name.is_empty() || name.contains(ID_DELIMITER) {
            return Err(IdentityError::AgentName(name));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A session's identity: the triple, and nothing else.
///
/// There is no Sylph-minted UUID (`docs/architecture.md` § Identity). The native
/// id is available for every supported agent — by dictation for `claude` and `pi`,
/// reported on assignment for `codex` — so a second id space would be pure
/// redundancy plus a permanent invitation to compare across the two.
///
/// **The encoded form is for a wire and a URL path segment, not for storage.** The
/// journal keys on three columns (`docs/database-schema.md`), because every listing
/// filters on `environment` and groups by `workdir` within it. [`Self::encode`] and
/// [`Self::decode`] exist so no caller ever hand-assembles the string, which is how
/// one producer's spelling drifts from another's reader.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionId {
    pub environment: EnvironmentId,
    pub agent: AgentName,
    pub native_id: NativeId,
}

impl SessionId {
    pub fn new(environment: EnvironmentId, agent: AgentName, native_id: NativeId) -> Self {
        Self {
            environment,
            agent,
            native_id,
        }
    }

    /// The wire form, `<environment>:<agent>:<native_id>`.
    ///
    /// A method rather than a `format!` at each call site. The whole point is that
    /// there is one producer and one reader, so an encoding change is a change to
    /// two functions instead of a search for every place a client was assembling
    /// the string itself.
    ///
    /// Not percent-encoded here: that is a property of carrying it in a URL path,
    /// and doing it inside the encoder would produce a value that double-encodes
    /// the moment a caller does it properly.
    pub fn encode(&self) -> String {
        format!(
            "{}{ID_DELIMITER}{}{ID_DELIMITER}{}",
            self.environment, self.agent, self.native_id
        )
    }

    /// Read back what [`Self::encode`] produced.
    ///
    /// Splits on the *first two* delimiters and takes everything after as the
    /// native id, because only the first two fields are constrained not to contain
    /// one. A vendor id containing a `:` therefore round-trips; splitting from the
    /// right, or on every delimiter, would truncate it.
    pub fn decode(encoded: &str) -> Result<Self, IdentityError> {
        let mut parts = encoded.splitn(3, ID_DELIMITER);
        let (Some(environment), Some(agent), Some(native)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(IdentityError::Malformed(encoded.to_string()));
        };
        if native.is_empty() {
            return Err(IdentityError::EmptyNativeId);
        }
        Ok(Self {
            environment: EnvironmentId::parse(environment)?,
            agent: AgentName::parse(agent)?,
            native_id: NativeId::new(native),
        })
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl FromStr for SessionId {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::decode(value)
    }
}

/// How much resolution the record of a session has.
///
/// **A property of the record, not of who started the session**
/// (`docs/architecture.md` § What genuinely differs). A session Sylph launched, read
/// back from the vendor's native store after the process died, is `Messages` too —
/// so this cannot be inferred from anything about the session's origin, and there is
/// no origin field to infer it from.
///
/// A renderer reads this *before drawing a duration*. A waterfall over
/// `Messages`-fidelity data presents equal-length bars as measurements, and no care
/// downstream recovers a timing that was never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// Every delta and its arrival time is recorded. Only `Adapter::run` produces
    /// this, because only the process's own stdio carries it.
    Stream,
    /// The vendor's store flushed at block boundaries — a tool call, a content
    /// block. Durations within a block are not recoverable.
    Blocks,
    /// The vendor's store flushed per message. The coarsest of the three, and what
    /// most native stores actually hold.
    Messages,
}

impl fmt::Display for Fidelity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stream => "stream",
            Self::Blocks => "blocks",
            Self::Messages => "messages",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple(native: &str) -> SessionId {
        SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("claude").unwrap(),
            NativeId::new(native),
        )
    }

    #[test]
    fn an_encoded_id_round_trips_through_decode() {
        let id = triple("9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60");
        assert_eq!(
            id.encode(),
            "env7fk2:claude:9bdacb6e-1c4d-4f2a-8e11-2a7c3d4e5f60"
        );
        assert_eq!(SessionId::decode(&id.encode()).unwrap(), id);
    }

    /// The native id is the vendor's, so a delimiter inside it is the vendor's
    /// business. Splitting from the right, or on every delimiter, would truncate
    /// it — which is the same class of defect as splitting a pi model id on `/`.
    #[test]
    fn a_native_id_containing_the_delimiter_survives_the_round_trip() {
        let id = triple("thread:42:b");
        assert_eq!(SessionId::decode(&id.encode()).unwrap(), id);
        assert_eq!(
            SessionId::decode(&id.encode()).unwrap().native_id.as_str(),
            "thread:42:b"
        );
    }

    /// `/` is what this encoding exists to avoid. It has no special meaning here,
    /// which is the point: a slash in a native id is data, and nothing splits on it.
    #[test]
    fn a_slash_in_a_native_id_is_data_and_not_a_separator() {
        let id = triple("worktrees/agent-a505/main");
        let decoded = SessionId::decode(&id.encode()).unwrap();
        assert_eq!(decoded.native_id.as_str(), "worktrees/agent-a505/main");
    }

    #[test]
    fn an_environment_id_must_be_base32() {
        assert!(EnvironmentId::parse("env7fk2").is_ok());
        // Uppercase, digits outside base32, and a delimiter each break the promise
        // that lets `decode` split on the first two separators unambiguously.
        assert!(EnvironmentId::parse("Env7FK2").is_err());
        assert!(EnvironmentId::parse("env0189").is_err());
        assert!(EnvironmentId::parse("env:7").is_err());
        assert!(EnvironmentId::parse("").is_err());
        // A hostname is the thing this type exists to refuse.
        assert!(EnvironmentId::parse("hsw-macbook.local").is_err());
    }

    #[test]
    fn an_agent_name_may_not_contain_the_delimiter() {
        assert!(AgentName::parse("codex").is_ok());
        assert!(AgentName::parse("co:dex").is_err());
        assert!(AgentName::parse("").is_err());
    }

    #[test]
    fn a_malformed_encoded_id_is_refused_rather_than_guessed_at() {
        assert!(SessionId::decode("env7fk2:claude").is_err());
        assert!(SessionId::decode("env7fk2:claude:").is_err());
        assert!(SessionId::decode("").is_err());
    }
}
