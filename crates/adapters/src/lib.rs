//! One module per vendor CLI, each implementing [`sylph_protocol::Adapter`].
//!
//! `docs/agents.md` is the governing spec and holds the measurement behind every quirk
//! these modules carry. `docs/session-protocol.md` §§ "The acquisition and control contract"
//! and "What the adapter owns, and what it must not" govern the contract.
//!
//! # Availability is `PATH`, and that is the whole check
//!
//! There is no discovery. [`known_agents`] names the three this build supports and
//! [`for_agent`] answers `None` when a vendor CLI is absent from this machine's `PATH` —
//! which a caller renders as [`sylph_protocol::UnavailableReason::NoAgent`], the reason no
//! retry can fix. Availability is therefore the same question as openability, rather than
//! two mechanisms that can disagree.
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

use sylph_protocol::{Adapter, Facet, LinkFacets, UnavailableReason};

pub mod claude;
pub mod codex;
pub mod native;
pub mod pi;
pub mod process;

#[cfg(feature = "e2e")]
pub mod e2e;

/// Epoch milliseconds now.
///
/// `SystemTime` rather than `chrono`, because this crate has no `chrono` and does not need
/// one: every timestamp the protocol carries is an `i64` of epoch millis
/// (`sylph_protocol` § Not a runtime). A pre-1970 clock yields `0` rather than a negative
/// stamp, which is a state nothing here can do anything useful about.
///
/// One copy. It was written out four times — once per adapter plus the fixture,
/// byte-identical — which is four places to look when a stamp is wrong and four to fix.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
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
    // Exclusive rather than additive: under this feature the binary *is* a test fixture,
    // so a real agent resolving here would let a spec spend real tokens. `known_agents`
    // holds the same rule, and `a_fixture_build_offers_only_the_fixture` pins both.
    #[cfg(feature = "e2e")]
    {
        return (name == e2e::AGENT_NAME)
            .then(|| Box::new(e2e::E2eAdapter::new()) as Box<dyn Adapter>);
    }

    #[allow(unreachable_code)]
    match name {
        claude::AGENT_NAME if claude::ClaudeAdapter::is_installed() => {
            Some(Box::new(claude::ClaudeAdapter::new()))
        }
        codex::AGENT_NAME if codex::CodexAdapter::is_installed() => {
            Some(Box::new(codex::CodexAdapter::new()))
        }
        pi::AGENT_NAME if pi::PiAdapter::is_installed() => Some(Box::new(pi::PiAdapter::new())),
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
    known_agents().contains(&name)
}

/// Every agent name this build knows, whether or not its CLI is installed.
pub fn known_agents() -> &'static [&'static str] {
    // Only the fixture under the feature, so `is_known` — which a stored session's agent
    // name is checked against — agrees with what can actually be resolved. Listing all four
    // would report `claude` as known-but-absent in a build that would never run it.
    #[cfg(feature = "e2e")]
    {
        &[e2e::AGENT_NAME]
    }
    #[cfg(not(feature = "e2e"))]
    {
        &[claude::AGENT_NAME, codex::AGENT_NAME, pi::AGENT_NAME]
    }
}

/// The link facets a session read from a native store may claim, per vendor.
///
/// # Why this exists rather than a literal at the call site
///
/// The envagent used to answer this with a hardcoded JSON object for *every* native
/// source, while all three adapters declared the same answer in a
/// `native_link_facets` nothing called. Two derivations of one fact, and the rule
/// (`AGENTS.md` § Rules That Keep Costing Time) is that they diverge the moment
/// either grows a case — which `codex` has already scheduled: its own facets note
/// that an envagent *may replace the control facet once it has authenticated a live
/// app-server socket*, because `codex` is drivable by a second party and the other
/// two are not. On that day a literal meaning "no native source is drivable" is
/// silently wrong for one vendor, and it is wrong in the direction that ships a
/// Stop button which cannot work.
///
/// So the vendor owns the answer and this is the one place that asks. An unknown
/// name gets the conservative facets rather than a panic: a journal written under a
/// build that knew another agent must stay readable here (see [`is_known`]), and
/// "cannot drive it" is the safe reading of a name this build cannot resolve.
pub fn native_link_facets(agent: &str) -> LinkFacets {
    #[cfg(not(feature = "e2e"))]
    match agent {
        claude::AGENT_NAME => claude::ClaudeAdapter::native_link_facets(),
        codex::AGENT_NAME => codex::CodexAdapter::native_link_facets(),
        pi::AGENT_NAME => pi::PiAdapter::native_link_facets(),
        _ => conservative_native_facets(),
    }

    #[cfg(feature = "e2e")]
    {
        let _ = agent;
        conservative_native_facets()
    }
}

/// Observation from the server's own record, and no control.
///
/// The floor every vendor's native answer sits at or above: the store is Sylph's to
/// read, and reaching the process is what a native source cannot assume.
fn conservative_native_facets() -> LinkFacets {
    LinkFacets {
        observe: Facet::available(),
        control: Facet::unavailable(UnavailableReason::NotOursToDrive),
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
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
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
    /// Asserted on `is_known` rather than on what resolves, because which of the three
    /// is *installed* depends on the machine running the test — which is exactly the
    /// property that makes it unassertable.
    #[test]
    #[cfg(not(feature = "e2e"))]
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

    /// Under the `e2e` feature the fixture is the **only** agent, which is the property
    /// that keeps a suite off the developer's real CLIs.
    ///
    /// Asserted rather than left implicit because the failure it prevents is expensive and
    /// silent: a default-agent path picking `claude` over `e2e` spends real tokens and
    /// passes or fails on what happens to be installed. The `SYLPH_AGENTS_DIR` pin that
    /// used to hold that back is deleted with the adapter directories, so this is what
    /// replaces it — in the build rather than in a fixture's environment, which is where
    /// it cannot be forgotten.
    #[test]
    #[cfg(feature = "e2e")]
    fn the_fixture_feature_offers_the_fixture_and_nothing_else() {
        assert_eq!(known_agents(), &[e2e::AGENT_NAME]);
        assert!(is_known(e2e::AGENT_NAME));
        assert!(for_agent(e2e::AGENT_NAME).is_some());
        for real in ["claude", "codex", "pi"] {
            assert!(
                !is_known(real),
                "{real} must not be known to a fixture build"
            );
            assert!(
                for_agent(real).is_none(),
                "{real} must not resolve, however the PATH looks"
            );
        }
        // The fixture *replaces* the three rather than joining them, which is the point
        // of the feature: a build offering both would make "which agents exist" depend on
        // which vendor CLIs the developer happens to have installed, and the E2E suite's
        // default-agent path would pick `claude` and spend real tokens.
        assert_eq!(known_agents(), &[e2e::AGENT_NAME]);
    }

    /// An unknown name must never produce an adapter, however the `PATH` happens to look.
    #[test]
    fn an_unsupported_agent_never_resolves_to_an_adapter() {
        assert!(for_agent("gemini").is_none());
        assert!(for_agent("opencode").is_none());
        assert!(for_agent("").is_none());
        assert!(
            for_agent("claude ").is_none(),
            "no trimming: a name is exact"
        );
    }

    /// Listing an agent and being able to open it are the same question.
    ///
    /// The state this rules out is the one the deleted directory-scanning discovery had:
    /// an agent that was listed and unstartable, because availability came from one
    /// mechanism and opening from another. With `for_agent` as the only mechanism, a name
    /// that resolves is a name that opens — and this is asserted over *installed* agents
    /// only, since which of the three that is depends on the machine running the test.
    #[test]
    fn every_installed_agent_is_reachable_by_name() {
        let installed: Vec<&str> = known_agents()
            .iter()
            .copied()
            .filter(|name| for_agent(name).is_some())
            .collect();
        for name in installed {
            assert!(is_known(name), "{name} resolves but is not known");
            // Stable within one process: a caller renders this list, and an answer that
            // varied between calls would read as a bug.
            assert!(for_agent(name).is_some());
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

    /// Every agent this build knows answers for its own native facets.
    ///
    /// Exhaustive over `known_agents`, which is the list a stored session's agent name
    /// is checked against — so an agent this build can resolve but the dispatch cannot
    /// is the gap that would fall through to the conservative floor unnoticed.
    #[test]
    fn every_known_agent_answers_for_its_native_facets() {
        for agent in known_agents() {
            let facets = native_link_facets(agent);

            // Observation is the invariant: the record is the server's, so a native
            // source is always readable even when nothing can drive it.
            assert!(
                facets.observe.available,
                "{agent} must stay observable from the store"
            );

            // Control must be a *stated* answer either way. An unavailable facet with
            // no reason is the shape a renderer cannot explain to a user.
            if !facets.control.available {
                assert!(
                    facets.control.reason.is_some(),
                    "{agent} reports control unavailable without saying why"
                );
            }
        }
    }
}
