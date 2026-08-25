//! Discovering the agents this server can run.
//!
//! An agent is a directory containing an executable `adapter.sh` that speaks
//! `sylph.agent.v3`. Discovery learns what an agent *is* by handshaking with it,
//! because that is the only source: an adapter's capabilities and flags come from
//! the CLI behind it, not from a descriptor a human maintains beside it.
//!
//! Two roots are searched, bundled adapters first. A bundled adapter is versioned
//! with the protocol it implements; a user-added one lives outside the install and
//! may shadow a bundled name, which is the escape hatch when a vendor changes
//! something faster than the bundled adapter catches up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::adapter::link::{Link, LinkConfig};
use crate::env::Environment;

/// How long an agent has to complete a discovery handshake.
///
/// Matches the link's own budget: an adapter that cannot say who it is in ten
/// seconds will not run a turn either.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// What an agent is, as reported to clients.
///
/// Shape is unchanged from the v1 descriptor so no client had to change: `flags`
/// and `commands` are still maps keyed by name. What changed is where it comes
/// from — a live handshake rather than a static `--describe` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub session_key: String,
    #[serde(default)]
    pub flags: HashMap<String, FlagSpec>,
    /// Named commands, when discovery could learn them cheaply.
    ///
    /// Usually empty, and deliberately so: a project's real command list depends
    /// on files the CLI reads at startup, so it belongs to a session rather than
    /// to an agent. `GET /sessions/{id}/capabilities` is where dozens of them
    /// come from. Kept in the shape so the settings UI still renders.
    #[serde(default)]
    pub commands: HashMap<String, CommandSpec>,
    /// What the adapter declared it can do, e.g. `interrupt`, `approval`.
    #[serde(default)]
    pub supports: Vec<String>,
    /// The agent CLI's own version, when it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagSpec {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub description: String,
    #[serde(default)]
    pub args: HashMap<String, ArgSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    #[serde(rename = "type")]
    pub arg_type: String,
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

/// The agents discovered on this machine.
pub struct Registry {
    roots: Vec<PathBuf>,
    environment: Arc<dyn Environment>,
    agents: HashMap<String, AgentInfo>,
    broken: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct AgentInfo {
    descriptor: AgentDescriptor,
    adapter_path: PathBuf,
}

impl Registry {
    /// Discover agents under `roots`, in order of precedence.
    pub async fn new(
        roots: Vec<PathBuf>,
        environment: Arc<dyn Environment>,
    ) -> Result<Self> {
        let mut registry = Self {
            roots,
            environment,
            agents: HashMap::new(),
            broken: HashMap::new(),
        };
        registry.discover().await?;
        Ok(registry)
    }

    async fn discover(&mut self) -> Result<()> {
        for root in self.roots.clone() {
            if !root.exists() {
                debug!("Agent root does not exist: {:?}", root);
                continue;
            }

            let mut entries = match tokio::fs::read_dir(&root).await {
                Ok(entries) => entries,
                Err(error) => {
                    warn!("Cannot read agent root {:?}: {}", root, error);
                    continue;
                }
            };

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let adapter = path.join("adapter.sh");
                if !adapter.exists() {
                    // A directory holding a v1 `runner.sh` is an *agent* that this
                    // server cannot speak to, not an ordinary directory. Skipping
                    // it silently is what made a stale runners tree read as "this
                    // agent does not exist": discovery reported zero usable and
                    // zero broken, so nothing anywhere said the directory had been
                    // seen and rejected. A README stays silent; this does not.
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                    if !name.is_empty()
                        && !self.agents.contains_key(name)
                        && !self.broken.contains_key(name)
                        && (path.join("runner.sh").exists() || path.join("runner.py").exists())
                    {
                        warn!("Agent {} predates sylph.agent.v3: {:?} has a runner but no adapter.sh", name, path);
                        self.broken.insert(
                            name.to_string(),
                            format!(
                                "{:?} holds a v1 runner but no adapter.sh, so it cannot speak \
                                 sylph.agent.v3",
                                path
                            ),
                        );
                    }
                    continue;
                }

                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };

                // First root wins, so a bundled adapter is not replaced by a
                // later one with the same name — and a name already known good is
                // not re-probed just to be overwritten.
                if self.agents.contains_key(&name) {
                    continue;
                }

                match self.probe(&name, &adapter).await {
                    Ok(descriptor) => {
                        info!("Discovered agent {} at {:?}", name, adapter);
                        // A later root may carry the v3 adapter for a name an
                        // earlier root recorded as a v1 leftover. Usable wins:
                        // leaving it listed as broken would report an agent as
                        // unavailable while every link to it succeeded.
                        self.broken.remove(&name);
                        self.agents.insert(
                            name,
                            AgentInfo { descriptor, adapter_path: adapter },
                        );
                    }
                    Err(error) => {
                        error!("Agent {} is unusable: {:#}", name, error);
                        self.broken.insert(name, format!("{:#}", error));
                    }
                }
            }
        }

        Ok(())
    }

    /// Handshake with an adapter to learn what it is.
    ///
    /// A full link is established and immediately closed. That costs a process
    /// per agent at startup, and it buys the only honest answer: the adapter's
    /// declared capabilities and its CLI's actual version, rather than a
    /// hand-written descriptor that can disagree with both.
    ///
    /// The session is opened in a throwaway directory with identity injection
    /// off. Discovery must not create an agent-native session or touch a real
    /// project, and `soul: ""` is how the protocol spells "inject nothing".
    async fn probe(&self, name: &str, adapter: &Path) -> Result<AgentDescriptor> {
        let workdir = std::env::temp_dir().to_string_lossy().into_owned();

        let (link, _events) = tokio::time::timeout(
            DISCOVERY_TIMEOUT,
            Link::establish(
                self.environment.as_ref(),
                LinkConfig {
                    adapter_path: adapter.to_path_buf(),
                    workdir,
                    session_id: None,
                    soul: Some(String::new()),
                    flags: HashMap::new(),
                    permission_mode: None,
                    env: HashMap::new(),
                },
            ),
        )
        .await
        .context("Agent did not finish its handshake in time")?
        .context("Agent handshake failed")?;

        let identity = link.identity().clone();
        let capabilities = link.capabilities().await;

        // Closed immediately: discovery is a question, not a session. Leaving it
        // open would hold a process per installed agent for the life of the
        // server.
        link.close().await;

        if identity.agent_name != name {
            anyhow::bail!(
                "Agent in directory '{}' identifies itself as '{}'",
                name,
                identity.agent_name
            );
        }

        Ok(AgentDescriptor {
            name: name.to_string(),
            description: format!("{} via {}", identity.agent_name, identity.adapter),
            session_key: name.to_string(),
            flags: identity
                .flags
                .into_iter()
                .map(|(key, spec)| {
                    (
                        key,
                        FlagSpec { choices: spec.choices, description: spec.description },
                    )
                })
                .collect(),
            commands: capabilities
                .commands
                .into_iter()
                .map(|command| {
                    (
                        command.name,
                        CommandSpec {
                            description: command.description,
                            args: HashMap::new(),
                        },
                    )
                })
                .collect(),
            supports: identity
                .supports
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            version: identity.agent_version,
        })
    }

    pub fn list_agents(&self) -> Vec<AgentDescriptor> {
        self.agents
            .values()
            .map(|info| info.descriptor.clone())
            .collect()
    }

    /// Agents that are installed but unusable, with why.
    pub fn broken_adapters(&self) -> &HashMap<String, String> {
        &self.broken
    }

    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }

    /// The adapter to run for an agent.
    pub fn adapter_path(&self, name: &str) -> Option<&Path> {
        self.agents.get(name).map(|info| info.adapter_path.as_path())
    }

    pub async fn refresh(&mut self) -> Result<()> {
        self.agents.clear();
        self.broken.clear();
        self.discover().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::LocalEnvironment;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Install an adapter that answers the handshake as `name`.
    fn write_adapter(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adapter.sh");
        std::fs::write(&path, format!("#!/usr/bin/env python3\n{body}")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    /// A cooperative adapter that identifies itself as `name`.
    fn cooperative(name: &str, version: &str) -> String {
        format!(
            r#"
import json, sys

def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    t, mid = msg.get("t"), msg.get("id")
    if t == "hello":
        emit({{"t": "ok", "id": mid, "adapter": "test/1.0.0",
               "agent": {{"name": "{name}", "version": "{version}"}},
               "supports": ["interrupt", "approval"],
               "flags": {{"model": {{"choices": ["fast", "slow"], "description": "model"}}}}}})
    elif t == "open":
        emit({{"t": "ok", "id": mid, "sessionId": "probe-1",
               "commands": [{{"name": "status", "description": "show status"}}]}})
    elif t == "close":
        emit({{"t": "ok", "id": mid}})
        break
    else:
        emit({{"t": "ok", "id": mid}})
"#
        )
    }

    fn environment() -> Arc<dyn Environment> {
        Arc::new(LocalEnvironment::new())
    }

    #[tokio::test]
    async fn a_missing_agents_directory_yields_an_empty_registry() {
        // A fresh machine has no adapters installed. That is an empty list, not a
        // startup failure — the server still serves history and settings.
        let registry = Registry::new(vec!["/nonexistent/adapters".into()], environment())
            .await
            .unwrap();
        assert!(registry.list_agents().is_empty());
        assert!(registry.broken_adapters().is_empty());
    }

    /// Discovery reads what the adapter says, not a descriptor beside it. The CLI
    /// version is the proof: nothing in the directory carries it.
    #[tokio::test]
    async fn discovery_learns_an_agent_from_its_handshake() {
        let tmp = TempDir::new().unwrap();
        write_adapter(tmp.path(), "claude", &cooperative("claude", "2.1.238"));

        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();

        let agents = registry.list_agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "claude");
        assert_eq!(agents[0].version.as_deref(), Some("2.1.238"));
        assert_eq!(agents[0].flags["model"].choices, vec!["fast", "slow"]);
        assert!(agents[0].supports.contains(&"interrupt".to_string()));
        assert!(registry.has_agent("claude"));
        assert_eq!(
            registry.adapter_path("claude"),
            Some(tmp.path().join("claude").join("adapter.sh").as_path())
        );
    }

    /// One unusable agent must not hide the working ones, so the UI can still
    /// offer what runs and report what does not.
    #[tokio::test]
    async fn a_broken_agent_is_recorded_without_failing_discovery() {
        let tmp = TempDir::new().unwrap();
        write_adapter(tmp.path(), "good", &cooperative("good", "1.0"));
        write_adapter(tmp.path(), "exits", "import sys\nsys.exit(1)\n");
        write_adapter(
            tmp.path(),
            "silent",
            "import sys\nfor line in sys.stdin:\n    pass\n",
        );

        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();

        assert_eq!(registry.list_agents().len(), 1);
        assert!(registry.has_agent("good"));
        let broken = registry.broken_adapters();
        assert_eq!(broken.len(), 2);
        assert!(broken.contains_key("exits"));
        assert!(broken.contains_key("silent"));
        assert!(registry.adapter_path("silent").is_none());
    }

    /// A name mismatch is a configuration error worth surfacing: the directory is
    /// how a session records its agent, so an adapter answering to another name
    /// would make sessions unresumable after a rename.
    #[tokio::test]
    async fn an_adapter_that_answers_to_another_name_is_rejected() {
        let tmp = TempDir::new().unwrap();
        write_adapter(tmp.path(), "codex", &cooperative("claude", "1.0"));

        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();

        assert!(!registry.has_agent("codex"));
        assert!(registry.broken_adapters()["codex"].contains("identifies itself"));
    }

    #[tokio::test]
    async fn directories_without_an_adapter_are_skipped_silently() {
        // A README or a shared library folder is ordinary, not broken, and should
        // produce no diagnostic at all.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("not-an-agent")).unwrap();
        std::fs::write(tmp.path().join("README.md"), "docs").unwrap();
        write_adapter(tmp.path(), "claude", &cooperative("claude", "1.0"));

        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();

        assert_eq!(registry.list_agents().len(), 1);
        assert!(registry.broken_adapters().is_empty());
    }

    /// A v1 runner tree is an agent that cannot be spoken to, not an absent one.
    ///
    /// This is the shape that made a real slash menu look broken: `SYLPH_RUNNERS_DIR`
    /// held four v1 agents with `runner.sh` and no `adapter.sh`, so discovery
    /// reported "0 usable, 0 broken" and nothing anywhere said those directories
    /// had been seen. The composer then had no commands to offer and no reason to
    /// give, which reads as a broken feature rather than a stale install.
    #[tokio::test]
    async fn a_v1_runner_without_an_adapter_is_reported_rather_than_skipped() {
        let tmp = TempDir::new().unwrap();
        let legacy = tmp.path().join("codex");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("runner.sh"), "#!/bin/sh\n").unwrap();

        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();

        assert!(!registry.has_agent("codex"));
        assert!(
            registry.broken_adapters()["codex"].contains("adapter.sh"),
            "the reason names what is missing: {:?}",
            registry.broken_adapters()
        );
    }

    /// A v3 adapter in a later root outranks a v1 leftover of the same name.
    ///
    /// Otherwise an agent that every link reaches successfully is still listed as
    /// broken, and the UI reports it unavailable while it works.
    #[tokio::test]
    async fn a_usable_adapter_clears_a_v1_leftover_of_the_same_name() {
        let legacy_root = TempDir::new().unwrap();
        let legacy = legacy_root.path().join("claude");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("runner.sh"), "#!/bin/sh\n").unwrap();

        let adapter_root = TempDir::new().unwrap();
        write_adapter(adapter_root.path(), "claude", &cooperative("claude", "1.0"));

        // The v1 root first, so the leftover is recorded before the adapter is
        // found — the order that would otherwise leave it listed as broken.
        let registry = Registry::new(
            vec![
                legacy_root.path().to_path_buf(),
                adapter_root.path().to_path_buf(),
            ],
            environment(),
        )
        .await
        .unwrap();

        assert!(registry.has_agent("claude"));
        assert!(registry.broken_adapters().is_empty());
    }

    /// The first root wins, which is what lets a user shadow a bundled adapter
    /// when a vendor changes faster than the bundled one keeps up.
    #[tokio::test]
    async fn an_earlier_root_shadows_a_later_one_with_the_same_agent() {
        let bundled = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        write_adapter(bundled.path(), "claude", &cooperative("claude", "bundled"));
        write_adapter(user.path(), "claude", &cooperative("claude", "user"));

        let registry = Registry::new(
            vec![user.path().to_path_buf(), bundled.path().to_path_buf()],
            environment(),
        )
        .await
        .unwrap();

        let agents = registry.list_agents();
        assert_eq!(agents.len(), 1, "one name, one agent");
        assert_eq!(agents[0].version.as_deref(), Some("user"));
    }

    #[tokio::test]
    async fn refresh_picks_up_a_new_agent_and_drops_a_removed_one() {
        let tmp = TempDir::new().unwrap();
        write_adapter(tmp.path(), "claude", &cooperative("claude", "1.0"));
        let mut registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();
        assert_eq!(registry.list_agents().len(), 1);

        write_adapter(tmp.path(), "codex", &cooperative("codex", "1.0"));
        registry.refresh().await.unwrap();
        assert_eq!(registry.list_agents().len(), 2);

        std::fs::remove_dir_all(tmp.path().join("codex")).unwrap();
        registry.refresh().await.unwrap();
        assert_eq!(registry.list_agents().len(), 1);
        assert!(!registry.has_agent("codex"), "refresh clears stale entries");
    }

    #[tokio::test]
    async fn refresh_clears_an_agent_that_stopped_being_broken() {
        let tmp = TempDir::new().unwrap();
        write_adapter(tmp.path(), "claude", "import sys\nsys.exit(1)\n");
        let mut registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();
        assert_eq!(registry.broken_adapters().len(), 1);

        write_adapter(tmp.path(), "claude", &cooperative("claude", "1.0"));
        registry.refresh().await.unwrap();

        assert!(registry.broken_adapters().is_empty());
        assert!(registry.has_agent("claude"));
    }

    #[tokio::test]
    async fn an_unknown_agent_has_no_adapter_path() {
        let tmp = TempDir::new().unwrap();
        let registry = Registry::new(vec![tmp.path().to_path_buf()], environment())
            .await
            .unwrap();
        assert!(!registry.has_agent("nope"));
        assert!(registry.adapter_path("nope").is_none());
    }
}
