//! The host the server runs on: always reachable, and its own filesystem.
//!
//! The one implementation of [`super::Environment`] until pairing lands. It answers the
//! two questions in that trait and holds nothing else — no process, no stdio, no signal —
//! for the reason recorded there.

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

    async fn probe(&self) -> Health {
        // The host is reachable by definition: this code is running on it.
        Health::up()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_local_environment_is_always_reachable_and_identifies_itself() {
        let env = LocalEnvironment::new();
        assert!(env.probe().await.available);
        let descriptor = env.descriptor();
        assert_eq!(descriptor.kind, "local");
        assert_eq!(descriptor.id, "local");
    }
}
