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

pub mod bridge;
pub mod registry;

pub use bridge::{DispatchResult, EnvBridge};
pub use registry::{
    EnvironmentRegistry, EnvironmentSnapshot, PairingGrant, PairingOffer, RegistryError,
};

/// Hash a bearer invite code exactly as the bridge does before persistence.
pub(crate) fn hash_invite_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    hex::encode(hasher.finalize())
}
