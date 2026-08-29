//! The host itself: the host filesystem, and the fact that it is always reachable.
//!
//! # What this stopped doing, and why that is a subtraction rather than a loss
//!
//! It used to be `tokio::process` plus the host filesystem: `spawn` built a `Command`,
//! put the child in its own process group, and handed back stdio and a `killpg` signaller
//! — deliberately bit-for-bit what the supervisor did inline before the seam existed.
//!
//! Nothing calls that any more. The server no longer starts a process to reach an agent:
//! an adapter is a Rust trait linked in, and where a vendor CLI genuinely needs a process
//! the adapter owns it (`crates/adapters/src/process.rs`). So a second process-spawning
//! implementation here would be one fact with two derivations — and the one that survived
//! is the one whose caller exists.
//!
//! What remains is what `docs/environments.md` needs an environment *for*: what it is
//! called, whether a path is a directory inside it, and whether it can run work right now.

use anyhow::Result;
use async_trait::async_trait;

use super::{Environment, EnvironmentDescriptor, Health};

pub struct LocalEnvironment;

impl LocalEnvironment {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Environment for LocalEnvironment {
    fn descriptor(&self) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            kind: "local".to_string(),
            id: "local".to_string(),
            label: "This machine".to_string(),
        }
    }

    async fn is_dir(&self, path: &str) -> Result<bool> {
        Ok(tokio::fs::metadata(path)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false))
    }

    async fn probe(&self) -> Health {
        // The host is reachable by definition: this code is running on it.
        Health::up()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_dir_answers_for_the_host_filesystem() {
        let env = LocalEnvironment::new();
        assert!(env.is_dir("/tmp").await.unwrap());
        assert!(!env.is_dir("/tmp/definitely-not-here-9f3c").await.unwrap());
        // A file is not a directory, which is what session creation must reject.
        assert!(!env.is_dir("/etc/hosts").await.unwrap());
    }

    #[tokio::test]
    async fn the_local_environment_is_always_reachable_and_identifies_itself() {
        let env = LocalEnvironment::new();
        assert!(env.probe().await.available);
        let descriptor = env.descriptor();
        assert_eq!(descriptor.kind, "local");
        assert_eq!(descriptor.id, "local");
    }
}
