//! Which agents this machine can run, and where the server gets one from.
//!
//! # Availability is `PATH`, and that is the whole check
//!
//! The server holds a [`sylph_protocol::Session`] obtained from
//! `sylph_adapters::for_agent(name)?.run(spec)`, so there is nothing to find and no path
//! to get wrong. `sylph_adapters::on_path` is a filesystem check rather than a spawn,
//! deliberately: an absent binary is a persistent state, and probing per request turned
//! one missing CLI into 2339 identical failure lines in a single E2E run.
//!
//! One mechanism means listing an agent and being able to open it are the same question.
//! The state that rules out is worth naming because it is what a second mechanism
//! produced: an agent **listed and unstartable**, reported because its CLI was on `PATH`
//! and then failing `AGENT_START_FAILED` because no adapter directory answered for it.
//!
//! # Why this is a trait rather than three free functions
//!
//! [`AdapterSource`](crate::adapters::AdapterSource) is a **test seam**, and its own documentation below is where that is
//! argued — including why it must not grow a third implementation. Holding it in
//! [`crate::AppState`] is what lets `/health`, `/diagnostics`, `GET /environments`, the
//! validation gate on `POST /sessions`, and the link manager read one object, so none of
//! them can disagree about which agents exist.
//!
//! # Zero agents is a state a client must be able to explain
//!
//! It was once a silent, total failure: no session could be created, every existing one
//! reported `attached: false`, and the composer's slash menu made `/` a dead keystroke —
//! three layers from the cause, logged at the same level as success. So availability is
//! reported **per environment** by [`crate::api::environments`]: "codex is not installed
//! on this laptop" is a fact about one machine, and a merged global list would be true of
//! nowhere.

use std::sync::Arc;

use sylph_protocol::Adapter;

/// Where the server gets an adapter, and what it may report about them.
///
/// Three questions rather than one, because the answers have different consequences and
/// collapsing them is what discovery could not do: [`Self::available`] is what a client
/// renders, [`Self::for_agent`] is what a session is opened with, and [`Self::is_known`]
/// is what lets a caller say **which** thing is wrong. `"codex"` on a machine without the
/// CLI is a missing install, which names a fix; `"gemini"` is an unsupported agent, which
/// does not (`AGENTS.md` § Product Invariants).
///
/// # This is a test seam, **not** a plug-in point
///
/// It has exactly two implementations and is meant to: [`LinkedAdapters`] in production and
/// the integration suite's mock. **Do not add a third to load user-supplied adapters.**
/// There is deliberately no adapter directory any more — the escape hatch for "a vendor
/// changed faster than the shipped adapter" is upgrading the envagent on that one machine
/// (`AGENTS.md` § Configuration) — and a trait shaped like an extension point is an
/// invitation to put one back.
///
/// The reason it exists at all is that availability is `PATH`, which is **global process
/// state a test cannot mutate per case**: `std::env::set_var` is process-wide and unsafe in
/// a threaded test binary, so several tests running concurrently cannot each offer a
/// different agent. `MockAgents::uninstall` needs somewhere to answer from, and the
/// alternative was the filesystem-shaped fixture this replaced — which `AGENTS.md` records
/// silently stopping to test anything the moment the harness layout moved, leaving a test
/// that still asserted the detached path and no longer produced it.
pub trait AdapterSource: Send + Sync {
    /// The adapter for one agent, or `None` when this machine cannot run it.
    ///
    /// `None` is `sylph_protocol::UnavailableReason::NoAgent`: an install, not a retry.
    /// That distinction is why the reason is typed — a client caches an unretryable
    /// reason like a success, while a failed handshake must go stale.
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>>;

    /// The agents this source can run, sorted.
    ///
    /// **An empty result is a real state and not an error.** A machine with none of the
    /// three installed genuinely has no agents, and the caller's job is to say which are
    /// missing rather than to treat the emptiness as a failure to discover something.
    fn available(&self) -> Vec<String>;

    /// Whether this source knows an agent by this name at all, installed or not.
    ///
    /// A stored session's agent name is checked here rather than against an enum, because
    /// a journal written under a build that knew four agents must stay readable under one
    /// that knows three.
    fn is_known(&self, name: &str) -> bool;

    /// Every name this source knows, whether or not its CLI is installed.
    ///
    /// Here rather than read from `sylph_adapters` at the call site, so a message listing
    /// what is supported cannot contradict [`Self::is_known`]. It could: a caller that
    /// asked the source whether a name was known and then printed the *compiled-in* list
    /// would answer "Unknown agent: claude. This build supports: [claude, codex, pi]" the
    /// moment the two disagreed — which is exactly what an injected source makes true.
    fn known_agents(&self) -> Vec<String>;
}

/// The adapters compiled into this binary, for the agents installed on this machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinkedAdapters;

impl AdapterSource for LinkedAdapters {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        sylph_adapters::for_agent(name)
    }

    /// Sorted, because callers render it and an order that varies between calls reads as
    /// a bug — `/diagnostics` churned on it when the answer came from directory
    /// iteration.
    fn available(&self) -> Vec<String> {
        let mut names: Vec<String> = sylph_adapters::known_agents()
            .iter()
            .filter(|name| sylph_adapters::for_agent(name).is_some())
            .map(|name| name.to_string())
            .collect();
        names.sort();
        names
    }

    fn is_known(&self, name: &str) -> bool {
        sylph_adapters::is_known(name)
    }

    fn known_agents(&self) -> Vec<String> {
        sylph_adapters::known_agents()
            .iter()
            .map(|name| name.to_string())
            .collect()
    }
}

/// The production source, as the server holds it.
pub fn linked() -> Arc<dyn AdapterSource> {
    Arc::new(LinkedAdapters)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three supported agents are known, and the two dropped ones are not.
    ///
    /// Asserted on `is_known` rather than on [`AdapterSource::available`], because what
    /// is *installed* depends on the machine running the test — which is exactly the
    /// property that makes it unassertable.
    #[test]
    fn the_supported_agents_are_known_and_the_dropped_two_are_not() {
        let source = LinkedAdapters;
        for supported in ["claude", "codex", "pi"] {
            assert!(source.is_known(supported), "{supported} must be known");
        }
        // `AGENTS.md` § Product Invariants: a deliberate narrowing, not a gap.
        for dropped in ["gemini", "opencode"] {
            assert!(!source.is_known(dropped), "{dropped} must not be known");
        }
    }

    /// Every agent listed resolves to an adapter, in both directions.
    ///
    /// This is the property the deleted union could not have: it reported names from two
    /// mechanisms that could disagree, so a listed agent was not necessarily a startable
    /// one. With one mechanism the two answers are the same question.
    #[test]
    fn every_available_agent_resolves_to_an_adapter() {
        let source = LinkedAdapters;
        for name in source.available() {
            assert!(
                source.for_agent(&name).is_some(),
                "{name} is listed but has no adapter"
            );
            assert!(source.is_known(&name));
        }
    }

    /// An unknown name never produces an adapter, however the `PATH` happens to look.
    #[test]
    fn an_unsupported_agent_never_resolves_to_an_adapter() {
        let source = LinkedAdapters;
        assert!(source.for_agent("gemini").is_none());
        assert!(source.for_agent("").is_none());
    }

    /// Sorted and stable within one process, because a caller renders it.
    #[test]
    fn availability_is_sorted_and_stable() {
        let source = LinkedAdapters;
        let first = source.available();
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(first, sorted);
        assert_eq!(first, source.available());
    }
}
