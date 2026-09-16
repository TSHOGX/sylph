//! Environment-local directory browsing and Quick Start path resolution.
use super::*;
impl EnvAgent {
    pub(super) fn resolve_workdir(&self, workdir: &str) -> Result<PathBuf> {
        if workdir != QUICK_START_SENTINEL {
            return Ok(PathBuf::from(workdir));
        }
        let home = self
            .config
            .path_policy
            .roots()
            .first()
            .context("environment has no home directory")?;
        let path = self
            .config
            .path_policy
            .contain_write_target(home.join("scratch"))?;
        std::fs::create_dir_all(&path)?;
        Ok(self.config.path_policy.contain(path)?)
    }
    pub(super) fn browse_roots(&self) -> serde_json::Value {
        let roots = self.config.path_policy.roots();
        let home = roots
            .first()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entries: Vec<_> = roots
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let display = path.to_string_lossy();
                let name = if index == 0 {
                    "Home"
                } else {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(&display)
                };
                serde_json::json!({"name":name,"path":display})
            })
            .collect();
        serde_json::json!({"home":home,"roots":entries})
    }
    pub(super) fn browse_tree(&self, path: Option<String>) -> Result<serde_json::Value> {
        let path = path
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| self.config.path_policy.roots().first().cloned())
            .context("environment has no browse root")?;
        let path = self.config.path_policy.contain(path)?;
        const SKIPPED: &[&str] = &[
            ".git",
            "node_modules",
            "target",
            ".next",
            "__pycache__",
            ".venv",
            "venv",
            ".mypy_cache",
            ".pytest_cache",
            "dist",
            "build",
        ];
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in std::fs::read_dir(&path)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SKIPPED.contains(&name.as_str()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_symlink = file_type.is_symlink();
            let resolved = if is_symlink {
                match self.config.path_policy.contain(entry.path()) {
                    Ok(path) => path,
                    Err(_) => continue,
                }
            } else {
                entry.path()
            };
            let Ok(metadata) = std::fs::metadata(&resolved) else {
                continue;
            };
            if !metadata.is_dir() {
                continue;
            }
            if entries.len() >= 2000 {
                truncated = true;
                break;
            }
            let modified_at_ms = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as i64;
            entries.push(serde_json::json!({"name":name,"path":entry.path(),"modifiedAtMs":modified_at_ms,"isSymlink":is_symlink}));
        }
        entries
            .sort_by_cached_key(|entry| entry["name"].as_str().unwrap_or_default().to_lowercase());
        let parent = path
            .parent()
            .filter(|parent| self.config.path_policy.contains(parent));
        Ok(serde_json::json!({"path":path,"parent":parent,"entries":entries,"truncated":truncated}))
    }
}
