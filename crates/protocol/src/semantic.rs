//! Semantic values shared by the server and an envagent.
//!
//! This module names facts that must survive reconnects: producer/server sequence axes,
//! source cursors, session graph edges, adopted user input, and command outcomes. Transport
//! framing is intentionally left to the `sylph.env.v1` layer.

use crate::{EnvironmentId, Event, SessionId};
use serde::{Deserialize, Serialize};

/// Why a turn ended before its normal vendor outcome. This is independent of
/// the terminal status and is persisted on the turn record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Interrupted,
    SpoolFull,
    GrantRevoked,
    LinkLost,
    AgentExited,
    AdapterSilent,
}

impl CloseReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::SpoolFull => "spool_full",
            Self::GrantRevoked => "grant_revoked",
            Self::LinkLost => "link_lost",
            Self::AgentExited => "agent_exited",
            Self::AdapterSilent => "adapter_silent",
        }
    }
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a semantic identifier cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemanticError {
    #[error("producer id must not be empty")]
    EmptyProducerId,
    #[error("command request id must not be empty")]
    EmptyCommandRequestId,
    #[error("source name must not be empty")]
    EmptySource,
    #[error("a session edge cannot point a session at itself")]
    SelfEdge,
}

/// Opaque identity for one event producer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProducerId(String);

impl ProducerId {
    pub fn parse(id: impl Into<String>) -> Result<Self, SemanticError> {
        let id = id.into();
        if id.trim().is_empty() {
            Err(SemanticError::EmptyProducerId)
        } else {
            Ok(Self(id))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProducerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Sequence allocated by an envagent producer and persisted before send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProducerSeq(u64);

impl ProducerSeq {
    pub const ZERO: Self = Self(0);
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
impl From<u64> for ProducerSeq {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}
impl std::fmt::Display for ProducerSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Canonical sequence allocated transactionally by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerSeq(u64);

impl ServerSeq {
    pub const ZERO: Self = Self(0);
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
impl From<u64> for ServerSeq {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}
impl std::fmt::Display for ServerSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Source-local cursor that survives polling and reconnects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCursor {
    pub source: String,
    pub generation: u64,
    pub offset: u64,
}
impl SourceCursor {
    pub fn new(
        source: impl Into<String>,
        generation: u64,
        offset: u64,
    ) -> Result<Self, SemanticError> {
        let source = source.into();
        if source.trim().is_empty() {
            return Err(SemanticError::EmptySource);
        }
        Ok(Self {
            source,
            generation,
            offset,
        })
    }
}

/// Event with producer provenance and optional server assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedEvent {
    pub producer_id: ProducerId,
    pub producer_seq: ProducerSeq,
    /// Session that owns the event. This is identity, not arrival-turn
    /// provenance; one producer may multiplex several sessions.
    pub session_id: SessionId,
    /// Optional turn on which the server received the item. It is provenance,
    /// not event ownership, and may be absent for a native source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_seq: Option<ServerSeq>,
    pub event: Event,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<SourceCursor>,
}
impl SequencedEvent {
    pub fn pending(
        producer_id: ProducerId,
        producer_seq: ProducerSeq,
        session_id: SessionId,
        event: Event,
        source_cursor: Option<SourceCursor>,
    ) -> Self {
        Self {
            producer_id,
            producer_seq,
            session_id,
            arrival_turn_id: None,
            server_seq: None,
            event,
            source_cursor,
        }
    }
    pub fn with_server_seq(mut self, server_seq: ServerSeq) -> Self {
        self.server_seq = Some(server_seq);
        self
    }
}

/// Why a child session is related to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEdgeRelation {
    Delegation,
    Fork,
}
impl SessionEdgeRelation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delegation => "delegation",
            Self::Fork => "fork",
        }
    }
}
impl std::fmt::Display for SessionEdgeRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Server-owned edge in the session graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEdge {
    pub parent: SessionId,
    pub child: SessionId,
    pub relation: SessionEdgeRelation,
}
impl SessionEdge {
    pub fn new(
        parent: SessionId,
        child: SessionId,
        relation: SessionEdgeRelation,
    ) -> Result<Self, SemanticError> {
        if parent == child {
            return Err(SemanticError::SelfEdge);
        }
        Ok(Self {
            parent,
            child,
            relation,
        })
    }
}

/// User request recovered from a native store.
///
/// This is a container fact, not an [`Event`] and not a new event kind. Live turns continue
/// to receive input from the server request path; adopted/watch sources emit this fact so the
/// server can populate `turns.request_text` without fabricating timeline history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInputFact {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<SourceCursor>,
}
impl TurnInputFact {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            recorded_at_ms: None,
            source_cursor: None,
        }
    }
}

/// Opaque id for one server-to-envagent command.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandRequestId(String);
impl CommandRequestId {
    pub fn parse(id: impl Into<String>) -> Result<Self, SemanticError> {
        let id = id.into();
        if id.trim().is_empty() {
            Err(SemanticError::EmptyCommandRequestId)
        } else {
            Ok(Self(id))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for CommandRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a command may be retried after interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyPolicy {
    Replayable,
    AtMostOnce,
}
impl IdempotencyPolicy {
    pub const fn retry_allowed(self) -> bool {
        matches!(self, Self::Replayable)
    }
}

/// Outcome of one command attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    Accepted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
    },
    Rejected {
        code: String,
        message: String,
    },
    Unknown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}
impl CommandOutcome {
    pub const fn accepted() -> Self {
        Self::Accepted { result: None }
    }

    pub fn accepted_with_result(result: serde_json::Value) -> Self {
        Self::Accepted {
            result: Some(result),
        }
    }

    /// A refusal naming one of the codes the server branches on.
    ///
    /// The only constructor an envagent should use for a rejection: a code spelled by
    /// hand at the call site is a code the server may not know, and an unknown code is
    /// reported as a link failure rather than as the refusal it was.
    pub fn rejected(code: crate::env::RejectCode, message: impl Into<String>) -> Self {
        Self::Rejected {
            code: code.as_str().to_string(),
            message: message.into(),
        }
    }

    /// The typed code behind a rejection, when this build knows it.
    pub fn reject_code(&self) -> Option<crate::env::RejectCode> {
        match self {
            Self::Rejected { code, .. } => crate::env::RejectCode::parse(code),
            _ => None,
        }
    }
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Unknown { .. })
    }

    pub fn result(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Accepted { result } => result.as_ref(),
            Self::Rejected { .. } | Self::Unknown { .. } => None,
        }
    }
}

/// Identity and retry policy sent with every server command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRequest {
    pub request_id: CommandRequestId,
    pub idempotency: IdempotencyPolicy,
}

/// Command result carrying request identity back to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub request_id: CommandRequestId,
    pub outcome: CommandOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<IdempotencyPolicy>,
}

/// Typed deduplication key components; intentionally not a concatenated string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProducerKey<'a> {
    pub environment: &'a EnvironmentId,
    pub producer_id: &'a ProducerId,
    pub producer_seq: ProducerSeq,
}
impl<'a> ProducerKey<'a> {
    pub const fn new(
        environment: &'a EnvironmentId,
        producer_id: &'a ProducerId,
        producer_seq: ProducerSeq,
    ) -> Self {
        Self {
            environment,
            producer_id,
            producer_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentName, EventKind, NativeId, Phase, Stability};

    fn session(native: &str) -> SessionId {
        SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("claude").unwrap(),
            NativeId::new(native),
        )
    }
    fn event() -> Event {
        Event {
            kind: EventKind::Assistant,
            phase: Some(Phase::Completed),
            stream_id: Some("block-1".into()),
            span: None,
            interior_state: None,
            name: None,
            text: "hello".into(),
            data: None,
            is_error: false,
            stability: Stability::Stable,
            recorded_at_ms: Some(1_700_000_000_000),
            created_at_ms: 1_700_000_000_000,
            started_at_ms: None,
        }
    }
    #[test]
    fn producer_and_server_sequences_are_distinct_axes() {
        let producer = ProducerId::parse("agent-a").unwrap();
        let cursor = SourceCursor::new("rollout.jsonl", 3, 42).unwrap();
        let pending = SequencedEvent::pending(
            producer.clone(),
            ProducerSeq::new(7),
            session("s1"),
            event(),
            Some(cursor.clone()),
        );
        assert_eq!(pending.producer_seq, ProducerSeq::new(7));
        assert_eq!(pending.server_seq, None);
        let committed = pending.with_server_seq(ServerSeq::new(19));
        assert_eq!(committed.producer_id, producer);
        assert_eq!(committed.server_seq, Some(ServerSeq::new(19)));
        assert_eq!(committed.source_cursor, Some(cursor));
        let wire = serde_json::to_value(&committed).unwrap();
        assert_eq!(wire["producerSeq"], 7);
        assert_eq!(wire["serverSeq"], 19);
        assert_eq!(wire["sourceCursor"]["generation"], 3);
    }
    #[test]
    fn identifiers_reject_empty_values() {
        assert_eq!(
            ProducerId::parse("   ").unwrap_err(),
            SemanticError::EmptyProducerId
        );
        assert_eq!(
            CommandRequestId::parse("").unwrap_err(),
            SemanticError::EmptyCommandRequestId
        );
        assert_eq!(
            SourceCursor::new("", 0, 0).unwrap_err(),
            SemanticError::EmptySource
        );
    }
    #[test]
    fn session_edges_name_relation_and_refuse_self_edges() {
        let parent = session("parent");
        let child = session("child");
        let edge = SessionEdge::new(
            parent.clone(),
            child.clone(),
            SessionEdgeRelation::Delegation,
        )
        .unwrap();
        assert_eq!(edge.parent, parent);
        assert_eq!(edge.child, child);
        assert_eq!(edge.relation.to_string(), "delegation");
        assert_eq!(
            serde_json::to_value(&edge).unwrap()["relation"],
            "delegation"
        );
        assert_eq!(
            SessionEdge::new(session("same"), session("same"), SessionEdgeRelation::Fork)
                .unwrap_err(),
            SemanticError::SelfEdge
        );
    }
    #[test]
    fn adopted_turn_input_is_a_fact_not_an_event() {
        let mut fact = TurnInputFact::new("fix the login bug");
        fact.recorded_at_ms = Some(1_700_000_001_000);
        fact.source_cursor = Some(SourceCursor::new("session.jsonl", 1, 128).unwrap());
        let wire = serde_json::to_value(&fact).unwrap();
        assert_eq!(wire["text"], "fix the login bug");
        assert_eq!(wire["recordedAtMs"], 1_700_000_001_000i64);
        assert!(wire.get("kind").is_none());
    }
    #[test]
    fn command_policy_and_unknown_outcome_are_explicit() {
        let replayable = CommandRequest {
            request_id: CommandRequestId::parse("cmd-1").unwrap(),
            idempotency: IdempotencyPolicy::Replayable,
        };
        assert!(replayable.idempotency.retry_allowed());
        let at_most_once = CommandRequest {
            request_id: CommandRequestId::parse("cmd-2").unwrap(),
            idempotency: IdempotencyPolicy::AtMostOnce,
        };
        assert!(!at_most_once.idempotency.retry_allowed());
        let result = CommandResult {
            request_id: at_most_once.request_id.clone(),
            outcome: CommandOutcome::Unknown {
                message: Some("connection dropped after send".into()),
            },
            idempotency: Some(IdempotencyPolicy::AtMostOnce),
        };
        assert!(!result.outcome.is_terminal());
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["requestId"], "cmd-2");
        assert_eq!(wire["outcome"]["status"], "unknown");
        assert_eq!(
            serde_json::from_value::<CommandResult>(wire).unwrap(),
            result
        );

        let accepted = CommandOutcome::accepted_with_result(serde_json::json!({"accepted": true}));
        assert!(accepted.is_terminal());
        assert_eq!(
            accepted.result(),
            Some(&serde_json::json!({"accepted": true}))
        );
    }
}

/// A session-scoped, non-reusable turn generation. The envagent opens this lease
/// on a successful send and closes it on adapter settlement or stream loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnLease {
    pub turn_id: String,
}

/// A vendor acknowledgement states its delivery scope; queue insertion never
/// proves that text belongs to the active turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "delivery", rename_all = "snake_case")]
pub enum ControlReceipt {
    ActiveTurn { lease: TurnLease },
    NativeQueue,
}
