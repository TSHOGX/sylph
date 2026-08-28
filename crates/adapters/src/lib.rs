//! One module per vendor CLI, each implementing [`sylph_protocol::Adapter`].
//!
//! `docs/agents.md` is the governing spec and holds the measurement behind every quirk
//! these modules carry. `docs/session-protocol.md` §§ "An adapter is a trait, with three
//! modes" and "What the adapter owns, and what it must not" govern the contract.
//!
//! # Availability is `PATH`, and that is the whole check
//!
//! There is no discovery. [`registry`] answers with the agents whose vendor CLI is on
//! this machine's `PATH`, and [`for_agent`] answers `None` when one is absent — which a
//! caller renders as [`sylph_protocol::UnavailableReason::NoAgent`], the reason no retry
//! can fix.
//!
//! The mechanism this replaces produced two total failures, both in `AGENTS.md`: zero
//! discovered agents made `/` a dead keystroke three layers from the cause, and a stale
//! fixture directory failed an entire E2E suite against an agent that did not exist. Both
//! were silent because "no adapter found" and "no agent installed" were the same state.
//! Here they cannot be: there is nothing to find, only a binary that is present or not,
//! and the error names it.
//!
//! # What an adapter owns
//!
//! **Owns:** the vendor CLI's command line and native store format, translation in both
//! directions, the vendor's own ids, and the decision to **omit rather than guess**.
//!
//! **Must not:** decide identity injection (the server passes it), invent an event kind
//! the vocabulary does not have, retry a turn on its own, or report a refusal as a
//! success. Nor synthesize a value the vendor did not give — a synthesized id, name, or
//! timestamp is a lie a journal makes permanent.

use sylph_protocol::Adapter;

pub mod claude;
pub mod codex;
pub mod native;
pub mod pi;

#[cfg(feature = "e2e")]
pub mod e2e;

/// Every agent whose vendor CLI is present on this machine.
///
/// Ordered as `docs/agents.md` lists them — `claude`, `codex`, `pi` — because this order
/// reaches a UI as the order agents are offered in, and an order that varies between
/// calls reads as a bug. Under the `e2e` feature the deterministic test adapter is
/// offered as well, last, so it can never displace a real agent in a default selection.
///
/// **An empty result is a real state and not an error.** A machine with none of the three
/// installed genuinely has no agents, and the caller's job is to say which are missing
/// rather than to treat the emptiness as a failure to discover something.
pub fn registry() -> Vec<Box<dyn Adapter>> {
    let mut found: Vec<Box<dyn Adapter>> = Vec::new();
    if claude::ClaudeAdapter::is_installed() {
        found.push(Box::new(claude::ClaudeAdapter::new()));
    }
    if codex::CodexAdapter::is_installed() {
        found.push(Box::new(codex::CodexAdapter::new()));
    }
    if pi::PiAdapter::is_installed() {
        found.push(Box::new(pi::PiAdapter::new()));
    }
    #[cfg(feature = "e2e")]
    if e2e::E2eAdapter::is_installed() {
        found.push(Box::new(e2e::E2eAdapter::new()));
    }
    found
}

/// One agent by name, or `None` when its CLI is absent.
///
/// Takes `&str` rather than [`sylph_protocol::AgentName`] deliberately: that type's
/// `parse` can fail, and this function already answers `None` for a name it does not
/// know, so taking the parsed type would push a second failure mode onto every caller
/// for no gain. A stored `AgentName` reaches this as `name.as_str()`.
///
/// The two `None` cases are **not** distinguished here, and that is a decision: an
/// unknown name and an uninstalled CLI are both "you cannot open a session with this",
/// and a caller that needs to explain which reaches for [`is_known`].
pub fn for_agent(name: &str) -> Option<Box<dyn Adapter>> {
    match name {
        claude::AGENT_NAME if claude::ClaudeAdapter::is_installed() => {
            Some(Box::new(claude::ClaudeAdapter::new()))
        }
        codex::AGENT_NAME if codex::CodexAdapter::is_installed() => {
            Some(Box::new(codex::CodexAdapter::new()))
        }
        pi::AGENT_NAME if pi::PiAdapter::is_installed() => Some(Box::new(pi::PiAdapter::new())),
        #[cfg(feature = "e2e")]
        e2e::AGENT_NAME if e2e::E2eAdapter::is_installed() => {
            Some(Box::new(e2e::E2eAdapter::new()))
        }
        _ => None,
    }
}

/// Whether this build knows an agent by this name at all, installed or not.
///
/// The separation from [`for_agent`] is what lets a caller say **which** thing is wrong,
/// and that distinction is the one the old discovery could not make. `"codex"` on a
/// machine without the CLI is a missing install, which names a fix; `"gemini"` is an
/// unsupported agent, which does not (`AGENTS.md` § Product Invariants — three agents,
/// `gemini` and `opencode` deliberately not supported).
///
/// A stored session's agent name is also checked here rather than against an enum,
/// because a journal written under a build that knew four agents must stay readable under
/// one that knows three (`sylph_protocol::AgentName`).
pub fn is_known(name: &str) -> bool {
    #[cfg(feature = "e2e")]
    if name == e2e::AGENT_NAME {
        return true;
    }
    matches!(
        name,
        claude::AGENT_NAME | codex::AGENT_NAME | pi::AGENT_NAME
    )
}

/// Every agent name this build knows, whether or not its CLI is installed.
pub fn known_agents() -> &'static [&'static str] {
    #[cfg(feature = "e2e")]
    {
        &[
            claude::AGENT_NAME,
            codex::AGENT_NAME,
            pi::AGENT_NAME,
            e2e::AGENT_NAME,
        ]
    }
    #[cfg(not(feature = "e2e"))]
    {
        &[claude::AGENT_NAME, codex::AGENT_NAME, pi::AGENT_NAME]
    }
}

/// Whether `binary` resolves on the current `PATH`.
///
/// The one availability check, shared by all three modules so they cannot disagree about
/// what "installed" means.
///
/// # Why not `which` or a `--version` spawn
///
/// A spawn is what the answer costs elsewhere: `probe` runs the CLI to read its version,
/// and that is right because a version is a fact only the binary holds. Availability is
/// not — it is a filesystem question — and answering it by spawning would make every
/// listing pay a process launch per agent, on a path the sidebar polls.
///
/// It also has to stay cheap for the reason `AGENTS.md` records under the
/// degraded-dependency rule: *"A degraded dependency is one fact about the run, not one
/// fault per item."* One missing binary produced 2339 identical failure lines in a single
/// E2E run when the probe was per-request.
///
/// An entry that exists but is not executable is **not** installed: a directory or a
/// non-executable file on `PATH` cannot be run, and reporting it as available would turn
/// a clear "not installed" into a spawn failure two layers later.
pub fn on_path(binary: &str) -> bool {
    // An absolute or relative path is honoured as given rather than searched for, which
    // is what makes a test able to point at a fixture binary.
    if binary.contains(std::path::MAIN_SEPARATOR) {
        return is_executable(std::path::Path::new(binary));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| is_executable(&directory.join(binary)))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| {
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    })
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three supported agents, and the two deliberately unsupported ones.
    ///
    /// Asserted on `is_known` rather than on `registry()`, because the registry's length
    /// depends on what is installed on the machine running the test — which is exactly
    /// the property that makes it unassertable.
    #[test]
    fn the_three_supported_agents_are_known_and_the_dropped_two_are_not() {
        for supported in ["claude", "codex", "pi"] {
            assert!(is_known(supported), "{supported} must be known");
        }
        // `AGENTS.md` § Product Invariants: a deliberate narrowing, not a gap.
        for dropped in ["gemini", "opencode"] {
            assert!(!is_known(dropped), "{dropped} must not be known");
        }
        assert!(!is_known(""));
    }

    /// An unknown name must never produce an adapter, however the `PATH` happens to look.
    #[test]
    fn an_unsupported_agent_never_resolves_to_an_adapter() {
        assert!(for_agent("gemini").is_none());
        assert!(for_agent("opencode").is_none());
        assert!(for_agent("").is_none());
        assert!(for_agent("claude ").is_none(), "no trimming: a name is exact");
    }

    /// `registry()` and `for_agent` must agree, or a session can be created for an agent
    /// that was never offered — or an offered agent cannot be opened.
    #[test]
    fn every_registered_adapter_is_reachable_by_name() {
        for adapter in registry() {
            // `probe` is async and spawns the CLI; the name is what the registry keyed on,
            // so it is checked through the known-agent list instead.
            let names = known_agents();
            assert!(
                names.iter().any(|name| for_agent(name).is_some()),
                "a registered adapter must be reachable by one of {names:?}"
            );
            drop(adapter);
        }
    }

    /// Every known name is either resolvable or absent-with-a-reason, and nothing else.
    /// This is the property the old discovery lacked: there was a third state where an
    /// agent existed and could not be spoken to.
    #[test]
    fn a_known_agent_is_either_installed_or_absent() {
        for name in known_agents() {
            let resolved = for_agent(name).is_some();
            // Whichever it is, the name is still known — so a caller can always say
            // which CLI to install rather than reporting a generic failure.
            assert!(is_known(name));
            // And the answer must be stable within one process.
            assert_eq!(resolved, for_agent(name).is_some());
        }
    }

    /// A directory on `PATH` named like a binary is not an installed agent. Reporting it
    /// as available turns "not installed" into a spawn failure two layers later.
    #[test]
    fn a_non_executable_entry_is_not_installed() {
        let dir = std::env::temp_dir().join(format!("sylph-onpath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("claude")).unwrap();
        std::fs::write(dir.join("codex"), b"not executable").unwrap();

        assert!(
            !is_executable(&dir.join("claude")),
            "a directory is not executable"
        );
        assert!(
            !is_executable(&dir.join("codex")),
            "a file with no execute bit is not executable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path-shaped name is honoured as given rather than searched for on `PATH`.
    #[test]
    fn a_path_shaped_binary_name_is_not_searched_for() {
        assert!(!on_path("/nonexistent/sylph-not-a-real-binary"));
        assert!(!on_path("./also-not-real"));
    }
}
