//! Pairing credentials kept on the environment machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Durable credential returned by the server's one-shot pairing exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<String>,
    pub token: String,
    pub server: String,
}

/// Atomic, permission-restricted credential file access.
#[derive(Debug, Clone)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<Credential>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&self.path)
            .with_context(|| format!("reading envagent credential {}", self.path.display()))?;
        let credential = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding envagent credential {}", self.path.display()))?;
        Ok(Some(credential))
    }

    /// Write through a sibling temporary file and rename it into place.
    ///
    /// A credential is security-sensitive, so the file is mode `0600` on Unix
    /// and is never partially replaced if the process is interrupted.
    pub fn save(&self, credential: &Credential) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating credential directory {}", parent.display()))?;
        }
        let tmp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(credential)?;
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("writing temporary credential {}", tmp.display()))?;
        restrict_permissions(&tmp)?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("installing credential {}", self.path.display()))?;
        restrict_permissions(&self.path)?;
        Ok(())
    }

    pub fn remove(&self) -> Result<bool> {
        if !self.path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&self.path)
            .with_context(|| format!("removing envagent credential {}", self.path.display()))?;
        Ok(true)
    }
}

fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_round_trip_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credential.json"));
        let credential = Credential {
            environment_id: "env7fk2".into(),
            installation_id: Some("install-1".into()),
            token: "secret".into(),
            server: "http://127.0.0.1:4317".into(),
        };
        store.save(&credential).unwrap();
        assert_eq!(store.load().unwrap(), Some(credential));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(store.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
