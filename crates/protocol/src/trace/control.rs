//! Immutable server command inputs and operational facts exported into trace evidence.
//!
//! `docs/unified-trace-storage.md` § Operational facts use an outbox defines their
//! durability. These types carry no storage, dispatch, runtime, or clock behavior.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

use super::{AgentRef, CommandTarget, IngestionEpoch, ObservationRef};

/// A retry is permitted only where the adapter established native idempotency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryPolicy {
    AtMostOnce,
    VendorIdempotent { key: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Send,
    Steer,
    Queue,
    NativeSteer,
    NativeQueue,
    Stop,
    Approval,
    Configure,
}

/// The authorizer supplies this receipt after evaluating the actual target.
/// Dispatch takes a fresh receipt; an acceptance lease is not a permanent grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationReceipt {
    pub subject: String,
    pub decision_ref: String,
    pub lease_ref: Option<String>,
    pub evaluated_at_ms: i64,
}

/// Exact submitted bytes, including attachments in the caller's declared encoding.
/// Base64 preserves non-UTF-8 input and avoids JSON's per-byte integer expansion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubmittedRevision {
    pub revision: String,
    pub media_type: String,
    #[serde(with = "byte_string")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandAcceptance {
    pub command_id: String,
    pub target: CommandTarget,
    pub kind: CommandKind,
    pub input: SubmittedRevision,
    pub authorization: AuthorizationReceipt,
    pub ingestion_authorization_epoch: IngestionEpoch,
    pub retry_policy: RetryPolicy,
    pub idempotency_key: Option<String>,
    pub accepted_at_ms: i64,
}

/// An occurrence identity supplied by its producer makes a repeated response idempotent.
/// Receipt bytes are never parsed and rewritten here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryFact {
    ReceiverAccepted {
        #[serde(with = "byte_string")]
        receipt: Vec<u8>,
    },
    Rejected {
        #[serde(with = "byte_string")]
        receipt: Vec<u8>,
    },
    OutcomeUnknown {
        reason: String,
    },
    Consumed {
        revision: String,
        #[serde(with = "byte_string")]
        receipt: Vec<u8>,
    },
    Cancelled {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationalFact {
    Accepted {
        command: Box<CommandAcceptance>,
    },
    Dispatched {
        command_id: String,
        input_revision: String,
        attempt: u64,
        authorization: AuthorizationReceipt,
    },
    Delivery {
        command_id: String,
        input_revision: String,
        fact: DeliveryFact,
    },
    Revised {
        command_id: String,
        previous_revision: String,
        input: SubmittedRevision,
        authorization: AuthorizationReceipt,
    },
    IdentityBound {
        command_id: String,
        agent_ref: AgentRef,
        source_evidence: Vec<ObservationRef>,
    },
}

mod byte_string {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<u8>, D::Error> {
        STANDARD
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submitted_binary_revision_has_the_same_lossless_wire_representation() {
        let wire = r#"{"revision":"submitted-v1","mediaType":"application/octet-stream","bytes":"AP9BDQo="}"#;
        let revision: SubmittedRevision = serde_json::from_str(wire).unwrap();
        assert_eq!(revision.bytes, [0, 255, 65, 13, 10]);
        assert_eq!(serde_json::to_string(&revision).unwrap(), wire);
    }

    #[test]
    fn operational_delivery_fact_keeps_lexical_receipt_bytes() {
        let wire = r#"{"kind":"delivery","command_id":"command-a","input_revision":"submitted-v1","fact":{"kind":"consumed","revision":"submitted-v1","receipt":"e30NCg=="}}"#;
        let fact: OperationalFact = serde_json::from_str(wire).unwrap();
        let OperationalFact::Delivery {
            command_id,
            input_revision: _,
            fact: DeliveryFact::Consumed { revision, receipt },
        } = &fact
        else {
            panic!("expected the declared consumed occurrence");
        };
        assert_eq!(command_id, "command-a");
        assert_eq!(revision, "submitted-v1");
        assert_eq!(receipt, b"{}\r\n");
        assert_eq!(serde_json::to_string(&fact).unwrap(), wire);
    }
}
