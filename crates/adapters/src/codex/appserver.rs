//! The `codex` app-server control surface.
//!
//! `codex`'s second-party control is the strongest of the three agents and the reason
//! `docs/architecture.md` makes control a facet that can report unavailable with a
//! reason: the app-server's own generated schema declares 153 client request methods,
//! and `turn/interrupt` and `turn/steer` were both measured driving a live turn
//! (`docs/measurements/codex/`).
//!
//! **`app-server --listen unix://PATH` is the door, not `remote-control`.** The latter
//! is gated twice — `remote control requires ChatGPT authentication; API key auth is
//! not supported`, plus a managed standalone install — so an envagent that reached for
//! it would work on one class of account and fail on another with a message about
//! authentication rather than about the transport.
//!
//! # Status
//!
//! Skeleton. The two functions below are what `codex/mod.rs` names; their bodies are
//! the live control path, which lands with the envagent in step 4 of
//! `docs/roadmap.md` § Implementation order — the same deferral `claude/mod.rs`'s
//! `run()` makes, and for the same reason: holding a vendor's stdio is the envagent's
//! job, on the machine whose files it reads.
//!
//! What is *not* deferred is the knowledge, which is recorded here rather than in a
//! commit message because each item is a branch a reader would otherwise write wrong:
//!
//! - **`turn/steer` requires an `expectedTurnId`** and refuses a stale one (`expected
//!   active turn id X but found Y`). That is the mid-turn precondition
//!   `docs/session-protocol.md` asks for, implemented by the vendor rather than
//!   simulated by the adapter.
//! - **`thread/queue/add` requires an undocumented `clientUserMessageId`.**
//! - **Observation requires an explicit `thread/resume` and is independent of
//!   control.** A second client attached to the same daemon sees zero `item/*` events
//!   until it resumes — including for a turn it started itself — and `thread/unsubscribe`
//!   returns it to silence *while leaving control working*. A thread with no completed
//!   turn is unresumable (`no rollout found for thread id`).
//! - **An approval decision outside the enum is accepted and silently denies the
//!   command.** See [`APPROVAL_DECISIONS`].

use anyhow::{anyhow, Result};

/// The four tokens an approval decision may carry, and nothing else.
///
/// **This is the most dangerous vendor-key instance measured in this repo**, because
/// every observable layer reports success. Measured on codex-cli 0.150.1: sending
/// `"approved"` — a plausible guess, and not in the enum — produced **no error at any
/// layer**. The JSON-RPC reply was accepted, the turn reported `completed`, and only
/// the command's own status two levels down inside an item said otherwise.
///
/// Measured three ways against the same command:
///
/// | sent | command status | elapsed |
/// | --- | --- | --- |
/// | `accept` | `completed`, `exitCode: 0` | 72 ms — it really ran |
/// | `decline` | `declined` | — |
/// | `"approved"` | `failed` | 3 ms — it never ran |
///
/// So an adapter that guesses this token ships an approval channel that refuses
/// **everything** while reporting success everywhere a caller would look. The tokens
/// are read from the vendor's own schema
/// (`docs/measurements/codex/schema/CommandExecutionRequestApprovalParams.json`), never
/// inferred from what the word obviously ought to be.
pub const APPROVAL_DECISIONS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];

/// Whether a decision token is one the vendor will act on.
///
/// Exists so the guess cannot be made in two places. `docs/session-protocol.md`
/// § "A refused set" wants an unknown value refused as `INVALID_VALUE`, and that is
/// only possible if something checks before the wire.
pub fn is_valid_decision(decision: &str) -> bool {
    APPROVAL_DECISIONS.contains(&decision)
}

/// The efforts a given model will actually accept, from `model/list`.
///
/// **This is the authority for a live session, and it has to be, because the vendor
/// stops validating here.** The schema types `ReasoningEffort` as a bare non-empty
/// string, so `effort: "banana"` is accepted, echoed back by `thread/settings/updated`
/// as the current value, and fails only when a turn runs. An unknown *option name* is
/// accepted silently with `result: {}`. So codex emits no
/// `RefusalCode::UnsupportedField` signal at all and anything not enum-typed must be
/// caught adapter-side or it is lost.
///
/// Per model rather than globally, because that is how the vendor reports it —
/// `supportedReasoningEfforts` is a field on a catalogue entry.
pub fn supported_efforts(_model: &str) -> Result<Vec<String>> {
    Err(anyhow!(
        "codex `supported_efforts` needs a live app-server connection, which lands \
         with the envagent in step 4 of docs/roadmap.md § Implementation order; until \
         then a spawn offers codex::SPAWN_EFFORT_CHOICES"
    ))
}

/// The parameters `thread/start` takes, built from a [`sylph_protocol::RunSpec`].
///
/// **`thread/start` has no `threadId` parameter at all** — the id comes back at
/// `result.thread.id` — which is how "codex assigns its own session id" follows from
/// the shape of the API rather than from observation. So a `RunSpec` asking to dictate
/// an id cannot be honoured here, and that has to be an error rather than a silently
/// ignored field.
pub fn thread_start_params(_spec: &sylph_protocol::RunSpec) -> Result<serde_json::Value> {
    Err(anyhow!(
        "codex `thread_start_params` needs the live app-server path, which lands with \
         the envagent in step 4 of docs/roadmap.md § Implementation order"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four tokens, pinned exactly.
    ///
    /// A test rather than a comment because the failure mode is silent: a fifth token
    /// added here, or one of these four misspelled, produces an approval channel that
    /// denies everything while every layer reports success.
    #[test]
    fn the_approval_enum_is_exactly_the_four_tokens_the_vendor_declares() {
        assert_eq!(
            APPROVAL_DECISIONS,
            &["accept", "acceptForSession", "decline", "cancel"],
        );
        for decision in APPROVAL_DECISIONS {
            assert!(is_valid_decision(decision), "{decision} must be accepted");
        }
    }

    /// The specific guess that was measured failing silently.
    #[test]
    fn a_plausible_guess_outside_the_enum_is_refused_before_the_wire() {
        // Measured on 0.150.1: sent to the vendor, this answered `completed` at the
        // turn while the command reported `failed` in 3 ms, never having run.
        assert!(!is_valid_decision("approved"));
        // Two more of the same shape, since the point is the class rather than the word.
        assert!(!is_valid_decision("allow"));
        assert!(!is_valid_decision("yes"));
    }
}
