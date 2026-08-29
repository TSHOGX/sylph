//! Where a session's work happens.
//!
//! Local fork/exec used to be the interface rather than an implementation of
//! one: the supervisor called `Command::new(path).current_dir(workdir)` directly.
//! This module is the seam that makes local, a Khora sandbox, and SSH
//! interchangeable — see `docs/environments.md` for why the trait has exactly
//! these four operations and no more.
//!
//! # Three operations, and `spawn` is not one of them any more
//!
//! The shape used to be dictated by the adapter protocol: NDJSON over a duplex byte
//! stream, plus a signal, plus files. That is gone with the subprocess — an adapter is a
//! trait linked in, and a vendor CLI's process belongs to the adapter that knows how to
//! talk to it (`crates/adapters/src/process.rs`). `spawn`, `ProcessSpec`, `ProcessHandle`,
//! and `ProcessSignal` had exactly one caller between them and went with it.
//!
//! What is left is what an environment is *asked* rather than what it can be made to do:
//! what it is called, whether a path is a directory inside it, and whether it can run work
//! right now. All three are answerable over an RPC to another machine, which is what step 4
//! needs — and none of them assumes the caller holds the process.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod local;

pub use local::LocalEnvironment;

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
        Self { available: true, reason: None }
    }

    pub fn down(reason: impl Into<String>) -> Self {
        Self { available: false, reason: Some(reason.into()) }
    }
}

/// A place where sessions run.
#[async_trait]
pub trait Environment: Send + Sync {
    fn descriptor(&self) -> EnvironmentDescriptor;

    /// Whether a path exists as a directory inside the environment.
    ///
    /// Session creation validates a workdir, and that check has to happen where
    /// the work will run. Validating a sandbox path against the host filesystem
    /// would accept paths that do not exist and reject ones that do.
    async fn is_dir(&self, path: &str) -> Result<bool>;

    async fn probe(&self) -> Health;
}
