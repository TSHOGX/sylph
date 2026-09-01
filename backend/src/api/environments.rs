//! Which environments are paired, whether they are reachable, and what they can run.
//!
//! # Why there is no top-level agent list
//!
//! Which agents exist is **per environment, not global** (`docs/api-design.md`
//! § `GET /api/v1/environments`), so a merged list would be true of nowhere. A user
//! picking an environment with no `codex` on it should learn that before creating the
//! session rather than after.
//!
//! Availability reduces to "is the vendor CLI on that machine's `PATH`", but **"codex is
//! not installed on this machine" still needs somewhere to be said, and this is that
//! place** (`docs/environments.md` § Health is per environment). Per-environment health
//! also distinguishes a missing CLI from a sleeping laptop from an exhausted sandbox node,
//! which one global boolean cannot.
//!
//! Reporting this is not optional: **zero usable agents is a silent, total failure** —
//! every session reports `attached: false`, the composer's slash menu will not open, and
//! `/` is a dead keystroke three layers from the cause (`AGENTS.md` § Rules that keep
//! costing time).

use axum::{extract::State, Json};
use futures::future::join_all;
use serde::Serialize;
use std::time::Duration;
use sylph_protocol::{AgentInfo, Capability, FlagSchema};
use tokio::time::timeout;

use crate::{error::Result, AppState};

/// One environment, and what it can do right now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// The opaque id minted at pairing, never a hostname: a name in a key means
    /// renaming destroys identity.
    pub id: String,
    /// Display-only and mutable. Never a key, for the same reason.
    pub name: String,
    pub connected: bool,
    /// How this envagent came to exist: `self-installed`, `ssh-bootstrapped`,
    /// `sandbox-provisioned`, or `development`.
    pub origin: String,
    /// **Not decoration.** A non-null value means this environment is reachable only
    /// while the named one relays for it, so **control dies when that machine
    /// leaves**. A UI rendering it identically to a directly-connected environment
    /// would promise AFK continuity it cannot keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable_via: Option<String>,
    /// Which agents this machine can actually run.
    ///
    /// Empty is a real and reportable answer — an envagent with no vendor CLI on its
    /// `PATH` — and a client must render it as "nothing installed here", never as a
    /// loading state.
    pub agents: Vec<AgentAvailability>,
}

/// One agent on one machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAvailability {
    pub name: String,
    /// The CLI's own version, when the adapter probe reported one.
    ///
    /// `Option` because absent is a real state and **never synthesized**: a
    /// fabricated version would make an unsupported install look supported, and a
    /// journal makes every guess permanent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Spawn-time options the adapter declares before a session is opened.
    ///
    /// An empty map means the adapter has no static choices here; session-scoped
    /// catalogues such as Pi's model list arrive from the draft capability surface.
    pub flags: FlagSchema,
    /// Controls available on a live session of this agent.
    pub supports: Vec<Capability>,
    /// Whether this agent's CLI is on that machine's `PATH`.
    ///
    /// One question, one answer, one fix — which is the whole argument for replacing
    /// discovery. When false, `GET /sessions/{id}/capabilities` reports `no_agent`,
    /// which a client caches like a success because no retry installs a CLI. The local
    /// implementation currently lists only PATH-present agents, so this is true for
    /// every emitted row; paired envagents may also report unavailable names.
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentsResponse {
    pub environments: Vec<Environment>,
}

/// GET /api/v1/environments
///
/// # Reporting an install that can run nothing
///
/// The server does not embed an envagent (`AGENTS.md` § Configuration: two roots,
/// two products), so "the machine the server runs on" is not a privileged
/// environment with its own code path. Until pairing lands (step 4), the only
/// environment this can describe is the local one, and what it reports about that
/// one is honest about the seam: an [`crate::adapters::AdapterSource`] that lists nothing
/// yields an environment with no agents, which is exactly the state a client must be able
/// to explain rather than render as empty chrome.
pub async fn list(State(state): State<AppState>) -> Result<Json<EnvironmentsResponse>> {
    let descriptor = state.environment.descriptor();
    let health = state.environment.probe().await;

    // Probe each installed adapter once, concurrently.  The environment list is the
    // only machine-level catalogue, so it must carry the same spawn-time metadata the
    // draft UI uses for defaults.  A probe failure does not hide a PATH-present agent:
    // opening a session will report the classified handshake failure, while this list
    // still tells the user which CLI was found.  The timeout prevents a wedged vendor
    // binary from hanging the environments picker indefinitely.
    let probes = state.adapters.available().into_iter().map(|name| {
        let adapters = state.adapters.clone();
        async move {
            let fallback = || AgentAvailability {
                name: name.clone(),
                version: None,
                flags: FlagSchema::new(),
                supports: Vec::new(),
                available: true,
            };
            let Some(adapter) = adapters.for_agent(&name) else {
                return fallback();
            };
            match timeout(Duration::from_secs(10), adapter.probe()).await {
                Ok(Ok(info)) => AgentAvailability::from(info),
                Ok(Err(error)) => {
                    tracing::debug!(agent = %name, "Agent probe failed while listing environments: {error:#}");
                    fallback()
                }
                Err(_) => {
                    tracing::debug!(agent = %name, "Agent probe timed out while listing environments");
                    fallback()
                }
            }
        }
    });
    let agents = join_all(probes).await;

    Ok(Json(EnvironmentsResponse {
        environments: vec![Environment {
            id: descriptor.id.clone(),
            name: descriptor.label.clone(),
            connected: health.available,
            origin: "development".to_string(),
            // Directly connected: nothing relays for it.
            reachable_via: None,
            agents,
        }],
    }))
}

impl From<AgentInfo> for AgentAvailability {
    fn from(info: AgentInfo) -> Self {
        Self {
            name: info.name,
            version: info.version,
            flags: info.flags,
            supports: info.supports,
            available: true,
        }
    }
}
