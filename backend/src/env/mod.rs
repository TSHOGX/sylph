//! Where a session's work happens.
//!
//! Local fork/exec used to be the interface rather than an implementation of
//! one: the supervisor called `Command::new(path).current_dir(workdir)` directly.
//! This module is the seam that makes local, a Khora sandbox, and SSH
//! interchangeable — see `docs/environments.md` for why the trait has exactly
//! these four operations and no more.
//!
//! The shape is dictated by the adapter protocol: NDJSON over a duplex byte
//! stream, plus a signal, plus files. Every candidate environment can provide
//! that. Had the protocol needed a pty or a shared local path for correlation,
//! this seam would leak — and it would only leak once the second implementation
//! arrived, which is exactly when it is most expensive to discover.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

pub mod local;

pub use local::LocalEnvironment;

/// How to start one process in an environment.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Working directory, interpreted inside the environment.
    pub cwd: String,
    pub env: HashMap<String, String>,
    /// Whether stdin stays open for the process's lifetime.
    ///
    /// This is the whole difference between the v1 and v2 adapter protocols, so
    /// it is explicit at the spawn boundary rather than implied by how the
    /// caller happens to use the handle. A v1 runner gets its prompt and an EOF;
    /// a v2 adapter gets a channel it can be asked questions on.
    pub duplex_stdin: bool,
}

/// Signals a caller may send. A closed set rather than a raw integer, because
/// only these three have a defined meaning across environments — Khora and SSH
/// both preserve them, and an arbitrary signal number does not survive either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Ask to terminate. What a stop request sends first.
    Term,
    /// Force termination. The escalation after the grace period.
    Kill,
    /// Interrupt, as a terminal's Ctrl-C. Distinct from `Term` because an agent
    /// CLI may treat it as "end this turn" rather than "exit".
    Int,
}

/// The ability to signal a process, separated from ownership of its streams.
///
/// Stopping a turn arrives on a different HTTP request than the one draining the
/// process's output, so the two capabilities have different owners and
/// lifetimes. Folding `signal` into [`ProcessHandle`] would mean the stopper had
/// to borrow the handle that the reader is already holding across an await.
///
/// It is also the truthful shape for a remote environment: in Khora signalling
/// is `SendSignal`, an independent RPC addressed by id, not a method on a local
/// process object.
#[async_trait]
pub trait ProcessSignal: Send + Sync {
    /// Signal the process, and its children where the environment groups them.
    ///
    /// Reaching the whole group matters: an adapter usually `exec`s or spawns a
    /// vendor CLI, and signalling only the wrapper leaves the CLI running.
    async fn signal(&self, signal: Signal) -> Result<()>;
}

/// A running process, with its streams and its exit.
#[async_trait]
pub trait ProcessHandle: Send {
    /// Taken once. `None` afterwards.
    fn take_stdin(&mut self) -> Option<Box<dyn AsyncWrite + Send + Unpin>>;
    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;
    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;

    /// A signal channel that outlives this borrow and can be held elsewhere.
    fn signaler(&self) -> Arc<dyn ProcessSignal>;

    /// Wait for exit. `None` exit code means killed by a signal.
    async fn wait(&mut self) -> Result<Option<i32>>;

    /// Process id where the environment exposes one, for diagnostics only.
    fn id(&self) -> Option<u32>;
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

    async fn spawn(&self, spec: ProcessSpec) -> Result<Box<dyn ProcessHandle>>;

    /// Whether a path exists as a directory inside the environment.
    ///
    /// Session creation validates a workdir, and that check has to happen where
    /// the work will run. Validating a sandbox path against the host filesystem
    /// would accept paths that do not exist and reject ones that do.
    async fn is_dir(&self, path: &str) -> Result<bool>;

    async fn probe(&self) -> Health;
}
