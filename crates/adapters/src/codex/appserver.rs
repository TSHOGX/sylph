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
//! - **An approval decision is not a string, and the enum is not the authority.** See
//!   [`Decision`] and [`REQUEST_SCOPED_AUTHORITY`].

use anyhow::{anyhow, Result};
use serde_json::Value;

/// One approval decision, in the shape the vendor's schema actually declares.
///
/// **`CommandExecutionApprovalDecision` has six variants and only four are strings.**
/// Read from `docs/measurements/codex/schema/CommandExecutionRequestApprovalResponse.json`:
/// `accept`, `acceptForSession`, `decline`, and `cancel` are bare strings, while
/// `acceptWithExecpolicyAmendment` and `applyNetworkPolicyAmendment` are **objects
/// carrying a payload** — an execpolicy amendment or a network policy rule.
///
/// An earlier version of this module typed a decision as `&str` with a four-token
/// allowlist. That was a correctness hole in the one place this module exists to get
/// right: a `&str` cannot express either object variant, so the two decisions that carry
/// the user's actual intent — "approve this, and stop asking for commands like it" —
/// were unrepresentable.
///
/// It was worse than incomplete, because the allowlist was also wrong about what the
/// vendor offers. See [`REQUEST_SCOPED_AUTHORITY`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// One of the four bare-string tokens.
    Token(String),
    /// An object variant, keyed by its single member name — `acceptWithExecpolicyAmendment`
    /// or `applyNetworkPolicyAmendment` — carrying the payload the vendor offered.
    ///
    /// The payload is echoed back rather than constructed: the request's own
    /// `availableDecisions` supplies it complete, and rebuilding it invites an
    /// amendment that does not match what the user was shown.
    Amendment { name: String, payload: Value },
}

impl Decision {
    /// The wire form, ready to place in a `result`.
    pub fn to_wire(&self) -> Value {
        match self {
            Self::Token(token) => Value::String(token.clone()),
            Self::Amendment { name, payload } => {
                serde_json::json!({ name.clone(): payload.clone() })
            }
        }
    }
}

/// The four bare-string tokens `CommandExecutionApprovalDecision` declares.
///
/// **This is the string subset, not the whole enum** — the other two variants are
/// objects ([`Decision::Amendment`]). Kept as a constant because the four spellings are
/// worth pinning: they are camelCase here and snake_case in the sibling enum below, and
/// crossing them is silent.
pub const APPROVAL_TOKENS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];

/// Why the enum is the type and the *request* is the authority.
///
/// **Measured over every approval in `docs/measurements/codex/captures/`: all three offer
/// `["accept", {"acceptWithExecpolicyAmendment": {…}}, "cancel"]`.** So on this install
/// the vendor never offered `acceptForSession` or `decline` at all, while offering an
/// object variant every single time.
///
/// A global allowlist therefore fails in both directions at once: it admits
/// `acceptForSession`, which the vendor did not offer for that request and may reject,
/// and it rejects the amendment decision the vendor offered in every measured case.
///
/// So an adapter validates a decision against **that request's own
/// `availableDecisions`**, and treats this enum as the type rather than as the
/// permission. `PermissionsRequestApprovalResponse` makes the same point from the other
/// side: it is not a string either, but `{permissions, scope, strictAutoReview}`.
pub const REQUEST_SCOPED_AUTHORITY: &str =
    "validate a decision against the request's availableDecisions, not a global list";

/// Whether `decision` is one the vendor offered *for this request*.
///
/// `available` is the request's own `availableDecisions` array, verbatim. Comparison is
/// structural rather than by token, because an offered decision may be an object.
pub fn is_offered(decision: &Decision, available: &[Value]) -> bool {
    let wire = decision.to_wire();
    available.iter().any(|offered| match (&wire, offered) {
        // An object variant matches on its member *name*: the payload the vendor offered
        // is the one to send back, so a caller echoing it is right and a caller
        // constructing a different amendment is not silently accepted.
        (Value::Object(sent), Value::Object(offer)) => {
            sent.keys().next() == offer.keys().next()
        }
        (sent, offer) => sent == offer,
    })
}

/// The **other** decision vocabulary in the same schema, and the reason a "plausible
/// guess" was not one.
///
/// `ReviewDecision` governs `execCommandApproval` and `applyPatchApproval` — the two
/// legacy server requests, which `ServerRequest` declares **alongside** the `item/*`
/// forms, so both vocabularies are live at once. Its variants are
/// `approved`, `approved_for_session`, `approved_mcp_policy_amendment`, `timed_out`,
/// `abort`, plus objects `approved_execpolicy_amendment`, `network_policy_amendment`,
/// and `denied{rejection}`.
///
/// **This is why `"approved"` was accepted and silently denied the command.** It is not
/// an invented word: it is a *real token from this enum*, sent to a request governed by
/// the other one. `AGENTS.md` records the general form — an SDK's attribute names and a
/// wire's field names are different namespaces — and here the two namespaces differ in
/// **case convention as well as vocabulary**, snake_case here against camelCase there.
/// So the hazard is permanent rather than a one-off: an adapter handling both request
/// families keeps two decision vocabularies straight or produces an approval channel
/// that refuses everything while every layer reports success.
///
/// Note `denied` is an object variant here too, so even inside its own namespace the
/// bare string is wrong.
pub const REVIEW_DECISIONS: &[&str] = &[
    "approved",
    "approved_for_session",
    "approved_mcp_policy_amendment",
    "timed_out",
    "abort",
];

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
pub fn thread_start_params(_spec: &sylph_protocol::RunSpec) -> Result<Value> {
    Err(anyhow!(
        "codex `thread_start_params` needs the live app-server path, which lands with \
         the envagent in step 4 of docs/roadmap.md § Implementation order"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four string tokens, pinned — and the fact that they are not the whole enum.
    #[test]
    fn the_string_tokens_are_four_and_are_not_the_whole_enum() {
        assert_eq!(
            APPROVAL_TOKENS,
            &["accept", "acceptForSession", "decline", "cancel"],
        );
        // The other two variants are objects, so a `&str` allowlist cannot express
        // them. This assertion exists so a future simplification back to strings
        // fails here rather than in production.
        let amendment = Decision::Amendment {
            name: "acceptWithExecpolicyAmendment".into(),
            payload: serde_json::json!({"execpolicy_amendment": ["echo", "ok"]}),
        };
        assert!(amendment.to_wire().is_object());
    }

    /// The measured shape of a real request, and both directions the old check failed in.
    ///
    /// Every approval in the capture corpus offers exactly this: `accept`, an execpolicy
    /// amendment, and `cancel` — so `acceptForSession` was never on offer while the
    /// amendment always was.
    #[test]
    fn a_decision_is_validated_against_the_request_rather_than_a_global_list() {
        let available = vec![
            serde_json::json!("accept"),
            serde_json::json!({"acceptWithExecpolicyAmendment": {
                "execpolicy_amendment": ["echo", "needs-approval"]
            }}),
            serde_json::json!("cancel"),
        ];

        assert!(is_offered(&Decision::Token("accept".into()), &available));
        assert!(is_offered(&Decision::Token("cancel".into()), &available));

        // Offered in every measured request, and unrepresentable under the old `&str`
        // check — which is the hole this shape closes.
        assert!(is_offered(
            &Decision::Amendment {
                name: "acceptWithExecpolicyAmendment".into(),
                payload: serde_json::json!({"execpolicy_amendment": ["echo", "needs-approval"]}),
            },
            &available
        ));

        // In the enum, and *not offered here*. The old global allowlist admitted both.
        assert!(!is_offered(
            &Decision::Token("acceptForSession".into()),
            &available
        ));
        assert!(!is_offered(&Decision::Token("decline".into()), &available));
    }

    /// The two vocabularies are disjoint, and crossing them is the measured failure.
    ///
    /// `"approved"` is a real `ReviewDecision` token sent to a request governed by
    /// `CommandExecutionApprovalDecision`. Measured: no error at any layer, the turn
    /// reported `completed`, and the command reported `failed` in 3 ms having never run.
    #[test]
    fn the_two_decision_vocabularies_share_no_token() {
        for review in REVIEW_DECISIONS {
            assert!(
                !APPROVAL_TOKENS.contains(review),
                "{review} belongs to ReviewDecision and must not be accepted as a \
                 CommandExecutionApprovalDecision",
            );
        }
        // The specific crossing that was measured failing silently.
        assert!(REVIEW_DECISIONS.contains(&"approved"));
        assert!(!APPROVAL_TOKENS.contains(&"approved"));
        // And `denied` is an object variant even in its own namespace, so it is absent
        // from the string list above rather than merely spelled differently.
        assert!(!REVIEW_DECISIONS.contains(&"denied"));
    }
}
