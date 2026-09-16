//! Operational identifiers and command receipts. Trace facts live in `crate::trace`.

use serde::{Deserialize, Serialize};

/// Why a semantic identifier cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemanticError {
    #[error("producer id must not be empty")]
    EmptyProducerId,
    #[error("command request id must not be empty")]
    EmptyCommandRequestId,
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
