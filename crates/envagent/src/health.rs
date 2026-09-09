//! Machine-local agent inventory.
//!
//! Which agents this machine can run is discovered, not configured, and it is a fact
//! about this one machine (`docs/environments.md` § Agent availability). Every agent
//! the build knows is reported, available or not, because "known and unavailable" is
//! the state that names a fix.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use sylph_protocol::{Adapter, AgentInventory};
use tokio::time::timeout;

use crate::adapters::AdapterSource;
pub type AgentHealth = AgentInventory;
pub type EnvironmentHealth = sylph_protocol::EnvironmentHealth;

/// How long a vendor CLI gets to say what it is before it is called unusable.
///
/// A CLI that cannot answer `--version` in ten seconds is not going to run a turn. A
/// generous bound rather than a tight one because a cold vendor binary on a slow disk
/// is a real case, and the cost of being wrong is refusing a session that would have
/// worked.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Probes every agent an adapter source knows, once per pass.
#[derive(Clone)]
pub struct HealthReporter {
    adapters: Arc<dyn AdapterSource>,
}

impl HealthReporter {
    pub fn new() -> Self {
        Self {
            adapters: crate::adapters::linked(),
        }
    }

    pub fn with_adapters(adapters: Arc<dyn AdapterSource>) -> Self {
        Self { adapters }
    }

    /// One entry per known agent, sorted by name.
    pub async fn inventory(&self) -> Vec<AgentInventory> {
        let probes = self.adapters.known_agents().into_iter().map(|name| {
            let adapters = self.adapters.clone();
            async move {
                let Some(adapter) = adapters.for_agent(&name) else {
                    return AgentInventory::unavailable(
                        name.clone(),
                        None,
                        format!("the `{name}` CLI is not on this machine's PATH"),
                    );
                };
                probe_one(name, adapter).await
            }
        });
        let mut agents = join_all(probes).await;
        agents.sort_by(|left, right| left.name.cmp(&right.name));
        agents
    }

    pub async fn snapshot(&self, _environment_id: impl Into<String>) -> EnvironmentHealth {
        EnvironmentHealth {
            connected: true,
            agents: self.inventory().await,
            facts: Default::default(),
        }
    }
}

impl Default for HealthReporter {
    fn default() -> Self {
        Self::new()
    }
}

/// Probe one adapter, bounding how long it may take.
pub async fn probe_one(name: String, adapter: Box<dyn Adapter>) -> AgentInventory {
    let floor = adapter.version_floor();
    match timeout(PROBE_TIMEOUT, adapter.probe()).await {
        Ok(Ok(info)) => match floor {
            Some(floor) => match info.version.as_deref() {
                Some(version) if version_meets_floor(version, floor) => {
                    AgentInventory::available(info)
                }
                Some(version) => AgentInventory::unavailable(
                    name,
                    Some(version.to_string()),
                    format!(
                        "the `{}` CLI reports version {version}, but this adapter requires at least {floor}",
                        info.name
                    ),
                ),
                None => AgentInventory::unavailable(
                    name,
                    None,
                    format!(
                        "the `{}` CLI did not report a version; this adapter requires at least {floor}",
                        info.name
                    ),
                ),
            },
            None => AgentInventory::available(info),
        },
        Ok(Err(error)) => AgentInventory::unavailable(name, None, format!("{error:#}")),
        Err(_) => AgentInventory::unavailable(
            name,
            None,
            format!("the CLI did not answer a version probe within {PROBE_TIMEOUT:?}"),
        ),
    }
}

/// Compare dotted numeric vendor versions without treating a malformed value as valid.
///
/// Vendor probes currently return numeric versions (`2.1.250`, `0.150.1`, and `0.84.3`).
/// Keeping this parser here avoids a semver policy in the protocol crate, while rejecting
/// a future non-numeric report instead of silently making an availability decision from a
/// partial string. Missing components compare as zero, so `1.2` is below `1.2.1`.
pub fn version_meets_floor(version: &str, floor: &str) -> bool {
    let Some(version) = parse_version(version) else {
        return false;
    };
    let Some(floor) = parse_version(floor) else {
        return false;
    };
    version >= floor
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut components = Vec::new();
    for component in trimmed.split('.') {
        if component.is_empty() {
            return None;
        }
        let digits = component.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        components.push(component[..digits].parse().ok()?);
        if digits != component.len() {
            // A prerelease suffix is not a version this adapter has measured. Refuse it
            // rather than accidentally treating `2.1.247-beta` as the released floor.
            return None;
        }
    }
    while components.len() < 3 {
        components.push(0);
    }
    Some(components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inventory_lists_every_known_agent_sorted_with_a_reason_when_absent() {
        let inventory = HealthReporter::new().inventory().await;
        let names: Vec<_> = inventory.iter().map(|agent| agent.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(
            names,
            crate::adapters::linked()
                .known_agents()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "an absent CLI is still listed, so a client can name the fix"
        );
        for agent in &inventory {
            if agent.available {
                assert!(agent.info.is_some());
                assert!(agent.reason.is_none());
            } else {
                assert!(
                    agent.reason.is_some(),
                    "{} says why it is unavailable",
                    agent.name
                );
                assert!(agent.info.is_none());
            }
        }
    }

    #[test]
    fn version_floor_accepts_equal_and_newer_numeric_versions() {
        assert!(version_meets_floor("2.1.247", "2.1.247"));
        assert!(version_meets_floor("2.1.248", "2.1.247"));
        assert!(version_meets_floor("2.2", "2.1.247"));
    }

    #[test]
    fn version_floor_rejects_older_missing_and_non_numeric_versions() {
        assert!(!version_meets_floor("2.1.246", "2.1.247"));
        assert!(!version_meets_floor("2.1", "2.1.247"));
        assert!(!version_meets_floor("2.1.247-beta", "2.1.247"));
        assert!(!version_meets_floor("", "2.1.247"));
    }
}
