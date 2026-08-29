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
//! # Transport: stdio NDJSON, and the socket is a different mode
//!
//! [`crate::codex::live`] holds the vendor's own stdio, so it speaks **NDJSON** —
//! `codex app-server` with no further arguments, which is what all 15 probes measured.
//! `--listen unix://PATH` speaks the same JSON-RPC vocabulary over **WebSocket** and is
//! the door for attaching to a daemon *somebody else* started; that is a `watch`
//! concern, and building `run` on it would mean a hand-written RFC6455 handshake
//! (`docs/measurements/codex/README.md`) to reach a process Sylph already owns the
//! stdio of.
//!
//! What is recorded here rather than in a commit message, because each item is a branch
//! a reader would otherwise write wrong:
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
use serde_json::{json, Value};
use sylph_protocol::{AdapterError, RequestedNativeId, RunSpec};

/// The client requests this adapter issues, out of the schema's 153.
///
/// Named constants rather than string literals at each call site, because a misspelled
/// method here is answered `-32600` at a layer that reports it as a refused control —
/// and only the *four* below were watched answering on a live session. The other 149 are
/// deliberately absent: `AGENTS.md` § "A type in a vendor's event union is not a promise
/// that anything emits it" applies to methods as much as to events.
pub mod method {
    /// The handshake. Answers `{userAgent, codexHome, platformFamily, platformOs}`.
    pub const INITIALIZE: &str = "initialize";
    /// The notification that follows it. **Not a request** — it takes no id, and giving
    /// it one leaves a reply nothing correlates.
    pub const INITIALIZED: &str = "initialized";
    /// Opens a thread. **No `threadId` parameter at all**; the id comes back at
    /// `result.thread.id`.
    pub const THREAD_START: &str = "thread/start";
    /// Continues one, **and is also the subscribe**: a client sees no `item/*` events
    /// until it resumes, including for a turn it started itself.
    pub const THREAD_RESUME: &str = "thread/resume";
    /// The real config surface. Answers `{}` — see [`SETTINGS_REPLY_IS_EMPTY`].
    pub const THREAD_SETTINGS_UPDATE: &str = "thread/settings/update";
    /// Starts a turn. Answers with the turn, whose `id` a steer needs.
    pub const TURN_START: &str = "turn/start";
    /// Ends the running turn, keeping the thread.
    pub const TURN_INTERRUPT: &str = "turn/interrupt";
    /// Adds to a running turn. Requires `expectedTurnId`.
    pub const TURN_STEER: &str = "turn/steer";
    /// The model catalogue, which carries `supportedReasoningEfforts` per entry.
    pub const MODEL_LIST: &str = "model/list";
}

/// The server notifications this adapter reads.
pub mod notification {
    pub const ITEM_STARTED: &str = "item/started";
    pub const ITEM_COMPLETED: &str = "item/completed";
    /// The only per-token frame this vendor emits, and therefore the whole reason
    /// `run` is [`sylph_protocol::Fidelity::Stream`] where the store is `blocks`.
    pub const AGENT_MESSAGE_DELTA: &str = "item/agentMessage/delta";
    pub const TURN_STARTED: &str = "turn/started";
    pub const TURN_COMPLETED: &str = "turn/completed";
    pub const SETTINGS_UPDATED: &str = "thread/settings/updated";
    pub const TOKEN_USAGE: &str = "thread/tokenUsage/updated";
    /// A turn-level failure, carrying the vendor's own sentence.
    pub const ERROR: &str = "error";
    /// Advisory, and the only notice of a bogus model: measured
    /// `Model metadata for 'no-such-model-xyz' not found`.
    pub const WARNING: &str = "warning";
    pub const MCP_STARTUP: &str = "mcpServer/startupStatus/updated";
    /// A request somebody else answered. See [`crate::codex::live`].
    pub const SERVER_REQUEST_RESOLVED: &str = "serverRequest/resolved";
    /// The one server **request** kind a real turn produced, out of 15 declared.
    pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
}

/// Why `set_config` cannot answer from its own reply.
///
/// **Measured on 0.150.1 over all 13 `thread/settings/update` calls in
/// `docs/measurements/codex/captures/s04_settings.jsonl`: every accepted one answers
/// `result: {}`.** The refreshed settings arrive separately, as a
/// [`notification::SETTINGS_UPDATED`] carrying the whole `threadSettings` object — and
/// `thread/read` does **not** carry them either (its `thread` object has no settings
/// member at all, measured on the same capture).
///
/// So the vendor's confirmation is a notification, which decides the shape of
/// `set_config`: it registers for that notification *before* writing, then reads the
/// surface the translate loop built from it. Answering from the request would make the
/// adapter the authority on a fact only the vendor holds, which is the defect
/// `AGENTS.md` records against `claude`'s `manual` → `default` normalization.
///
/// **And a no-op write emits nothing.** Of the 10 accepted calls, 8 notified; the two
/// silent ones are `noSuchOption` — accepted with `{}`, changing nothing — and a
/// `personality` set to the value already current. So an absent notification cannot be
/// read as failure, and the wait must fall back to the last known surface rather than
/// erroring.
pub const SETTINGS_REPLY_IS_EMPTY: &str =
    "thread/settings/update answers {}; the refreshed settings arrive as \
     thread/settings/updated, and a no-op write emits nothing at all";

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
///
/// `catalogue` is `model/list`'s own `data` array, verbatim. Answers `None` when the
/// catalogue does not hold that model, which is **not** the same as a model offering no
/// efforts: the caller has to refuse an unknown model rather than silently offering it an
/// empty ladder.
pub fn supported_efforts(catalogue: &[Value], model: &str) -> Option<Vec<String>> {
    let entry = catalogue
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))?;
    Some(
        entry
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .map(|efforts| {
                efforts
                    .iter()
                    // **`reasoningEffort`, not `effort` and not `id`.** Measured: an
                    // entry is `{"reasoningEffort": "low", "description": …}`, so the
                    // obvious two guesses both yield an empty ladder — which reads as a
                    // model that offers no efforts rather than as a misread key. This is
                    // the vendor-key family (`AGENTS.md`), and the sibling
                    // `supportedReasoningEfforts` entry is the one place in this
                    // vendor's payloads where the field repeats its own container's name.
                    .filter_map(|effort| effort.get("reasoningEffort").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Every model id the catalogue offers, for validating a `model` write.
///
/// **`id`, not `model` and not `displayName`.** An entry carries all three and `id` and
/// `model` happen to be equal on all 5 measured entries — which is exactly the kind of
/// coincidence that makes a wrong key survive: `id` is what `thread/settings/update`
/// takes, and a catalogue where they diverge would refuse every write while every layer
/// reported success.
pub fn catalogue_ids(catalogue: &[Value]) -> Vec<String> {
    catalogue
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The sandbox mode a client *sends*, from the policy the vendor *reports*.
///
/// **The spelling asymmetry is real and it is why a read-back never matches.** A client
/// sends `read-only` / `workspace-write` / `danger-full-access`; the vendor reports
/// `{"type": "readOnly"}` / `{"type": "workspaceWrite"}` / `{"type": "dangerFullAccess"}`.
/// So `current_value` has to be translated back into the sent spelling, or the picker can
/// never show a checkmark on any row — the failure `codex::SANDBOX_MODES` warns about,
/// here in the one function that can prevent it.
///
/// `externalSandbox` is a fourth variant the schema declares and this adapter does not
/// offer, so it maps to `None`: reporting it as one of the three would put a checkmark on
/// a row the session is not running under.
pub fn sandbox_mode_from_policy(policy: &Value) -> Option<&'static str> {
    match policy.get("type").and_then(Value::as_str)? {
        "readOnly" => Some("read-only"),
        "workspaceWrite" => Some("workspace-write"),
        "dangerFullAccess" => Some("danger-full-access"),
        _ => None,
    }
}

/// The `sandboxPolicy` object for a mode a client sent.
///
/// **`thread/start` takes `sandbox` as a bare string and `thread/settings/update` takes
/// `sandboxPolicy` as an *object*** — measured: the schema types the latter as
/// `SandboxPolicy`, a `oneOf` of objects keyed by `type`. Sending the string form to the
/// update method is a `-32600`, and sending the object form to `thread/start` likewise.
/// Two spellings of one concept, in one API, which is why this conversion is a named
/// function rather than an inline `json!`.
pub fn sandbox_policy(mode: &str) -> Option<Value> {
    let tag = match mode {
        "read-only" => "readOnly",
        "workspace-write" => "workspaceWrite",
        "danger-full-access" => "dangerFullAccess",
        _ => return None,
    };
    Some(json!({"type": tag}))
}

/// The parameters `thread/start` takes, built from a [`sylph_protocol::RunSpec`].
///
/// **`thread/start` has no `threadId` parameter at all** — the id comes back at
/// `result.thread.id` — which is how "codex assigns its own session id" follows from
/// the shape of the API rather than from observation. So a `RunSpec` asking to dictate
/// an id cannot be honoured here, and that has to be an error rather than a silently
/// ignored field.
/// So a [`RequestedNativeId::Dictate`] is refused here rather than ignored, and the
/// method to call is the caller's second answer: [`RequestedNativeId::Resume`] is
/// `thread/resume` with a `threadId`, and both take the same option members otherwise.
pub fn thread_start_params(spec: &RunSpec) -> Result<(&'static str, Value)> {
    let (method, mut params) = match &spec.native_id {
        RequestedNativeId::New => (method::THREAD_START, json!({})),
        // **`thread/resume` is also the subscribe.** A client that starts a thread and
        // never resumes sees zero `item/*` events; for `run` the start implies the
        // subscription, so only this branch names the method explicitly.
        RequestedNativeId::Resume(id) => (
            method::THREAD_RESUME,
            json!({ "threadId": id.as_str() }),
        ),
        RequestedNativeId::Dictate(id) => {
            // Refused rather than dropped: a server that believed it named the session
            // would hold an id no rollout has, so every later resume, prune, and
            // adoption of that session would ask an unanswerable question.
            return Err(anyhow!(
                "codex assigns its own thread id and reports it at result.thread.id, so \
                 the dictated id {} cannot be honoured; `dictates_native_id` is false \
                 for this agent",
                id.as_str()
            ));
        }
    };

    let object = params
        .as_object_mut()
        .expect("built from a json! object literal");
    object.insert("cwd".into(), json!(spec.workdir));

    // Spawn flags. Each one is also a runtime setting, and it is set in exactly one
    // place: **the `claude` `--permission-prompt-tool` lesson** is that a launch flag
    // gating a runtime-mutable value bets the user never changes it. Here they are
    // thread parameters rather than argv, so there is no second place to disagree.
    for (flag, key) in [
        ("model", "model"),
        ("effort", "effort"),
        ("approvalPolicy", "approvalPolicy"),
    ] {
        if let Some(value) = spec.flags.get(flag) {
            object.insert(key.into(), json!(value));
        }
    }
    // **`sandbox` here is the hyphenated string, unlike the update method's object.**
    // See [`sandbox_policy`].
    if let Some(sandbox) = spec.flags.get("sandbox") {
        object.insert("sandbox".into(), json!(sandbox));
    }

    // **`developerInstructions`, never `baseInstructions`.** Measured:
    // `session_meta.base_instructions.text` is 17730 characters of the vendor's own
    // system prompt, and that parameter *replaces* it — so injecting identity there
    // would delete the agent to introduce Sylph. This one is additive, which is the
    // same property `claude`'s `--append-system-prompt-file` has and for the same
    // reason.
    if spec.inject_identity {
        if let Some(preamble) = &spec.identity_preamble {
            object.insert("developerInstructions".into(), json!(preamble));
        }
    }

    Ok((method, params))
}

/// Whether a config write is one this adapter validates before sending, and against what.
///
/// **This exists because the vendor validates inconsistently, and the two behaviours are
/// indistinguishable to a caller that only checks for an error.** Measured on 0.150.1
/// across `s04_settings.jsonl`:
///
/// | Field | Vendor behaviour | Where validation lives |
/// | --- | --- | --- |
/// | `approvalPolicy` | refuses, listing variants | vendor (also checked here, for a better message) |
/// | `personality` | refuses, listing variants | vendor |
/// | `permissions` | refuses, naming the profile | vendor |
/// | `effort` | **accepts `"banana"`**, echoes it as current, fails at turn time | **here** |
/// | `model` | **accepts `no-such-model-xyz"`**, echoes it as current | **here** |
/// | *unknown name* | **accepts silently, `result: {}`, no notification** | **here** |
///
/// The last row is why an unknown option must be refused adapter-side with
/// [`sylph_protocol::RefusalCode::UnsupportedField`]: the vendor emits no such signal at
/// all, so a client would otherwise see a write accepted and nothing change — the
/// "refused write indistinguishable from an applied one" failure in its purest form.
pub fn refuse_unknown(id: &str) -> AdapterError {
    AdapterError::unsupported_field(format!(
        "'{id}' is not a setting a codex session has; codex accepts an unknown option \
         name silently with an empty result, so this is refused here rather than \
         reported as applied"
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

    /// The effort ladder is read from `reasoningEffort`, which is the key the real
    /// payload uses.
    ///
    /// Asserts on the **values it should have found**, against the catalogue entry
    /// printed in `docs/measurements/codex/captures/s01_initialize.jsonl` — not that the
    /// call returned. Reading `effort` or `id` here yields an empty ladder, which is
    /// indistinguishable from a model that offers no efforts.
    #[test]
    fn an_effort_ladder_is_read_from_the_key_the_vendor_uses() {
        let catalogue = vec![serde_json::json!({
            "id": "gpt-5.6-sol",
            "model": "gpt-5.6-sol",
            "displayName": "GPT-5.6-Sol",
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low", "description": "Fast responses"},
                {"reasoningEffort": "medium", "description": "Balances speed"},
                {"reasoningEffort": "high", "description": "Greater depth"},
                {"reasoningEffort": "xhigh", "description": "Extra high"},
                {"reasoningEffort": "max", "description": "Maximum depth"},
                {"reasoningEffort": "ultra", "description": "Automatic task delegation"},
            ],
            "defaultReasoningEffort": "low",
        })];

        assert_eq!(
            supported_efforts(&catalogue, "gpt-5.6-sol").as_deref(),
            Some(["low", "medium", "high", "xhigh", "max", "ultra"].map(String::from).as_slice()),
        );
        // A model the catalogue does not hold is `None`, which is not the same fact as
        // "offers no efforts" — the caller has to refuse it rather than offer an empty
        // ladder.
        assert!(supported_efforts(&catalogue, "no-such-model-xyz").is_none());
        assert_eq!(catalogue_ids(&catalogue), vec!["gpt-5.6-sol".to_string()]);
    }

    /// The sandbox spelling asymmetry, both directions.
    ///
    /// A client sends `workspace-write` and the vendor reports `{"type":"workspaceWrite"}`,
    /// so a `current_value` taken from the report without this translation matches no
    /// offered row and the picker can never show a checkmark.
    #[test]
    fn a_sandbox_policy_round_trips_between_the_two_spellings() {
        for (sent, reported) in [
            ("read-only", "readOnly"),
            ("workspace-write", "workspaceWrite"),
            ("danger-full-access", "dangerFullAccess"),
        ] {
            let policy = sandbox_policy(sent).expect("an offered mode has a policy");
            assert_eq!(policy["type"], reported, "{sent} sends {reported}");
            assert_eq!(
                sandbox_mode_from_policy(&policy),
                Some(sent),
                "and {reported} reads back as {sent}",
            );
        }

        // The real reported shape carries more than `type`, and the extra members must
        // not defeat the match.
        let measured = serde_json::json!({
            "type": "workspaceWrite",
            "writableRoots": [],
            "networkAccess": false,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        });
        assert_eq!(sandbox_mode_from_policy(&measured), Some("workspace-write"));

        // A fourth variant this adapter does not offer reports as unknown rather than as
        // one of the three, so no checkmark lands on a mode the session is not in.
        assert!(sandbox_mode_from_policy(&serde_json::json!({"type": "externalSandbox"})).is_none());
        assert!(sandbox_policy("banana").is_none());
    }

    /// Identity goes in the additive parameter, and a dictated id is refused.
    #[test]
    fn thread_params_inject_additively_and_refuse_a_dictated_id() {
        use sylph_protocol::NativeId;

        let base = |native_id| RunSpec {
            workdir: "/w".into(),
            native_id,
            inject_identity: true,
            identity_preamble: Some("I am Sylph.".into()),
            flags: [("effort".to_string(), "high".to_string())].into_iter().collect(),
            permission_mode: None,
        };

        let (method, params) = thread_start_params(&base(RequestedNativeId::New)).unwrap();
        assert_eq!(method, method::THREAD_START);
        assert_eq!(params["cwd"], "/w");
        assert_eq!(params["effort"], "high");
        // **Additive, not replacing.** `baseInstructions` would delete 17730 characters
        // of the vendor's own system prompt.
        assert_eq!(params["developerInstructions"], "I am Sylph.");
        assert!(params.get("baseInstructions").is_none());
        // No `threadId` on a start: the id comes back at `result.thread.id`.
        assert!(params.get("threadId").is_none());

        let resumed = NativeId::new("01a0460c-c359-7ed3-9e13-212c0a33d6c0");
        let (method, params) =
            thread_start_params(&base(RequestedNativeId::Resume(resumed.clone()))).unwrap();
        assert_eq!(method, method::THREAD_RESUME);
        assert_eq!(params["threadId"], resumed.as_str());

        // Refused, not silently ignored: a server holding an id no rollout has could
        // never resume, prune, or adopt that session.
        let error = thread_start_params(&base(RequestedNativeId::Dictate(resumed)))
            .expect_err("codex cannot honour a dictated id");
        assert!(
            error.to_string().contains("dictates_native_id"),
            "the error must name the fact that explains it: {error}"
        );
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
