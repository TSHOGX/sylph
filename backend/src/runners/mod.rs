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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    /// Write an executable `runner.sh` for `name` that answers `--describe`
    /// with `describe_stdout` and exits with `code`.
    fn write_runner(dir: &Path, name: &str, describe_stdout: &str, code: i32) {
        let runner_dir = dir.join(name);
        std::fs::create_dir_all(&runner_dir).unwrap();
        let script = runner_dir.join("runner.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat <<'EOF'\n{describe_stdout}\nEOF\nexit {code}\n"
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    fn descriptor_json(name: &str) -> String {
        format!(
            r#"{{"name":"{name}","description":"test agent","session_key":"resume",
                 "flags":{{"model":{{"choices":["fast","slow"],"description":"model"}}}},
                 "commands":{{"status":{{"description":"show status",
                   "args":{{"verbose":{{"type":"boolean","required":false}}}}}}}}}}"#
        )
    }

    #[tokio::test]
    async fn a_missing_runners_directory_yields_an_empty_registry() {
        // A fresh machine has no runners dir. That is an empty agent list, not
        // a startup failure.
        let registry = Registry::new("/nonexistent/runners/path").await.unwrap();
        assert!(registry.list_agents().is_empty());
        assert!(registry.broken_runners().is_empty());
    }

    #[tokio::test]
    async fn discovery_parses_a_descriptor_and_records_the_script_path() {
        let tmp = TempDir::new().unwrap();
        write_runner(tmp.path(), "claude", &descriptor_json("claude"), 0);

        let registry = Registry::new(tmp.path()).await.unwrap();

        let agents = registry.list_agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "claude");
        assert_eq!(agents[0].session_key, "resume");
        assert_eq!(agents[0].flags["model"].choices, vec!["fast", "slow"]);
        assert_eq!(agents[0].commands["status"].description, "show status");
        assert!(!agents[0].commands["status"].args["verbose"].required);
        assert!(registry.has_agent("claude"));
        assert_eq!(
            registry.get_runner_path("claude"),
            Some(tmp.path().join("claude").join("runner.sh").as_path())
        );
    }

    #[tokio::test]
    async fn a_broken_runner_is_recorded_without_failing_discovery() {
        // One misbehaving runner must not hide the healthy ones, so the UI can
        // still offer what works and report what does not.
        let tmp = TempDir::new().unwrap();
        write_runner(tmp.path(), "good", &descriptor_json("good"), 0);
        write_runner(tmp.path(), "exits-nonzero", "boom", 1);
        write_runner(tmp.path(), "bad-json", "not json at all", 0);

        let registry = Registry::new(tmp.path()).await.unwrap();

        assert_eq!(registry.list_agents().len(), 1);
        assert!(registry.has_agent("good"));
        let broken = registry.broken_runners();
        assert_eq!(broken.len(), 2);
        assert!(broken.contains_key("exits-nonzero"));
        assert!(broken.contains_key("bad-json"));
        assert!(!registry.has_agent("bad-json"));
        assert!(registry.get_runner_path("bad-json").is_none());
    }

    #[tokio::test]
    async fn entries_without_a_runner_script_are_skipped_silently() {
        // Directories that are not runners are ordinary, not broken: a README
        // or a shared lib folder should produce no diagnostic at all.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("not-a-runner")).unwrap();
        std::fs::write(tmp.path().join("README.md"), "docs").unwrap();
        write_runner(tmp.path(), "claude", &descriptor_json("claude"), 0);

        let registry = Registry::new(tmp.path()).await.unwrap();

        assert_eq!(registry.list_agents().len(), 1);
        assert!(
            registry.broken_runners().is_empty(),
            "a non-runner directory is not a broken runner"
        );
    }

    #[tokio::test]
    async fn a_minimal_descriptor_needs_only_a_name() {
        // Every other field defaults, so a runner can ship a one-line
        // descriptor and still be usable.
        let tmp = TempDir::new().unwrap();
        write_runner(tmp.path(), "minimal", r#"{"name":"minimal"}"#, 0);

        let registry = Registry::new(tmp.path()).await.unwrap();

        let agents = registry.list_agents();
        assert_eq!(agents.len(), 1);
        assert!(agents[0].description.is_empty());
        assert!(agents[0].flags.is_empty());
        assert!(agents[0].commands.is_empty());
    }

    #[tokio::test]
    async fn refresh_picks_up_a_newly_installed_runner_and_drops_a_removed_one() {
        let tmp = TempDir::new().unwrap();
        write_runner(tmp.path(), "claude", &descriptor_json("claude"), 0);
        let mut registry = Registry::new(tmp.path()).await.unwrap();
        assert_eq!(registry.list_agents().len(), 1);

        write_runner(tmp.path(), "codex", &descriptor_json("codex"), 0);
        registry.refresh().await.unwrap();
        assert_eq!(registry.list_agents().len(), 2);
        assert!(registry.has_agent("codex"));

        std::fs::remove_dir_all(tmp.path().join("codex")).unwrap();
        registry.refresh().await.unwrap();
        assert_eq!(registry.list_agents().len(), 1);
        assert!(!registry.has_agent("codex"), "refresh clears stale entries");
    }

    #[tokio::test]
    async fn refresh_clears_a_runner_that_stopped_being_broken() {
        let tmp = TempDir::new().unwrap();
        write_runner(tmp.path(), "claude", "not json", 0);
        let mut registry = Registry::new(tmp.path()).await.unwrap();
        assert_eq!(registry.broken_runners().len(), 1);

        write_runner(tmp.path(), "claude", &descriptor_json("claude"), 0);
        registry.refresh().await.unwrap();

        assert!(registry.broken_runners().is_empty(), "stale breakage cleared");
        assert!(registry.has_agent("claude"));
    }

    #[tokio::test]
    async fn an_unknown_agent_has_no_path() {
        let tmp = TempDir::new().unwrap();
        let registry = Registry::new(tmp.path()).await.unwrap();
        assert!(!registry.has_agent("nope"));
        assert!(registry.get_runner_path("nope").is_none());
    }
}
