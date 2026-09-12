//! Which environments are paired, whether they are reachable, and what they can run.
//!
//! # Why there is no top-level agent list
//!
//! Which agents exist is **per environment, not global** (`docs/api/environments.md`
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

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sylph_protocol::{AgentInfo, Capability, EnvironmentId, FlagSchema};

use crate::middleware::auth::SubjectId;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// How this envagent came to exist: `self-installed`, `ssh-bootstrapped`,
    /// `sandbox-provisioned`, or `development`.
    pub origin: String,
    /// Machine facts reported by the owning envagent, including OS user and home.
    pub facts: std::collections::BTreeMap<String, String>,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    pub code: String,
    #[serde(default = "default_pair_name")]
    pub name: String,
    #[serde(default = "default_pair_origin")]
    pub origin: String,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
}

fn default_pair_name() -> String {
    "Environment".to_string()
}

fn default_pair_origin() -> String {
    "self-installed".to_string()
}

/// POST /api/v1/environments/pairings
pub async fn create_pairing(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<crate::env::PairingOffer>)> {
    let offer = state
        .env_bridge
        .create_pairing()
        .await
        .map_err(|error| crate::error::Error::Internal(format!("{error:#}")))?;
    Ok((StatusCode::CREATED, Json(offer)))
}

/// POST /api/v1/environments/pair
///
/// This endpoint accepts only a short-lived pairing code.  It is exempt from
/// the browser cookie gate because the envagent has no browser session; the
/// code is single-use and the server remains loopback-bound.
pub async fn pair(
    State(state): State<AppState>,
    Json(request): Json<PairRequest>,
) -> Result<(StatusCode, Json<crate::env::PairingGrant>)> {
    let grant = state
        .env_bridge
        .claim_pairing(&request.code, request.name, request.origin, request.agents)
        .await
        .map_err(|error| crate::error::Error::InvalidRequest(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(grant)))
}

/// DELETE /api/v1/environments/{id}
pub async fn unpair(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let id = EnvironmentId::parse(id)
        .map_err(|error| crate::error::Error::InvalidRequest(error.to_string()))?;
    let authority = state
        .store
        .grant_authority(id.as_str())
        .await?
        .ok_or(crate::error::Error::SessionNotFound)?;
    let allowed = authority == "server" && state.store.is_admin(&subject.0).await?;
    if !allowed {
        return Err(crate::error::Error::Forbidden(
            "environment authority required".into(),
        ));
    }
    if !state
        .env_bridge
        .unpair(&id)
        .await
        .map_err(|error| crate::error::Error::Internal(format!("{error:#}")))?
    {
        return Err(crate::error::Error::SessionNotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke_grant(
    State(state): State<AppState>,
    Extension(subject): Extension<SubjectId>,
    Path((id, target_subject)): Path<(String, String)>,
) -> Result<StatusCode> {
    let environment =
        EnvironmentId::parse(id).map_err(|e| crate::error::Error::InvalidRequest(e.to_string()))?;
    if !state.store.is_admin(&subject.0).await? {
        return Err(crate::error::Error::Forbidden(
            "environment authority required".into(),
        ));
    }
    let mut revoked = false;
    for capability in ["observe", "control"] {
        revoked |= state
            .store
            .revoke_grant(&target_subject, environment.as_str(), capability)
            .await?;
    }
    if !revoked {
        return Err(crate::error::Error::SessionNotFound);
    }
    let _ = state
        .store
        .revoke_subject_work(&target_subject, environment.as_str())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/v1/environments
///
/// # Reporting an install that can run nothing
///
/// The server does not embed an envagent (`AGENTS.md` § Configuration: two roots,
/// two products), so "the machine the server runs on" is not a privileged
/// environment with its own code path. Until pairing lands (step 4), the only
/// environment this can describe is the local one, and what it reports about that
/// one is honest about the environment inventory: an envagent that lists no agents
/// yields an environment with no agents, which is exactly the state a client must be able
/// to explain rather than render as empty chrome.
pub async fn list(State(state): State<AppState>) -> Result<Json<EnvironmentsResponse>> {
    let mut environments = Vec::new();

    for snapshot in state
        .env_bridge
        .list_environments()
        .await
        .map_err(|error| crate::error::Error::Internal(format!("{error:#}")))?
    {
        environments.push(Environment {
            id: snapshot.id.to_string(),
            name: snapshot.name,
            connected: snapshot.connected,
            reason: (!snapshot.connected).then(|| "envagent disconnected".to_string()),
            origin: snapshot.origin,
            facts: snapshot.facts,
            reachable_via: snapshot.reachable_via,
            agents: snapshot
                .agents
                .into_iter()
                .map(AgentAvailability::from)
                .collect(),
        });
    }

    Ok(Json(EnvironmentsResponse { environments }))
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

impl From<sylph_protocol::AgentInventory> for AgentAvailability {
    fn from(inventory: sylph_protocol::AgentInventory) -> Self {
        let info = inventory.info;
        Self {
            name: inventory.name,
            version: inventory
                .version
                .or_else(|| info.as_ref().and_then(|i| i.version.clone())),
            flags: info.as_ref().map(|i| i.flags.clone()).unwrap_or_default(),
            supports: info
                .as_ref()
                .map(|i| i.supports.clone())
                .unwrap_or_default(),
            available: inventory.available,
        }
    }
}
