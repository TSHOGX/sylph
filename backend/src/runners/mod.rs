use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

/// Runner capability descriptor from `--describe` protocol
/// This matches the actual format from runner.sh implementations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub session_key: String,
    #[serde(default)]
    pub flags: HashMap<String, FlagSpec>,
    #[serde(default)]
    pub commands: HashMap<String, CommandSpec>,
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

/// Runner registry with discovered agents
pub struct Registry {
    runners_dir: PathBuf,
    agents: HashMap<String, AgentInfo>,
    broken: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct AgentInfo {
    descriptor: AgentDescriptor,
    runner_path: PathBuf,
}

impl Registry {
    /// Create and populate registry by discovering runners
    pub async fn new(runners_dir: impl AsRef<Path>) -> Result<Self> {
        let runners_dir = runners_dir.as_ref().to_path_buf();
        let mut registry = Self {
            runners_dir: runners_dir.clone(),
            agents: HashMap::new(),
            broken: HashMap::new(),
        };

        registry.discover().await?;
        Ok(registry)
    }

    /// Discover all runners in runners_dir
    async fn discover(&mut self) -> Result<()> {
        if !self.runners_dir.exists() {
            warn!("Runners directory does not exist: {:?}", self.runners_dir);
            return Ok(());
        }

        let mut read_dir = tokio::fs::read_dir(&self.runners_dir)
            .await
            .context("Failed to read runners directory")?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .context("Failed to read directory entry")?
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let runner_script = path.join("runner.sh");
            if !runner_script.exists() {
                continue;
            }

            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            match self.describe_runner(&runner_script).await {
                Ok(descriptor) => {
                    info!(
                        "Discovered runner: {} at {:?}",
                        descriptor.name, runner_script
                    );
                    self.agents.insert(
                        name.clone(),
                        AgentInfo {
                            descriptor,
                            runner_path: runner_script,
                        },
                    );
                }
                Err(e) => {
                    error!("Failed to describe runner {}: {}", name, e);
                    self.broken.insert(name, format!("{:#}", e));
                }
            }
        }

        Ok(())
    }

    /// Call runner.sh --describe and parse JSON response
    async fn describe_runner(&self, runner_path: &Path) -> Result<AgentDescriptor> {
        debug!("Describing runner at {:?}", runner_path);

        let output = Command::new(runner_path)
            .arg("--describe")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to spawn runner")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "Runner --describe failed with exit code {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout)
            .with_context(|| format!("Failed to parse descriptor JSON: {}", stdout))
    }

    /// List all successfully discovered agents
    pub fn list_agents(&self) -> Vec<AgentDescriptor> {
        self.agents
            .values()
            .map(|info| info.descriptor.clone())
            .collect()
    }

    /// Get broken runners with error messages
    pub fn broken_runners(&self) -> &HashMap<String, String> {
        &self.broken
    }

    /// Check if an agent exists
    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }

    /// Get runner path for an agent
    pub fn get_runner_path(&self, name: &str) -> Option<&Path> {
        self.agents.get(name).map(|info| info.runner_path.as_path())
    }

    /// Refresh registry by re-running discovery
    pub async fn refresh(&mut self) -> Result<()> {
        self.agents.clear();
        self.broken.clear();
        self.discover().await
    }
}
