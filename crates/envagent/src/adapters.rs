//! Where this envagent gets an adapter, and what it may report about them.
//!
//! # Availability is `PATH`, and that is the whole check
//!
//! An adapter is linked into this binary and `sylph_adapters::for_agent(name)` answers
//! `None` when the vendor CLI is absent from this machine's `PATH`, so there is nothing
//! to discover and no directory to get wrong. Listing an agent and being able to open
//! it are the same question, which is what rules out the state discovery produced: an
//! agent **listed and unstartable**.
//!
//! # Why this is a trait rather than free functions
//!
//! [`AdapterSource`] is a **test seam**, and its own documentation is where that is
//! argued. Holding it in the runtime's configuration is what lets the inventory, the
//! `Run` command, and the native-store commands read one object, so none of them can
//! disagree about which agents exist.
//!
//! It used to live in the server. That was the in-process execution path
//! `docs/roadmap.md` § Milestone 1 deletes: the server holds no adapter now, and the
//! only process that does is this one.

use std::sync::Arc;

use sylph_protocol::Adapter;

/// Where the envagent gets an adapter, and what it may report about them.
///
/// # This is a test seam, **not** a plug-in point
///
/// Production uses [`LinkedAdapters`]; tests substitute controlled sources. User-supplied
/// adapters are not loaded. There is deliberately no adapter directory — the escape hatch for "a
/// vendor changed faster than the shipped adapter" is upgrading the envagent on that one
/// machine (`docs/configuration.md`) — and a trait shaped like an extension point is an
/// invitation to put one back.
///
/// The reason it exists at all is that availability is `PATH`, which is **global process
/// state a test cannot mutate per case**: `std::env::set_var` is process-wide and unsafe
/// in a threaded test binary, so several tests running concurrently cannot each offer a
/// different agent. A mock's `uninstall` needs somewhere to answer from.
pub trait AdapterSource: Send + Sync {
    /// The adapter for one agent, or `None` when this machine cannot run it.
    ///
    /// `None` is `sylph_protocol::UnavailableReason::NoAgent`: an install, not a retry.
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>>;

    /// Every name this source knows, whether or not its CLI is installed, sorted.
    ///
    /// The inventory lists all of them, because "known and unavailable" is the state
    /// that names a fix (`sylph_protocol::AgentInventory`).
    fn known_agents(&self) -> Vec<String>;

    /// Whether this source knows an agent by this name at all, installed or not.
    ///
    /// `"codex"` on a machine without the CLI is a missing install, which names a fix;
    /// `"gemini"` is an unsupported agent, which does not.
    fn is_known(&self, name: &str) -> bool {
        self.known_agents().iter().any(|known| known == name)
    }
}

/// The adapters compiled into this binary, for the agents installed on this machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinkedAdapters;

impl AdapterSource for LinkedAdapters {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        sylph_adapters::for_agent(name)
    }

    fn known_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = sylph_adapters::known_agents()
            .iter()
            .map(|name| name.to_string())
            .collect();
        names.sort();
        names
    }
}

/// The production source, as the runtime holds it.
pub fn linked() -> Arc<dyn AdapterSource> {
    Arc::new(LinkedAdapters)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every agent this build knows is either resolvable or absent, and nothing else.
    ///
    /// Asserted over `known_agents` rather than a literal list, because which of the
    /// three is *installed* depends on the machine running the test — which is exactly
    /// the property that makes an exact set unassertable.
    #[test]
    fn a_known_agent_is_either_installed_or_absent() {
        let source = LinkedAdapters;
        assert!(!source.known_agents().is_empty());
        for name in source.known_agents() {
            assert!(source.is_known(&name));
            assert_eq!(
                source.for_agent(&name).is_some(),
                source.for_agent(&name).is_some(),
                "{name} must answer the same twice in one process"
            );
        }
        assert!(source.for_agent("gemini").is_none());
        assert!(!source.is_known("gemini"));
    }
}
