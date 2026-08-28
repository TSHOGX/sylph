//! What Sylph can and cannot do with a session right now, and why not.
//!
//! `docs/architecture.md` § One link, two typed capability facets is authoritative.

use serde::{Deserialize, Serialize};

/// Why one facet of a link is unavailable.
///
/// # What this replaces, and why the old vocabulary could not be extended
///
/// `LinkFailureCode` had two values, `NoAdapter` and `HandshakeFailed`, and its own doc
/// comment recorded the problem: **both describe *our own spawn* failing**, so neither
/// can say "somebody else's process, running fine, not ours to drive". That is the
/// common case now — a session running in a terminal — so it is a missing variant
/// rather than a mislabelled one.
///
/// The two names also changed meaning with the transport. There is no adapter to be
/// absent: adapters are linked into the envagent, and an agent's availability is
/// whether its CLI is on that machine's `PATH` (`docs/agents.md`). So `NoAdapter`
/// becomes [`Self::NoAgent`], which is a question with one answer and one fix.
///
/// # Caching, which is a client concern this enum decides
///
/// A detached link is a **200 with a reason**, not an error, because that keeps a
/// session readable when its agent is gone. Under `staleTime: Infinity` that made one
/// failed handshake permanent for the life of the page, and `/` stayed a dead keystroke
/// until a turn was sent or the tab reloaded — which is what made a deterministic
/// config problem present as intermittent.
///
/// [`Self::is_retryable`] is the line: a reason that no retry changes is cacheable like
/// a success, and a failed handshake must be allowed to go stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// This agent's CLI is not on that machine's `PATH`. An install, not a retry.
    NoAgent,
    /// No envagent is connected for this session's environment.
    ///
    /// **Observe still answers from the server's store**, which is the whole reason the
    /// two facets are separate: history reads while nothing can be sent. This is also
    /// what makes "another machine is read-only" fall out rather than being a rule
    /// anyone writes — control is available exactly while that environment's envagent
    /// is connected.
    EnvironmentUnreachable,
    /// The process is alive under someone else, and this vendor offers a second party no
    /// control channel.
    ///
    /// *Measured 2026-08-27:* `pi` is stdio-only — `RpcClientOptions` is
    /// `{cliPath, cwd, env, provider, model, args}`, no address field, no socket — so a
    /// terminal-born `pi` session is observable and undrivable. That is the case that
    /// rules out a third link *state*: the vendors do not sort into three buckets, so a
    /// third enum variant would have to pick one and misreport the others.
    NotOursToDrive,
    /// The channel exists and would not come up: a crash, a timeout, a refused
    /// handshake. **The one reason a retry can fix.**
    HandshakeFailed,
}

impl UnavailableReason {
    /// Whether trying again could change the answer.
    ///
    /// A client caches an unretryable reason like a success. No retry installs a CLI,
    /// wakes a sleeping laptop, or teaches a vendor a new channel — but a failed
    /// handshake against a reachable environment must go stale.
    ///
    /// Declared here rather than in the client so there is one answer. The frontend had
    /// this rule as a condition inside a query's `staleTime`, which is a place a second
    /// consumer cannot read.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::HandshakeFailed)
    }
}

/// One facet of a link: available, or unavailable with a reason.
///
/// Not a bare `bool`. An unexplained `false` is what the original defect was: the
/// server logged the reason and returned `attached: false`, so a client could not
/// distinguish a failed handshake from an agent that simply declares no commands, and
/// the only honest thing it could render was nothing at all. That is how a stale
/// install presented as a composer ignoring the `/` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facet {
    pub available: bool,
    /// Populated exactly when `available` is false.
    ///
    /// The invariant is enforced by the constructors rather than by the type, because a
    /// two-variant enum would serialize as a tagged union and this shape is what
    /// `docs/frontend-architecture.md` § Two facets, each with a reason renders
    /// (`link.control.available`, then `link.control.reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnavailableReason>,
}

impl Facet {
    pub fn available() -> Self {
        Self {
            available: true,
            reason: None,
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            available: false,
            reason: Some(reason),
        }
    }
}

/// A link's two facets.
///
/// **One state, two facets** — not three states (`docs/architecture.md`). The facets
/// are independent because the two questions they answer are: observation may be
/// concurrent, since reading creates no branch, while control must have a single owner
/// with explicit handoff. *Measured:* none of the agent CLIs takes a lock, and two
/// writers to one session produce sibling branches off a single parent pointer with
/// zero errors at the storage layer. The damage is purely semantic, which is why
/// nothing catches it.
///
/// The rule a consumer must hold to: **an empty capability surface means "Sylph cannot
/// reach it", never "there is nothing there"** ([`crate::SessionCapabilities`]). Those
/// coincide for a session Sylph launched and are opposite for one running in a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkFacets {
    /// Can Sylph see what this session is doing.
    ///
    /// Near-universal: all three supported agents write their native store mid-turn, so
    /// an observer sees progress rather than only outcomes. It is unavailable mainly
    /// when the environment is unreachable and the store holds nothing yet.
    pub observe: Facet,
    /// Can Sylph send this session a turn.
    pub control: Facet,
}

/// An agent refused a control, in its own words and with its own code.
///
/// # Two codes, and why one is not enough
///
/// | Code | Means | What a client may do |
/// | --- | --- | --- |
/// | [`RefusalCode::UnsupportedField`] | this agent has no such option | disable the control |
/// | [`RefusalCode::InvalidValue`] | the option is real; *this value* was rejected | report it, leave the control usable |
///
/// The server maps these to distinct statuses (`InvalidValue` → 400,
/// `UnsupportedField` → 409). Both adapters already drew the line and the server
/// flattened every refusal into one error type whose own doc comment said a client may
/// disable the control over it — so one refused permission mode would have retired a
/// picker whose other five values work.
///
/// `claude` is the case that showed why it matters: `set_permission_mode` to
/// `bypassPermissions` is **refused outright** unless the process was launched with a
/// flag marking that mode available — a refusal about a *precondition*, reported as a
/// missing capability, three layers from the picker it sent people to.
///
/// # The code is typed, and recovered by type
///
/// It travels through `anyhow` and is recovered with `downcast_ref`, **not** by parsing
/// it back out of the message. The text is the agent's and may be reworded; the code is
/// declared contract. `api/links.rs` compared `refusal.code == "INVALID_VALUE"` against
/// a `String`, so `UNSUPPORTED_FIELD` was never matched by name at all — an enum makes
/// that a compile error instead of a branch that never fires.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct AdapterError {
    pub code: RefusalCode,
    /// For a person to read. **An error must name the layer that is actually at
    /// fault** — answering a malformed value with "'model' is not settable on a pi
    /// session" says the option does not exist when the option was fine and the value
    /// was not, and the layer that is *not* at fault is then the one that gets debugged.
    pub message: String,
}

impl AdapterError {
    pub fn unsupported_field(message: impl Into<String>) -> Self {
        Self {
            code: RefusalCode::UnsupportedField,
            message: message.into(),
        }
    }

    pub fn invalid_value(message: impl Into<String>) -> Self {
        Self {
            code: RefusalCode::InvalidValue,
            message: message.into(),
        }
    }
}

/// Which of the two refusals this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RefusalCode {
    /// No such option on this agent. Disabling the control is the right response.
    UnsupportedField,
    /// The option exists and this value was rejected. **The control stays usable** —
    /// the other five values may well be fine, and the session may be able to change
    /// whatever made this one invalid.
    InvalidValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caching line, stated once here rather than as a condition inside a query.
    #[test]
    fn only_a_failed_handshake_is_worth_retrying() {
        assert!(UnavailableReason::HandshakeFailed.is_retryable());
        for permanent in [
            UnavailableReason::NoAgent,
            UnavailableReason::EnvironmentUnreachable,
            UnavailableReason::NotOursToDrive,
        ] {
            assert!(!permanent.is_retryable(), "{permanent:?}");
        }
    }

    /// The client reads `available` then `reason`, so an available facet must carry no
    /// reason and an unavailable one must always carry one.
    #[test]
    fn a_facet_carries_a_reason_exactly_when_it_is_unavailable() {
        let ok = serde_json::to_value(Facet::available()).unwrap();
        assert_eq!(ok["available"], true);
        assert!(ok.get("reason").is_none());

        let no = serde_json::to_value(Facet::unavailable(UnavailableReason::NotOursToDrive))
            .unwrap();
        assert_eq!(no["available"], false);
        assert_eq!(no["reason"], "not_ours_to_drive");
    }

    /// The observable-but-undrivable case, which is a state and never a failure. It is
    /// asserted because it is the shape a third link state would have misreported.
    #[test]
    fn a_terminal_session_is_observable_and_undrivable() {
        let facets = LinkFacets {
            observe: Facet::available(),
            control: Facet::unavailable(UnavailableReason::NotOursToDrive),
        };
        assert!(facets.observe.available);
        assert!(!facets.control.available);
    }

    /// The refusal code is matched by *type*, which is what the string comparison in
    /// `api/links.rs` could not do reliably — it compared against `"INVALID_VALUE"`, so
    /// `UNSUPPORTED_FIELD` was never matched by name.
    #[test]
    fn a_refusal_travels_through_anyhow_and_is_recovered_by_type() {
        let error = anyhow::Error::new(AdapterError::invalid_value("no such model"));
        let recovered = error
            .downcast_ref::<AdapterError>()
            .expect("the code must survive the trip through anyhow");
        assert_eq!(recovered.code, RefusalCode::InvalidValue);
        assert_ne!(recovered.code, RefusalCode::UnsupportedField);
    }

    /// The wire spelling is the protocol's, `SCREAMING_SNAKE_CASE`, because that is
    /// what `docs/session-protocol.md` names and what adapters already emit.
    #[test]
    fn a_refusal_code_keeps_its_protocol_spelling() {
        assert_eq!(
            serde_json::to_value(RefusalCode::UnsupportedField).unwrap(),
            "UNSUPPORTED_FIELD"
        );
        assert_eq!(
            serde_json::to_value(RefusalCode::InvalidValue).unwrap(),
            "INVALID_VALUE"
        );
    }
}
