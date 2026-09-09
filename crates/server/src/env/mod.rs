//! Where a session's work happens.
//!
//! # Transitional, and the spec says so
//!
//! `docs/environments.md` § The envagent is the whole abstraction is explicit that there
//! is **no** `Environment` trait with one implementation per substrate: local, SSH, and
//! sandbox differ only in how an envagent came to exist, and after it exists they are the
//! same case. So this is scaffolding for step 4 rather than the design — it holds the two
//! questions the server asks about a machine before it can talk to one, and
//! [`LocalEnvironment`](crate::env::LocalEnvironment) answers them for the host the server runs on.
//!
//! What it is *not* is a process abstraction. A vendor CLI's process belongs to the
//! adapter that knows how to talk to it (`crates/adapters/src/process.rs`), so nothing
//! here spawns, signals, or holds stdio. Both remaining operations are answerable over an
//! RPC to another machine, which is what step 4 needs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod bridge;
pub mod registry;

pub use bridge::{DispatchResult, EnvBridge, IngestResult};
pub use registry::{
    EnvironmentRegistry, EnvironmentSnapshot, PairingGrant, PairingOffer, RegistryError,
};

/// Server-side view of paired environments used by retention and diagnostics.
/// It never claims the server host is reachable; reachability comes from the
/// registry's authenticated envagent lease.
#[derive(Clone)]
pub struct RegistryEnvironment {
    registry: EnvironmentRegistry,
}

impl RegistryEnvironment {
    pub fn new(registry: EnvironmentRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl Environment for RegistryEnvironment {
    fn descriptor(&self) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            kind: "envagent".into(),
            id: "paired".into(),
            label: "Paired environments".into(),
        }
    }

    async fn probe(&self) -> Health {
        if self
            .registry
            .list()
            .await
            .iter()
            .any(|environment| environment.connected)
        {
            Health::up()
        } else {
            Health::down("no envagent is connected")
        }
    }
}

/// What an environment is, as reported to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    /// `local`, `khora`, `ssh`.
    pub kind: String,
    /// Stable id within the kind. For `local` this is `"local"`; for a sandbox
    /// it is the sandbox id, which is what makes a session's binding meaningful
    /// across restarts.
    pub id: String,
    /// Human label for the UI.
    pub label: String,
}

/// Whether an environment can run work right now.
///
/// Reported per environment rather than folded into one boolean, because the
/// cause and the fix differ per kind: a missing binary locally, an unreachable
/// orchestrator or an exhausted node for Khora, a dead connection for SSH.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub available: bool,
    /// Why not, when `available` is false. Absent when healthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Health {
    pub fn up() -> Self {
        Self {
            available: true,
            reason: None,
        }
    }

    pub fn down(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
        }
    }
}

/// A place where sessions run.
#[async_trait]
pub trait Environment: Send + Sync {
    fn descriptor(&self) -> EnvironmentDescriptor;

    async fn probe(&self) -> Health;
}
