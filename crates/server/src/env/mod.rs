//! Where a session's work happens.
//!
//! # Transitional, and the spec says so
//!
//! `docs/environments.md` § The envagent is the whole abstraction is explicit that there
//! is **no** `Environment` trait with one implementation per substrate: local, SSH, and
//! sandbox differ only in how an envagent came to exist, and after it exists they are the
//! same case. The server asks only about paired envagent state; it does not model
//! the server host as a privileged execution substrate.
//!
//! What it is *not* is a process abstraction. A vendor CLI's process belongs to the
//! adapter that knows how to talk to it (`crates/adapters/src/process.rs`), so nothing
//! here spawns, signals, or holds stdio. Both remaining operations are answerable over an
//! RPC to another machine, which is what step 4 needs.

use serde::{Deserialize, Serialize};

pub mod bridge;
pub mod registry;

pub use bridge::{DispatchResult, EnvBridge, IngestResult};
pub use registry::{
    EnvironmentRegistry, EnvironmentSnapshot, PairingGrant, PairingOffer, RegistryError,
};

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
