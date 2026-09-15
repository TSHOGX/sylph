//! Capture inventories authorize pre-identity acquisition without inventing a session.

use super::{
    identity::{decode_reference, encode_reference, nonempty},
    AgentRef, TraceIdentityError,
};
use crate::{AgentName, EnvironmentId};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRef {
    pub environment: EnvironmentId,
    pub vendor: AgentName,
    pub instance: String,
}

impl InventoryRef {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        EnvironmentId::parse(self.environment.as_str())
            .map_err(|error| TraceIdentityError::Environment(error.to_string()))?;
        AgentName::parse(self.vendor.as_str())
            .map_err(|error| TraceIdentityError::Session(error.to_string()))?;
        nonempty(&self.instance, "inventory instance")
    }
    pub fn encode(&self) -> String {
        encode_reference("inventory", self)
    }
    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("inventory", value)
    }
}

impl<'de> Deserialize<'de> for InventoryRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            environment: EnvironmentId,
            vendor: AgentName,
            instance: String,
        }
        let Wire {
            environment,
            vendor,
            instance,
        } = Wire::deserialize(deserializer)?;
        let value = Self {
            environment,
            vendor,
            instance,
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IngestionScope {
    Agent(AgentRef),
    Inventory(InventoryRef),
}

impl IngestionScope {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        match self {
            Self::Agent(agent) => agent.validate(),
            Self::Inventory(inventory) => inventory.validate(),
        }
    }
    pub fn environment(&self) -> &EnvironmentId {
        match self {
            Self::Agent(agent) => agent.environment(),
            Self::Inventory(inventory) => &inventory.environment,
        }
    }
    pub fn vendor(&self) -> &AgentName {
        match self {
            Self::Agent(agent) => &agent.anchor_session().agent,
            Self::Inventory(inventory) => &inventory.vendor,
        }
    }
    pub fn permits_subject(&self, agent: &AgentRef) -> bool {
        agent.environment() == self.environment() && &agent.anchor_session().agent == self.vendor()
    }
    pub fn encode(&self) -> String {
        encode_reference("ingestion_scope", self)
    }
    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        decode_reference("ingestion_scope", value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CommandTarget {
    Agent { agent: AgentRef },
    Launch { inventory: InventoryRef },
}

impl CommandTarget {
    pub fn validate(&self) -> Result<(), TraceIdentityError> {
        match self {
            Self::Agent { agent } => agent.validate(),
            Self::Launch { inventory } => inventory.validate(),
        }
    }
    pub fn ingestion_scope(&self) -> IngestionScope {
        match self {
            Self::Agent { agent } => IngestionScope::Agent(agent.clone()),
            Self::Launch { inventory } => IngestionScope::Inventory(inventory.clone()),
        }
    }
    /// Known capture subjects; a launch inventory is not an AgentThread.
    pub fn declared_subjects(&self) -> Vec<AgentRef> {
        match self {
            Self::Agent { agent } => vec![agent.clone()],
            Self::Launch { .. } => vec![],
        }
    }
    pub fn agent(&self) -> Option<&AgentRef> {
        match self {
            Self::Agent { agent } => Some(agent),
            Self::Launch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inventory(instance: &str) -> InventoryRef {
        InventoryRef {
            environment: EnvironmentId::parse("env7").unwrap(),
            vendor: AgentName::parse("codex").unwrap(),
            instance: instance.into(),
        }
    }
    #[test]
    fn an_inventory_address_preserves_instance_scope_without_becoming_a_session() {
        let one = inventory("launch:A/工作");
        let two = inventory("launch:a/工作");
        assert_ne!(one.encode(), two.encode());
        assert_eq!(InventoryRef::decode(&one.encode()).unwrap(), one);
        let launch = CommandTarget::Launch {
            inventory: one.clone(),
        };
        assert!(launch.declared_subjects().is_empty());
        assert_eq!(launch.agent(), None);
        assert_eq!(
            IngestionScope::decode(&launch.ingestion_scope().encode()).unwrap(),
            IngestionScope::Inventory(one)
        );
        assert!(AgentRef::decode(&two.encode()).is_err());
    }
    #[test]
    fn inventory_decoding_rejects_empty_instances_and_invalid_nested_identifiers() {
        let mut wire = serde_json::to_value(inventory("valid")).unwrap();
        wire["instance"] = serde_json::json!("");
        assert!(serde_json::from_value::<InventoryRef>(wire.clone()).is_err());
        wire["instance"] = serde_json::json!("valid");
        wire["vendor"] = serde_json::json!("bad:vendor");
        assert!(serde_json::from_value::<InventoryRef>(wire).is_err());
        let scope = IngestionScope::Inventory(inventory("scope"));
        assert!(!scope.permits_subject(&AgentRef::Session(
            crate::SessionId::decode("env7:pi:other").unwrap()
        )));
        assert!(!scope.permits_subject(&AgentRef::Session(
            crate::SessionId::decode("other:codex:session").unwrap()
        )));
    }
}
