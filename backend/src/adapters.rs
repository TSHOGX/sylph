//! Which agents this machine can run, and where the server gets one from.
//!
//! # Availability is `PATH`, and that is now the whole check
//!
//! This module used to answer as the **union** of two live mechanisms: the vendor CLI
//! being on `PATH`, and a directory holding an `adapter.sh` that the server could spawn.
//! Both were genuinely live while `adapter::Link` existed, and the union was the
//! truthful answer rather than compatibility code — but it produced a state worth naming
//! because it is the one this change ends: an agent could be **listed and unstartable**.
//! A machine with `claude` on `PATH` reported it, and `POST /sessions` naming it failed
//! `AGENT_START_FAILED` because no `adapters/claude` directory answered.
//!
//! The server now holds a [`sylph_protocol::Session`] obtained from
//! `sylph_adapters::for_agent(name)?.run(spec)`, so there is nothing to find and no path
//! to get wrong. `adapter_path`, `is_agent_dir`, the adapter roots, and the
//! `SYLPH_AGENTS_DIR`/`SYLPH_BUNDLED_AGENTS_DIR` plumbing behind them are all gone with
//! it: a setting that named a directory of adapters is only meaningful while a directory
//! of adapters answers.
//!
//! # Why this is a trait rather than three free functions
//!
//! [`AdapterSource`] is the seam, and it exists for the same reason
//! [`crate::env::Environment`] does: one implementation today, and step 4 of
//! `docs/roadmap.md` § Implementation order replaces it with a **per-environment** answer
//! dialled in from an envagent. Holding it in [`crate::AppState`] means pairing replaces
//! one field rather than reaching into every consumer — `/health`, `/diagnostics`,
//! `GET /environments`, the validation gate on `POST /sessions`, and the link manager all
//! read the same object, so none of them can disagree about which agents exist.
//!
//! It is also what makes the integration suite able to drive the production code path.
//! A test needs an agent that is deterministic, has no vendor CLI, and can be told to
//! fail — and it needs a *different* one per test running concurrently in one process,
//! which a name-keyed global registry cannot provide. Injecting the source is how the
//! harness gets that without the server growing a test-only branch.
//!
//! # Availability is one fact about the machine, not one fault per caller
//!
//! `sylph_adapters::on_path` is a filesystem check rather than a spawn, deliberately: one
//! missing binary produced 2339 identical failure lines in a single E2E run when the probe
//! was per-request.
//!
//! What must **not** be lost with discovery is the reporting. Zero usable agents was a
//! silent, total failure: no session could be created, every existing one reported
//! `attached: false`, and the composer's slash menu made `/` a dead keystroke — three
//! layers from the cause, logged at the same level as success. That is why
//! [`crate::api::environments`] reports availability **per environment**: "codex is not
//! installed on this laptop" is a fact about one machine, and a merged global list would be
//! true of nowhere.

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
