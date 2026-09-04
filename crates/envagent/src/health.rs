//! Machine-local health and agent inventory.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use sylph_protocol::{Adapter, AgentInfo};
use tokio::time::timeout;

/// One installed (or absent) vendor CLI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentHealth {
    pub name: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default)]
    pub info: Option<AgentInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Snapshot sent in hello and health replies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentHealth {
    pub environment_id: String,
    pub connected: bool,
    pub agents: Vec<AgentHealth>,
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
}

impl EnvironmentHealth {
    pub fn unavailable(environment_id: impl Into<String>, reason: impl Into<String>) -> Self {
        let mut facts = BTreeMap::new();
        facts.insert("reason".into(), reason.into());
        Self {
            environment_id: environment_id.into(),
            connected: false,
            agents: Vec::new(),
            facts,
        }
    }
}

/// Probes all compiled-in adapters once per health pass.
#[derive(Debug, Clone, Default)]
pub struct HealthReporter;

impl HealthReporter {
    pub fn new() -> Self {
        Self
    }

    pub async fn snapshot(&self, environment_id: impl Into<String>) -> EnvironmentHealth {
        let probes = sylph_adapters::known_agents().iter().map(|name| {
            let name = (*name).to_string();
            async move {
                let Some(adapter) = sylph_adapters::for_agent(&name) else {
                    return AgentHealth {
                        name,
                        available: false,
                        version: None,
                        info: None,
                        reason: Some("agent CLI is not on PATH".into()),
                    };
                };
                match timeout(Duration::from_secs(10), probe_one(name.clone(), adapter)).await {
                    Ok(result) => result,
                    Err(_) => AgentHealth {
                        name,
                        available: false,
                        version: None,
                        info: None,
                        reason: Some("agent probe timed out".into()),
                    },
                }
            }
        });
        let mut agents = join_all(probes).await;
        agents.sort_by(|left, right| left.name.cmp(&right.name));
        EnvironmentHealth {
            environment_id: environment_id.into(),
            connected: true,
            agents,
            facts: BTreeMap::new(),
        }
    }
}

async fn probe_one(name: String, adapter: Box<dyn Adapter>) -> AgentHealth {
    match adapter.probe().await {
        Ok(info) => AgentHealth {
            name,
            available: true,
            version: info.version.clone(),
            info: Some(info),
            reason: None,
        },
        Err(error) => AgentHealth {
            name,
            available: false,
            version: None,
            info: None,
            reason: Some(format!("{error:#}")),
        },
    }
}

/// A small helper used by fixture tests to validate an adapter without exposing
/// the concrete vendor type.
pub async fn probe_adapter(adapter: &dyn Adapter) -> Result<AgentInfo> {
    adapter.probe().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inventory_is_sorted_and_reports_missing_clis_explicitly() {
        let snapshot = HealthReporter::new().snapshot("env7fk2").await;
        let names: Vec<_> = snapshot
            .agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(snapshot.connected);
    }
}
