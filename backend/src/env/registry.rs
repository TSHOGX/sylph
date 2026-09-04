//! Server-side registry for paired envagent installations.
//!
//! The registry deliberately contains no adapter or filesystem logic. It owns
//! pairing credentials, connection liveness, inventory snapshots, and the
//! single active lease per environment. Event ingestion is implemented by the
//! bridge on top of this registry so the server remains the authority for
//! canonical journal sequences.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sylph_protocol::{AgentInfo, EnvironmentId};
use tokio::sync::RwLock;
use uuid::Uuid;

/// A short-lived code shown to an operator before an envagent is paired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PairingOffer {
    pub code: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PairingGrant {
    pub environment_id: EnvironmentId,
    pub installation_id: String,
    pub credential: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub id: EnvironmentId,
    pub name: String,
    pub origin: String,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable_via: Option<String>,
    pub connection_epoch: u64,
    pub agents: Vec<AgentInfo>,
    pub last_seen_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct PairingState {
    expires_at_ms: i64,
}

#[derive(Debug, Clone)]
struct Installation {
    id: EnvironmentId,
    name: String,
    origin: String,
    reachable_via: Option<String>,
    token: String,
    connection_epoch: u64,
    agents: Vec<AgentInfo>,
    connected: bool,
    last_seen_at_ms: Option<i64>,
}

#[derive(Default)]
struct RegistryState {
    pairings: HashMap<String, PairingState>,
    installations: HashMap<EnvironmentId, Installation>,
}

#[derive(Clone, Default)]
pub struct EnvironmentRegistry {
    state: Arc<RwLock<RegistryState>>,
    pairing_ttl: Duration,
}

impl EnvironmentRegistry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(RegistryState::default())),
            pairing_ttl: Duration::from_secs(5 * 60),
        }
    }

    pub fn with_pairing_ttl(mut self, ttl: Duration) -> Self {
        self.pairing_ttl = ttl.max(Duration::from_secs(1));
        self
    }

    /// Restore an installation loaded from the server journal. The token is
    /// held only in memory for this process; durable authentication remains
    /// the hashed value in the journal.
    pub async fn register_existing(
        &self,
        id: EnvironmentId,
        name: impl Into<String>,
        origin: impl Into<String>,
        token: String,
        agents: Vec<AgentInfo>,
    ) {
        self.state.write().await.installations.insert(
            id.clone(),
            Installation {
                id,
                name: name.into(),
                origin: origin.into(),
                reachable_via: None,
                token,
                connection_epoch: 0,
                agents,
                connected: false,
                last_seen_at_ms: None,
            },
        );
    }

    /// Mark a connection trusted after the bridge has authenticated it
    /// against the durable token hash.
    pub async fn connect_trusted(
        &self,
        environment_id: &EnvironmentId,
        epoch: u64,
        agents: Vec<AgentInfo>,
    ) {
        if let Some(installation) = self
            .state
            .write()
            .await
            .installations
            .get_mut(environment_id)
        {
            installation.connection_epoch = epoch;
            installation.connected = true;
            installation.last_seen_at_ms = Some(Self::now_ms());
            installation.agents = agents;
        }
    }

    pub fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Mint a one-shot pairing code. The code itself is never used as an
    /// environment identity or a durable credential.
    pub async fn create_pairing(&self) -> PairingOffer {
        let code = format!("pair-{}", Uuid::new_v4().simple());
        let expires_at_ms = Self::now_ms() + self.pairing_ttl.as_millis() as i64;
        self.state
            .write()
            .await
            .pairings
            .insert(code.clone(), PairingState { expires_at_ms });
        PairingOffer {
            code,
            expires_at_ms,
        }
    }

    pub async fn discard_pairing(&self, code: &str) {
        self.state.write().await.pairings.remove(code);
    }

    /// Consume a pairing code and issue an opaque environment id plus a
    /// durable bearer credential.
    pub async fn claim_pairing(
        &self,
        code: &str,
        name: impl Into<String>,
        origin: impl Into<String>,
        agents: Vec<AgentInfo>,
    ) -> Result<PairingGrant, RegistryError> {
        let now = Self::now_ms();
        let mut state = self.state.write().await;
        let Some(pairing) = state.pairings.remove(code) else {
            return Err(RegistryError::InvalidPairing);
        };
        if pairing.expires_at_ms <= now {
            return Err(RegistryError::ExpiredPairing);
        }
        let environment_id = mint_environment_id(&state).map_err(|_| RegistryError::IdExhausted)?;
        let credential = format!("envtok-{}", Uuid::new_v4().simple());
        let installation_id = format!("install-{}", Uuid::new_v4().simple());
        state.installations.insert(
            environment_id.clone(),
            Installation {
                id: environment_id.clone(),
                name: name.into(),
                origin: origin.into(),
                reachable_via: None,
                token: credential.clone(),
                connection_epoch: 0,
                agents,
                connected: false,
                last_seen_at_ms: None,
            },
        );
        Ok(PairingGrant {
            environment_id,
            installation_id,
            credential,
            expires_at_ms: now + 365 * 24 * 60 * 60 * 1000,
        })
    }

    /// Lease the one active connection for an installation.
    pub async fn connect(
        &self,
        environment_id: &EnvironmentId,
        token: &str,
        agents: Vec<AgentInfo>,
        name: Option<String>,
        origin: Option<String>,
        reachable_via: Option<String>,
    ) -> Result<u64, RegistryError> {
        let mut state = self.state.write().await;
        let Some(installation) = state.installations.get_mut(environment_id) else {
            return Err(RegistryError::UnknownEnvironment);
        };
        if installation.token != token {
            return Err(RegistryError::AuthenticationFailed);
        }
        installation.connection_epoch = installation.connection_epoch.saturating_add(1);
        installation.connected = true;
        installation.last_seen_at_ms = Some(Self::now_ms());
        installation.agents = agents;
        if let Some(name) = name {
            installation.name = name;
        }
        if let Some(origin) = origin {
            installation.origin = origin;
        }
        installation.reachable_via = reachable_via;
        Ok(installation.connection_epoch)
    }

    pub async fn touch(
        &self,
        environment_id: &EnvironmentId,
        epoch: u64,
    ) -> Result<(), RegistryError> {
        let mut state = self.state.write().await;
        let Some(installation) = state.installations.get_mut(environment_id) else {
            return Err(RegistryError::UnknownEnvironment);
        };
        if installation.connection_epoch != epoch {
            return Err(RegistryError::StaleConnection);
        }
        installation.connected = true;
        installation.last_seen_at_ms = Some(Self::now_ms());
        Ok(())
    }

    pub async fn disconnect(
        &self,
        environment_id: &EnvironmentId,
        epoch: u64,
    ) -> Result<(), RegistryError> {
        let mut state = self.state.write().await;
        let Some(installation) = state.installations.get_mut(environment_id) else {
            return Err(RegistryError::UnknownEnvironment);
        };
        if installation.connection_epoch != epoch {
            return Err(RegistryError::StaleConnection);
        }
        installation.connected = false;
        installation.last_seen_at_ms = Some(Self::now_ms());
        Ok(())
    }

    pub async fn unpair(&self, environment_id: &EnvironmentId) -> Result<bool, RegistryError> {
        Ok(self
            .state
            .write()
            .await
            .installations
            .remove(environment_id)
            .is_some())
    }

    pub async fn get(&self, environment_id: &EnvironmentId) -> Option<EnvironmentSnapshot> {
        self.state
            .read()
            .await
            .installations
            .get(environment_id)
            .map(snapshot)
    }

    pub async fn list(&self) -> Vec<EnvironmentSnapshot> {
        let mut snapshots: Vec<_> = self
            .state
            .read()
            .await
            .installations
            .values()
            .map(snapshot)
            .collect();
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        snapshots
    }

    pub async fn authenticate(&self, environment_id: &EnvironmentId, token: &str) -> bool {
        self.state
            .read()
            .await
            .installations
            .get(environment_id)
            .is_some_and(|installation| installation.token == token)
    }
}

fn snapshot(installation: &Installation) -> EnvironmentSnapshot {
    EnvironmentSnapshot {
        id: installation.id.clone(),
        name: installation.name.clone(),
        origin: installation.origin.clone(),
        connected: installation.connected,
        reachable_via: installation.reachable_via.clone(),
        connection_epoch: installation.connection_epoch,
        agents: installation.agents.clone(),
        last_seen_at_ms: installation.last_seen_at_ms,
    }
}

fn mint_environment_id(state: &RegistryState) -> Result<EnvironmentId, ()> {
    // UUID bytes are encoded into the restricted a-z2-7 alphabet so the id
    // can never collide with the session delimiter or a URL separator.
    for _ in 0..8 {
        let uuid = Uuid::new_v4();
        let id = base32_lower(uuid.as_bytes());
        let Ok(environment_id) = EnvironmentId::parse(id) else {
            continue;
        };
        if !state.installations.contains_key(&environment_id) {
            return Ok(environment_id);
        }
    }
    Err(())
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut accumulator = 0_u16;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((accumulator >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((accumulator << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RegistryError {
    #[error("pairing code is invalid")]
    InvalidPairing,
    #[error("pairing code has expired")]
    ExpiredPairing,
    #[error("could not mint an environment id")]
    IdExhausted,
    #[error("unknown environment")]
    UnknownEnvironment,
    #[error("environment credential is invalid")]
    AuthenticationFailed,
    #[error("connection lease is stale")]
    StaleConnection,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pairing_is_single_use_and_connection_epochs_are_monotonic() {
        let registry = EnvironmentRegistry::new();
        let offer = registry.create_pairing().await;
        let grant = registry
            .claim_pairing(&offer.code, "laptop", "self-installed", Vec::new())
            .await
            .unwrap();
        assert!(registry
            .claim_pairing(&offer.code, "again", "self-installed", Vec::new())
            .await
            .is_err());
        let first = registry
            .connect(
                &grant.environment_id,
                &grant.credential,
                Vec::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let second = registry
            .connect(
                &grant.environment_id,
                &grant.credential,
                Vec::new(),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(second, first + 1);
        assert!(registry.touch(&grant.environment_id, first).await.is_err());
        assert!(registry.touch(&grant.environment_id, second).await.is_ok());
    }
}
