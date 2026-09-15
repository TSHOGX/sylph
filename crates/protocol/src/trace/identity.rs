//! Typed trace addresses and the independent receipt/source occurrence identities.
//!
//! `docs/unified-trace-foundation.md` § Identity and reference resolution owns these
//! meanings. A digest addresses bytes; it never identifies a message occurrence.

use std::{fmt, marker::PhantomData, str::FromStr};

use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};

use crate::{EnvironmentId, SessionId};

/// An invalid trace address or capture identity is rejected before acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TraceIdentityError {
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("invalid session identity: {0}")]
    Session(String),
    #[error("invalid environment identity: {0}")]
    Environment(String),
    #[error("producer sequence must be positive")]
    ZeroSequence,
    #[error("a source byte interval must have end greater than start")]
    ByteInterval,
    #[error("malformed opaque trace reference")]
    MalformedEncoding,
    #[error("expected reference kind {expected}, received {actual}")]
    Kind { expected: String, actual: String },
}

pub(crate) fn nonempty(value: &str, field: &'static str) -> Result<(), TraceIdentityError> {
    if value.is_empty() {
        Err(TraceIdentityError::Empty(field))
    } else {
        Ok(())
    }
}

fn validate_session(session: &SessionId) -> Result<(), TraceIdentityError> {
    let decoded = SessionId::decode(&session.encode())
        .map_err(|error| TraceIdentityError::Session(error.to_string()))?;
    if &decoded != session {
        return Err(TraceIdentityError::Session(session.encode()));
    }
    Ok(())
}

/// An actual independently advancing conversation, whether publicly resumable or embedded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentRef {
    Session(SessionId),
    Embedded {
        anchor_session: SessionId,
        namespace: String,
        native_child_id: String,
    },
}

impl AgentRef {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        validate_session(self.anchor_session())?;
        if let Self::Embedded {
            namespace,
            native_child_id,
            ..
        } = self
        {
            nonempty(namespace, "embedded namespace")?;
            nonempty(native_child_id, "native child identity")?;
        }
        Ok(())
    }

    /// The containing session is a read/authorization anchor, never an implicit control route.
    pub fn anchor_session(&self) -> &SessionId {
        match self {
            Self::Session(session) => session,
            Self::Embedded { anchor_session, .. } => anchor_session,
        }
    }

    pub fn environment(&self) -> &EnvironmentId {
        &self.anchor_session().environment
    }

    pub fn encode(&self) -> String {
        encode_reference("agent", self)
    }

    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("agent", value)
    }
}

impl<'de> Deserialize<'de> for AgentRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "value",
            rename_all = "snake_case",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum Wire {
            Session(SessionId),
            Embedded {
                anchor_session: SessionId,
                namespace: String,
                native_child_id: String,
            },
        }
        let value = match Wire::deserialize(deserializer)? {
            Wire::Session(session) => Self::Session(session),
            Wire::Embedded {
                anchor_session,
                namespace,
                native_child_id,
            } => Self::Embedded {
                anchor_session,
                namespace,
                native_child_id,
            },
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Source and producer registries are namespaced by their durable issuer identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Issuer {
    Environment(EnvironmentId),
    Server(String),
}

impl Issuer {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        match self {
            Self::Environment(environment) => EnvironmentId::parse(environment.as_str())
                .map(|_| ())
                .map_err(|error| TraceIdentityError::Environment(error.to_string())),
            Self::Server(identity) => nonempty(identity, "server identity"),
        }
    }
}

impl<'de> Deserialize<'de> for Issuer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            content = "value",
            rename_all = "snake_case",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum Wire {
            Environment(EnvironmentId),
            Server(String),
        }
        let value = match Wire::deserialize(deserializer)? {
            Wire::Environment(id) => Self::Environment(id),
            Wire::Server(id) => Self::Server(id),
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// One producer delivery. Redelivery reuses all four fields, including execution epoch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationRef {
    pub issuer: Issuer,
    pub producer_id: String,
    pub producer_epoch: String,
    pub producer_seq: u64,
}

impl ObservationRef {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        self.issuer.validate()?;
        nonempty(&self.producer_id, "producer identity")?;
        nonempty(&self.producer_epoch, "producer epoch")?;
        if self.producer_seq == 0 {
            return Err(TraceIdentityError::ZeroSequence);
        }
        Ok(())
    }

    pub fn encode(&self) -> String {
        encode_reference("observation", self)
    }

    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("observation", value)
    }
}

impl<'de> Deserialize<'de> for ObservationRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            issuer: Issuer,
            producer_id: String,
            producer_epoch: String,
            producer_seq: u64,
        }
        let Wire {
            issuer,
            producer_id,
            producer_epoch,
            producer_seq,
        } = Wire::deserialize(deserializer)?;
        let value = Self {
            issuer,
            producer_id,
            producer_epoch,
            producer_seq,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// A durable source registry entry. A path is provenance and belongs outside this key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub issuer: Issuer,
    pub instance: String,
}

impl SourceRef {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        self.issuer.validate()?;
        nonempty(&self.instance, "source instance")
    }

    pub fn encode(&self) -> String {
        encode_reference("source", self)
    }

    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("source", value)
    }
}

impl<'de> Deserialize<'de> for SourceRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            issuer: Issuer,
            instance: String,
        }
        let Wire { issuer, instance } = Wire::deserialize(deserializer)?;
        let value = Self { issuer, instance };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// Physical source position. Vendor history ordinals are separately mapped evidence,
/// so later interpreting an ordinal cannot change the identity of captured bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SourceLocator {
    Jsonl {
        byte_start: u64,
        byte_end: u64,
        record_ordinal: u64,
    },
    Frame {
        ordinal: u64,
    },
    Snapshot {
        revision: String,
    },
    Occurrence {
        key: String,
    },
}

impl SourceLocator {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        match self {
            Self::Jsonl {
                byte_start,
                byte_end,
                ..
            } if byte_start >= byte_end => Err(TraceIdentityError::ByteInterval),
            Self::Snapshot { revision } => nonempty(revision, "snapshot occurrence"),
            Self::Occurrence { key } => nonempty(key, "source occurrence"),
            _ => Ok(()),
        }
    }
}

/// One immutable source occurrence, independent of transport receipt and retention epoch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePosition {
    pub source: SourceRef,
    pub generation: u64,
    pub locator: SourceLocator,
}

impl SourcePosition {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        self.source.validate()?;
        self.locator.validate()
    }

    pub fn encode(&self) -> String {
        encode_reference("source_position", self)
    }

    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("source_position", value)
    }
}

impl<'de> Deserialize<'de> for SourcePosition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            source: SourceRef,
            generation: u64,
            locator: SourceLocator,
        }
        let Wire {
            source,
            generation,
            locator,
        } = Wire::deserialize(deserializer)?;
        let value = Self {
            source,
            generation,
            locator,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

/// A key's scope is its reference namespace, not an ownership claim. An unresolved
/// owner remains unresolved until separately evidenced, even when the address is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ObjectAddress {
    Native {
        scope: AgentRef,
        namespace: String,
        key: Vec<String>,
    },
    Occurrence {
        scope: AgentRef,
        position: SourcePosition,
        local_slot: Vec<String>,
    },
}

impl ObjectAddress {
    pub fn scope(&self) -> &AgentRef {
        match self {
            Self::Native { scope, .. } | Self::Occurrence { scope, .. } => scope,
        }
    }

    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        self.scope().validate()?;
        match self {
            Self::Native { namespace, key, .. } => {
                nonempty(namespace, "vendor namespace")?;
                if key.is_empty() {
                    return Err(TraceIdentityError::Empty("native key tuple"));
                }
            }
            Self::Occurrence {
                position,
                local_slot,
                ..
            } => {
                position.validate()?;
                if local_slot.is_empty() {
                    return Err(TraceIdentityError::Empty("local slot tuple"));
                }
            }
        }
        Ok(())
    }
}

/// Each type marker supplies a distinct wire kind; keys cannot cross object kinds.
pub trait ObjectKind: Clone + fmt::Debug + Eq + Ord + std::hash::Hash {
    const KIND: &'static str;
}

macro_rules! object_kinds {
    ($($kind:ident => $alias:ident = $wire:literal),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $kind {}
        impl ObjectKind for $kind { const KIND: &'static str = $wire; }
        pub type $alias = ObjectRef<$kind>;
    )+};
}

object_kinds! {
    ActivationId => ActivationRef = "activation",
    MessageId => MessageRef = "message",
    MessageRevisionId => MessageRevisionRef = "message_revision",
    MessagePartId => MessagePartRef = "message_part",
    DeliveryId => DeliveryRef = "delivery",
    OperationId => OperationRef = "operation",
    ResultId => ResultRef = "result",
    ResourceId => ResourceRef = "resource",
    HistoryEntryId => EntryRef = "history_entry",
    HistorySelectionId => SelectionRef = "history_selection",
    ContextCheckpointId => CheckpointRef = "context_checkpoint",
    MeasurementId => MeasurementRef = "measurement",
    ClaimId => ClaimRef = "claim",
}

/// An opaque address preserves the type and every tuple boundary, including empty members.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef<K: ObjectKind> {
    // References are embedded in nearly every claim and relation. Keep the large
    // occurrence envelope behind one pointer so one optional reference does not
    // inflate every variant of an otherwise small lifecycle fact.
    address: Box<ObjectAddress>,
    kind: PhantomData<K>,
}

impl<K: ObjectKind> ObjectRef<K> {
    pub fn new(address: ObjectAddress) -> Result<Self, TraceIdentityError> {
        address.validate()?;
        Ok(Self {
            address: Box::new(address),
            kind: PhantomData,
        })
    }

    pub fn native(
        scope: AgentRef,
        namespace: impl Into<String>,
        key: Vec<String>,
    ) -> Result<Self, TraceIdentityError> {
        Self::new(ObjectAddress::Native {
            scope,
            namespace: namespace.into(),
            key,
        })
    }

    pub fn occurrence(
        scope: AgentRef,
        position: SourcePosition,
        local_slot: Vec<String>,
    ) -> Result<Self, TraceIdentityError> {
        Self::new(ObjectAddress::Occurrence {
            scope,
            position,
            local_slot,
        })
    }

    pub fn address(&self) -> &ObjectAddress {
        &self.address
    }

    pub fn scope(&self) -> &AgentRef {
        self.address.scope()
    }

    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        self.address.validate()
    }

    pub fn encode(&self) -> String {
        encode_reference(K::KIND, &self.address)
    }

    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        Self::new(decode_reference(K::KIND, value)?)
    }
}

impl<K: ObjectKind> Serialize for ObjectRef<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.encode())
    }
}

impl<'de, K: ObjectKind> Deserialize<'de> for ObjectRef<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::decode(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl<K: ObjectKind> fmt::Display for ObjectRef<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.encode())
    }
}

impl<K: ObjectKind> FromStr for ObjectRef<K> {
    type Err = TraceIdentityError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::decode(value)
    }
}

// Length prefixes prevent kind/payload boundaries from depending on a delimiter that
// an opaque vendor identifier might contain. JSON preserves native tuple member
// boundaries inside the payload; canonical round-trip rejects alternate URL spellings.
pub(super) fn encode_reference<T: Serialize>(kind: &str, value: &T) -> String {
    let payload =
        serde_json::to_vec(value).expect("trace identities contain only serializable fields");
    let mut bytes = Vec::with_capacity(16 + kind.len() + payload.len());
    bytes.extend_from_slice(&(kind.len() as u64).to_be_bytes());
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&payload);
    let mut encoded = String::from("tr_");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 15)]));
    }
    encoded
}

pub(super) fn decode_reference<T: DeserializeOwned + Serialize>(
    kind: &str,
    encoded: &str,
) -> Result<T, TraceIdentityError> {
    let raw = encoded
        .strip_prefix("tr_")
        .ok_or(TraceIdentityError::MalformedEncoding)?;
    if raw.len() % 2 != 0 {
        return Err(TraceIdentityError::MalformedEncoding);
    }
    let mut bytes = Vec::with_capacity(raw.len() / 2);
    for pair in raw.as_bytes().chunks_exact(2) {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(TraceIdentityError::MalformedEncoding),
        };
        bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
    }
    fn part<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], TraceIdentityError> {
        let length_bytes: [u8; 8] = bytes
            .get(..8)
            .ok_or(TraceIdentityError::MalformedEncoding)?
            .try_into()
            .unwrap();
        let length = usize::try_from(u64::from_be_bytes(length_bytes))
            .map_err(|_| TraceIdentityError::MalformedEncoding)?;
        let end = 8usize
            .checked_add(length)
            .ok_or(TraceIdentityError::MalformedEncoding)?;
        let value = bytes
            .get(8..end)
            .ok_or(TraceIdentityError::MalformedEncoding)?;
        *bytes = bytes
            .get(end..)
            .ok_or(TraceIdentityError::MalformedEncoding)?;
        Ok(value)
    }
    let mut remaining = bytes.as_slice();
    let actual = std::str::from_utf8(part(&mut remaining)?)
        .map_err(|_| TraceIdentityError::MalformedEncoding)?;
    if actual != kind {
        return Err(TraceIdentityError::Kind {
            expected: kind.into(),
            actual: actual.into(),
        });
    }
    let value: T = serde_json::from_slice(part(&mut remaining)?)
        .map_err(|_| TraceIdentityError::MalformedEncoding)?;
    if !remaining.is_empty() || encode_reference(kind, &value) != encoded {
        return Err(TraceIdentityError::MalformedEncoding);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentRef {
        AgentRef::Session(SessionId::decode("env7:claude:native:根").unwrap())
    }

    fn position() -> SourcePosition {
        SourcePosition {
            source: SourceRef {
                issuer: Issuer::Environment(EnvironmentId::parse("env7").unwrap()),
                instance: "root.jsonl".into(),
            },
            generation: 3,
            locator: SourceLocator::Jsonl {
                byte_start: 0,
                byte_end: 100,
                record_ordinal: 0,
            },
        }
    }

    #[test]
    fn opaque_native_keys_preserve_type_case_unicode_and_tuple_boundaries() {
        let left = MessageRef::native(agent(), "claude.record", vec!["a:b".into(), "c/根".into()])
            .unwrap();
        let right = MessageRef::native(agent(), "claude.record", vec!["a".into(), "b:c/根".into()])
            .unwrap();
        let case = MessageRef::native(agent(), "claude.record", vec!["A:b".into(), "c/根".into()])
            .unwrap();
        assert_ne!(left.encode(), right.encode());
        assert_ne!(left.encode(), case.encode());
        let other_namespace =
            MessageRef::native(agent(), "claude.other", vec!["a:b".into(), "c/根".into()]).unwrap();
        assert_ne!(left.encode(), other_namespace.encode());
        assert_eq!(
            MessageRef::decode(&left.encode()).unwrap().address(),
            &ObjectAddress::Native {
                scope: agent(),
                namespace: "claude.record".into(),
                key: vec!["a:b".into(), "c/根".into()],
            }
        );
        assert!(OperationRef::decode(&left.encode()).is_err());
        assert_eq!(
            serde_json::from_str::<MessageRef>(&serde_json::to_string(&left).unwrap()).unwrap(),
            left
        );
    }

    #[test]
    fn producer_redelivery_native_reread_and_repeated_content_have_separate_axes() {
        let observation = ObservationRef {
            issuer: Issuer::Server("server".into()),
            producer_id: "producer".into(),
            producer_epoch: "run-1".into(),
            producer_seq: 1,
        };
        let redelivery = observation.clone();
        let reread = ObservationRef {
            producer_seq: 2,
            ..observation.clone()
        };
        assert_eq!(observation, redelivery);
        assert_ne!(observation, reread);
        assert_ne!(
            observation,
            ObservationRef {
                producer_epoch: "run-2".into(),
                ..observation.clone()
            }
        );
        assert_eq!(
            ObservationRef::decode(&observation.encode()).unwrap(),
            observation
        );
        let mut independent_registry = position().source;
        independent_registry.issuer = Issuer::Server("env7".into());
        assert_ne!(independent_registry.encode(), position().source.encode());
        let first = MessageRef::occurrence(agent(), position(), vec!["/message".into()]).unwrap();
        let same_native =
            MessageRef::occurrence(agent(), position(), vec!["/message".into()]).unwrap();
        assert_eq!(first, same_native);
        let mut second_position = position();
        second_position.locator = SourceLocator::Jsonl {
            byte_start: 100,
            byte_end: 200,
            record_ordinal: 1,
        };
        let repeated_body =
            MessageRef::occurrence(agent(), second_position, vec!["/message".into()]).unwrap();
        assert_ne!(first, repeated_body);
        let mut rewritten = position();
        rewritten.generation = 4;
        assert_ne!(
            first,
            MessageRef::occurrence(agent(), rewritten, vec!["/message".into()]).unwrap()
        );
        assert_eq!(
            SourcePosition::decode(&position().encode())
                .unwrap()
                .locator,
            SourceLocator::Jsonl {
                byte_start: 0,
                byte_end: 100,
                record_ordinal: 0,
            }
        );
    }

    #[test]
    fn embedded_children_preserve_anchor_namespace_and_actual_child_id() {
        let child = AgentRef::Embedded {
            anchor_session: agent().anchor_session().clone(),
            namespace: "claude.agent".into(),
            native_child_id: "Worker:A".into(),
        };
        let other = AgentRef::Embedded {
            anchor_session: SessionId::decode("env7:claude:other-root").unwrap(),
            namespace: "claude.agent".into(),
            native_child_id: "Worker:A".into(),
        };
        assert_ne!(child, other);
        assert_eq!(AgentRef::decode(&child.encode()).unwrap(), child);
        assert!(
            matches!(child, AgentRef::Embedded { native_child_id, .. } if native_child_id == "Worker:A")
        );
    }

    #[test]
    fn wire_decoding_rejects_invalid_nested_identity_and_source_intervals() {
        let mut agent_json = serde_json::to_value(agent()).unwrap();
        agent_json["value"]["environment"] = serde_json::json!("bad:environment");
        assert!(serde_json::from_value::<AgentRef>(agent_json).is_err());
        let mut source_json = serde_json::to_value(position()).unwrap();
        source_json["locator"]["byteEnd"] = serde_json::json!(0);
        assert!(serde_json::from_value::<SourcePosition>(source_json).is_err());
        assert!(serde_json::from_value::<ObservationRef>(serde_json::json!({"issuer":{"kind":"server","value":"s"},"producerId":"p","producerEpoch":"e","producerSeq":0})).is_err());
    }
}
