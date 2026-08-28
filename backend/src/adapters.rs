//! Which agents this machine can run.
//!
//! # This is a seam standing in for `crates/adapters`
//!
//! `docs/roadmap.md` § Implementation order puts the adapter implementations in step 2,
//! which is being written concurrently with this one. The contract agreed between the two
//! is exactly:
//!
//! ```ignore
//! sylph_adapters::registry() -> Vec<Box<dyn Adapter>>
//! sylph_adapters::for_agent(name: &str) -> Option<Box<dyn Adapter>>
//! ```
//!
//! [`registry`] and [`for_agent`] below have that signature, so adopting the real crate
//! is adding it to `[workspace] members` and `backend/Cargo.toml` — neither of which this
//! agent owns — and replacing those two bodies with a delegation.
//!
//! # Availability has exactly one source, and that is the point
//!
//! [`available`] answers "which agents can this machine run", and it answers it by
//! looking **where the link path looks** — the adapter roots, for a directory holding an
//! executable `adapter.sh`. That coupling is deliberate and is the half of the deleted
//! discovery registry worth keeping.
//!
//! `AGENTS.md` records what two sources of truth cost here: discovery searched several
//! roots while `LinkManager` re-derived the path itself, so an agent found in one root
//! was **listed as usable while every link to it failed** naming the other. The
//! inverse is just as bad and is what a hardcoded list would produce — a validation
//! gate refusing an agent the link path can perfectly well start. One component knows
//! where adapters are; every caller asks it.
//!
//! So this module does not hold a literal list of the three supported agents. It holds
//! the *question*, and the answer comes from the filesystem until it comes from a linked
//! crate.
//!
//! # What replaced discovery, and what did not
//!
//! Discovery asked "is there an adapter for this name" and answered with a handshake, a
//! shadowing rule, and a `broken` map. Availability now reduces to one question with one
//! answer and an error message that can state it.
//!
//! What must **not** be lost with it is the reporting. Zero usable agents was a silent,
//! total failure: no session could be created, every existing one reported
//! `attached: false`, and the composer's slash menu made `/` a dead keystroke — three
//! layers from the cause, logged at the same level as success. That is why
//! [`crate::api::environments`] reports availability **per environment**: "codex is not
//! installed on this laptop" is a fact about one machine, and a merged global list would
//! be true of nowhere.

use std::path::{Path, PathBuf};

use sylph_protocol::Adapter;

/// The agents runnable from `roots`, sorted.
///
/// A directory holding `adapter.sh` is an agent. Roots are searched in order and the
/// first hit wins, so a user-installed adapter shadows a bundled one of the same name
/// rather than duplicating it.
///
/// Sorted because callers render it and directory iteration order is not stable — an
/// unsorted list churned between calls in `/diagnostics`.
///
/// **This is the only producer.** `/health`, `/diagnostics`, `GET /environments`, and the
/// validation gate on `POST /sessions` all read it, so none of them can disagree with
/// the link path about which agents exist.
pub fn available(roots: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            // A root that does not exist is not an error: an install may ship bundled
            // adapters and no user directory, or the reverse.
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join("adapter.sh").is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names
}

/// Where an `adapter.sh` for one agent is, searching `roots` in order.
///
/// Shared with [`available`] so the two cannot disagree: an agent this returns a path
/// for is exactly an agent that function lists.
pub fn adapter_path(roots: &[PathBuf], agent: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(agent).join("adapter.sh"))
        .find(|candidate| candidate.is_file())
}

/// Every adapter this build links against.
///
/// Empty until `crates/adapters` lands. Availability during the bridge is answered by
/// [`available`], which reads the filesystem the link path reads — see the module header
/// for why those must be one source rather than two.
pub fn registry() -> Vec<Box<dyn Adapter>> {
    Vec::new()
}

/// The adapter for one agent, or `None` when this build has none.
///
/// `None` is `UnavailableReason::NoAgent` (`sylph_protocol::UnavailableReason`): an
/// install, not a retry. That distinction is why the reason is typed — a client caches an
/// unretryable reason like a success, while a failed handshake must go stale.
pub fn for_agent(_name: &str) -> Option<Box<dyn Adapter>> {
    None
}

/// Whether a path names a runnable agent directory. Used by tests and `doctor`.
pub fn is_agent_dir(path: &Path) -> bool {
    path.is_dir() && path.join("adapter.sh").is_file()
}
