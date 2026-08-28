//! Which agents this machine can run.
//!
//! # The seam is closed, and half of it is
//!
//! This module used to stand in for `crates/adapters` with a filesystem search for
//! `adapter.sh`. That crate now exists and is linked in, so [`available`], [`registry`],
//! [`for_agent`], and [`is_known`] delegate to it and the search is gone: availability is
//! whether a vendor CLI is on this machine's `PATH`, which is one question with one answer
//! and an error message that can state it.
//!
//! **[`adapter_path`] survives, and it is the honest reason this module still exists.**
//! `crates/adapters` makes an adapter a Rust trait, but the server's *live* path does not
//! yet call it: `adapter::Link::establish` spawns an `adapter.sh` subprocess and speaks
//! NDJSON to it, which is step 3 of `docs/roadmap.md` § Implementation order — in-process
//! adapter calls. Until that lands, a live session still needs a path, so the two halves
//! resolve differently and this module is where that split is visible rather than spread
//! across call sites.
//!
//! ## The disagreement that split creates, stated plainly
//!
//! [`available`] answers from `PATH` and [`adapter_path`] answers from the adapter roots,
//! so they can disagree — and `AGENTS.md` records exactly that shape costing a release:
//! discovery listed an agent as usable while every link to it failed naming another root.
//! The direction here is the safe one and the reason is worth stating, because it is not
//! obvious:
//!
//! - An agent on `PATH` with no `adapter.sh` is **listed and cannot be linked**. That is
//!   the pre-existing state of this repo's own tree — the `adapters/` directory ships
//!   `claude` and `pi` and no `codex` — and it fails at `POST /sessions` with an error
//!   naming every root tried.
//! - An agent with an `adapter.sh` and no CLI on `PATH` is **not listed**, which is
//!   correct: there is nothing to run.
//!
//! Both halves collapse into one when step 3 replaces `Link` with `Box<dyn Session>`,
//! because a trait object has no path to get wrong. Nothing new should be built on
//! [`adapter_path`].
//!
//! # Availability is one fact about the machine, not one fault per caller
//!
//! `sylph_adapters::on_path` is a filesystem check rather than a spawn, deliberately: one
//! missing binary produced 2339 identical failure lines in a single E2E run when the probe
//! was per-request. `/health`, `/diagnostics`, `GET /environments`, and the validation gate
//! on `POST /sessions` all read [`available`], so none of them can disagree about which
//! agents exist.
//!
//! What must **not** be lost with discovery is the reporting. Zero usable agents was a
//! silent, total failure: no session could be created, every existing one reported
//! `attached: false`, and the composer's slash menu made `/` a dead keystroke — three
//! layers from the cause, logged at the same level as success. That is why
//! [`crate::api::environments`] reports availability **per environment**: "codex is not
//! installed on this laptop" is a fact about one machine, and a merged global list would be
//! true of nowhere.

use std::path::{Path, PathBuf};

use sylph_protocol::Adapter;

/// The agents this machine can run, sorted.
///
/// # Two start mechanisms exist right now, so this reports both
///
/// This is the measured shape of the half-finished migration, and collapsing it to one
/// source would break something either way:
///
/// - An agent whose **vendor CLI is on `PATH`** can be run by `crates/adapters`, which is
///   what [`registry`] and [`for_agent`] answer with.
/// - An agent with an **`adapter.sh`** in one of `roots` can be run by
///   `adapter::Link::establish`, which is what the *live* path still does until step 3 of
///   `docs/roadmap.md` § Implementation order.
///
/// Reporting only `PATH` names agents the live path cannot start, and hides the two fixture
/// agents that have no CLI at all — `backend/tests/common`'s `mock` and the E2E suite's
/// `e2e` — so a session cannot be created in either. That is not hypothetical: it fails 16
/// integration tests and every E2E spec, with the error naming an unknown agent. Reporting
/// only `roots` is what this module did before and it cannot see a linked-in adapter,
/// which is the whole point of the crate now being here.
///
/// So the union is the truthful answer while both mechanisms exist, and it is **not**
/// compatibility code for an absent caller: both paths are live and reachable today. It
/// collapses to `PATH` alone the moment `Link` becomes `Box<dyn Session>`, because then
/// there is one way to start an agent and [`adapter_path`] has no callers.
///
/// Sorted and de-duplicated because callers render it and an order that varies between
/// calls reads as a bug — `/diagnostics` churned on it when the answer came from directory
/// iteration.
///
/// **An empty result is a real state and not an error.** A machine with none of the three
/// installed and no fixtures genuinely has no agents, and the caller's job is to say which
/// are missing.
pub fn available(roots: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = sylph_adapters::known_agents()
        .iter()
        .filter(|name| sylph_adapters::for_agent(name).is_some())
        .map(|name| name.to_string())
        .collect();

    // A directory holding `adapter.sh` is an agent the live path can start. Roots are
    // searched in order and a name already present is not added twice, so a user-installed
    // adapter shadows a bundled one of the same name rather than duplicating it.
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            // A root that does not exist is not an error: an install may ship bundled
            // adapters and no user directory, or the reverse.
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_agent_dir(&path) {
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
/// **Transitional.** See the module header: the live path still spawns a subprocess, so it
/// still needs a path. This is the last consumer of the adapter roots and it goes with
/// `adapter::Link` in step 3.
///
/// Deliberately **not** reconciled with [`available`]: making this refuse an agent whose
/// CLI is absent would hide the real failure behind a second one, and making [`available`]
/// require an `adapter.sh` would report `codex` as uninstallable on a machine that has it.
/// The mismatch is a property of the half-finished migration, and it is better visible here
/// than papered over.
pub fn adapter_path(roots: &[PathBuf], agent: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(agent).join("adapter.sh"))
        .find(|candidate| candidate.is_file())
}

/// Every adapter this build links against, for the agents installed here.
pub fn registry() -> Vec<Box<dyn Adapter>> {
    sylph_adapters::registry()
}

/// The adapter for one agent, or `None` when this machine cannot run it.
///
/// `None` is `UnavailableReason::NoAgent` (`sylph_protocol::UnavailableReason`): an install,
/// not a retry. That distinction is why the reason is typed — a client caches an
/// unretryable reason like a success, while a failed handshake must go stale.
pub fn for_agent(name: &str) -> Option<Box<dyn Adapter>> {
    sylph_adapters::for_agent(name)
}

/// Whether this build knows an agent by this name at all, installed or not.
///
/// The separation from [`for_agent`] is what lets a caller say **which** thing is wrong, and
/// that distinction is the one discovery could not make: `"codex"` on a machine without the
/// CLI is a missing install, which names a fix, while `"gemini"` is an unsupported agent,
/// which does not.
///
/// A stored session's agent name is checked here rather than against an enum, because a
/// journal written under a build that knew four agents must stay readable under one that
/// knows three.
pub fn is_known(name: &str) -> bool {
    sylph_adapters::is_known(name)
}

/// Whether a path names a runnable agent directory. Used by tests and `doctor`.
///
/// Transitional with [`adapter_path`], for the same reason.
pub fn is_agent_dir(path: &Path) -> bool {
    path.is_dir() && path.join("adapter.sh").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three supported agents are known, and the two dropped ones are not.
    ///
    /// Asserted on `is_known` rather than on [`available`], because what is *installed*
    /// depends on the machine running the test — which is exactly the property that makes
    /// it unassertable.
    #[test]
    fn the_supported_agents_are_known_and_the_dropped_two_are_not() {
        for supported in ["claude", "codex", "pi"] {
            assert!(is_known(supported), "{supported} must be known");
        }
        // `AGENTS.md` § Product Invariants: a deliberate narrowing, not a gap.
        for dropped in ["gemini", "opencode"] {
            assert!(!is_known(dropped), "{dropped} must not be known");
        }
    }

    /// An absent root contributes nothing and costs nothing.
    ///
    /// This is what the old implementation could not do: it read *only* those roots, so an
    /// empty one meant zero agents and the total silent failure that produced — no session
    /// creatable, every session detached, and `/` a dead keystroke three layers away.
    #[test]
    fn an_absent_adapter_root_does_not_reduce_availability() {
        let nowhere = vec![PathBuf::from("/nonexistent/sylph-adapter-roots")];
        assert_eq!(available(&nowhere), available(&[]));
    }

    /// **A fixture agent with no vendor CLI is still runnable**, because the live path
    /// starts an `adapter.sh` rather than a binary on `PATH`.
    ///
    /// This is the property a `PATH`-only answer loses, and losing it fails 16 integration
    /// tests and every E2E spec with an error naming an unknown agent — a fixture problem
    /// that reads as a product one.
    #[test]
    fn an_adapter_directory_with_no_vendor_cli_is_still_available() {
        let root = std::env::temp_dir().join(format!("sylph-seam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("fixture-agent")).unwrap();
        std::fs::write(root.join("fixture-agent").join("adapter.sh"), b"#!/bin/sh\n").unwrap();

        let roots = vec![root.clone()];
        assert!(
            available(&roots).iter().any(|name| name == "fixture-agent"),
            "an agent the live path can start must be listed"
        );
        // And it is reachable by the path the live link resolves, or the gate would admit
        // an agent the link cannot start.
        assert!(adapter_path(&roots, "fixture-agent").is_some());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory with no `adapter.sh` is not an agent, so an unrelated directory in the
    /// root cannot become one.
    #[test]
    fn a_directory_without_an_adapter_script_is_not_an_agent() {
        let root = std::env::temp_dir().join(format!("sylph-seam-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("not-an-agent")).unwrap();

        assert!(!available(std::slice::from_ref(&root))
            .iter()
            .any(|name| name == "not-an-agent"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every agent listed from `PATH` alone resolves to a linked-in adapter.
    ///
    /// Scoped to the empty-roots case deliberately: with roots, a listed name may be a
    /// fixture `adapter.sh` that this build has no `Adapter` for, which is the union
    /// documented on [`available`]. Asserting it over both would be asserting the
    /// migration is finished.
    #[test]
    fn every_agent_available_from_path_resolves_to_an_adapter() {
        for name in available(&[]) {
            assert!(
                for_agent(&name).is_some(),
                "{name} is listed but has no adapter"
            );
            assert!(is_known(&name));
        }
    }

    /// An unknown name never produces an adapter, however the `PATH` happens to look.
    #[test]
    fn an_unsupported_agent_never_resolves_to_an_adapter() {
        assert!(for_agent("gemini").is_none());
        assert!(for_agent("").is_none());
    }

    /// A path lookup answers `None` rather than a path that is not there.
    #[test]
    fn an_absent_adapter_directory_yields_no_path() {
        let roots = vec![PathBuf::from("/nonexistent/sylph-adapter-roots")];
        assert!(adapter_path(&roots, "claude").is_none());
    }
}
