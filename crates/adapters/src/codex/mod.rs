//! The `codex` adapter. **New work rather than a port**: there has never been a codex
//! adapter in this repo, so there was no Python to carry across — and no ported defects
//! either. Every shape below was read off a printed payload.
//!
//! `docs/agents.md` § `codex` is the governing spec, and `docs/measurements/codex/` holds
//! the re-runnable probes and redacted frames behind it. [`measured_facts`] indexes the
//! vendor facts this module is built on, each stated at the code that acts on it — they
//! are the ones whose violation is silent, so a reader arriving from the document lands
//! on the measurement rather than on prose.
//!
//! # One file, three halves
//!
//! [`appserver`] holds the live control surface — the JSON-RPC method names, the approval
//! decision enums, and the config validation this vendor makes the adapter's job.
//! [`native`] reads `~/.codex/sessions/`. The split is the one `claude` and `pi` already
//! make, and for the same measured reason: a mistranslation does not raise, so the pure
//! half has to be assertable without a process.
//!
//! # Three event vocabularies, and nothing shared between them
//!
//! This vendor spells the same concepts three ways, and *an adapter reading one while
//! mapping names learned from another gets nothing, silently*:
//!
//! | Source | Spelling | Example |
//! | --- | --- | --- |
//! | app-server | slash + camelCase | `item/completed`, `commandExecution` |
//! | `exec --json` | dot + snake_case | `item.completed`, `command_execution` |
//! | rollout JSONL | `type` / `payload` | `response_item` / `custom_tool_call` |
//! | *projection SQLite* | camelCase, re-shaped | `commandExecution`, `agentThreadId` |
//!
//! There are in fact **four**, which is the first correction: the `thread_history_*.sqlite`
//! projection re-cases and re-shapes the same items again (`item_json` flattens
//! `AgentMessage.content[{type:"Text"}]` to a scalar `text`, and joins
//! `CommandExecution.command` from a `Vec<String>` into one string), so nothing there can
//! be deserialized with the rollout's types.
//!
//! **So no name constant is shared between the halves of this module.** [`appserver`] names
//! its own methods and item types; [`native`] names its own record and item types; and the
//! `exec --json` vocabulary is deliberately absent, because nothing here drives that
//! surface. A shared table would be one rename away from reading a legitimate payload as
//! absent.
//!
//! # What this adapter does not do
//!
//! No `commands`: codex has **no command concept at all**, so the facet is empty and a
//! client must not present that as a failure (`sylph_protocol::LinkFacets` — an empty
//! capability surface means "Sylph cannot reach it" only where reaching is possible).

pub mod appserver;
pub mod live;
pub mod native;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, EventStream, Facet, FlagSpec, LinkFacets, NativeId,
    NativeSummary, RequestedNativeId, RunSpec, Session, StoreFilter, UnavailableReason,
};

/// The name this adapter answers to, and the `PATH` binary that decides availability.
pub const AGENT_NAME: &str = "codex";

/// Minimum Codex CLI version covered by the adapter's measurements.
///
/// The capability and native-store measurements in `docs/agents.md` use codex-cli
/// 0.150.1, so this is the minimum version the adapter can currently verify.
pub const VERSION_FLOOR: &str = "0.150.1";

/// Where this vendor keeps its rollouts, `$HOME`-relative — **the third fact, and not
/// derivable from either of the two above.**
///
/// `claude`'s module states one constant because for that vendor the agent name and the
/// binary are the same string and the store is under a directory named after it. Here the
/// store root is `~/.codex/sessions` while the *agent* is `codex` and the *binary* is
/// `codex`, and nothing in either spelling says `sessions` — so it is named separately
/// rather than assembled from a name that happens to match today.
pub const STORE_SEGMENTS: &[&str] = &[".codex", "sessions"];

/// What `codex exec`/`app-server` accept as an approval policy, from the vendor's own
/// generated schema (`AskForApproval`).
///
/// **Enum-typed in the schema, so the vendor refuses a bad value and lists the valid ones**
/// — measured: `approvalPolicy: "sometimes"` answered
/// `unknown variant 'sometimes', expected one of 'untrusted', 'on-request', 'granular',
/// 'never'`. That maps to [`sylph_protocol::RefusalCode::InvalidValue`], and it is the
/// *good* case: the two settings below behave the opposite way.
///
/// `granular` is deliberately absent from what this adapter offers. It is a real variant and
/// it is not a string — the schema types it as an object carrying five booleans
/// (`mcp_elicitations`, `rules`, `sandbox_approval`, `request_permissions`,
/// `skill_approval`) — so offering it as a picker row would produce a value the vendor
/// refuses. A `ConfigChoice.value` is a token to echo back, never a shape to assemble.
pub const APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];

/// The `personality` values, from the schema's `Personality` enum. Also enum-typed, so also
/// refused with its variants listed.
pub const PERSONALITIES: &[&str] = &["none", "friendly", "pragmatic"];

/// The sandbox modes, from the schema's `SandboxMode`.
///
/// Note the spelling asymmetry, which is a real trap and not a typo: a client *sends*
/// `read-only` / `workspace-write` / `danger-full-access`, and the vendor *reports* the
/// resulting policy back as `{"type": "readOnly"}` / `{"type": "workspaceWrite"}`. So a
/// read-back that compares against what was sent never matches, and an adapter reporting
/// `currentValue` from the request rather than from the confirmation would look correct
/// while being unable to ever show a checkmark.
pub const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];

/// The reasoning efforts this vendor's frontier model enumerated, kept **only** as the
/// spawn-time fallback.
///
/// **`effort` is where this vendor stops validating, and that is the measurement that
/// decides where validation lives.** The schema types `ReasoningEffort` as a bare non-empty
/// string, so — measured on 0.150.1 — `effort: "banana"` is *accepted*, echoed back by
/// `thread/settings/updated` as `"effort": "banana"` **and** as
/// `collaborationMode.settings.reasoning_effort`, and fails only when a turn runs (`level
/// "banana" not supported, valid levels: low, medium, high, xhigh, max`). A nonexistent
/// model behaves identically, and an **unknown option name is accepted silently with
/// `result: {}`** — so codex offers no [`sylph_protocol::RefusalCode::UnsupportedField`]
/// signal at all, and anything not enum-typed must be caught adapter-side or it is lost.
///
/// So the authority for a live session is [`appserver::supported_efforts`], read from
/// `model/list`'s `supportedReasoningEfforts` — **per model**, because that is how the
/// vendor reports it. This list is what a *spawn* offers before any session exists to ask,
/// and it is the one place a hardcoded effort list is defensible.
///
/// `ultra` is included because it was enumerated by the real catalogue, and its description
/// says it is not merely "more" — *"Maximum reasoning with automatic task delegation"*. The
/// deprecated `multiAgentMode` field's own doc says to use it for that, so it is a
/// behavioural switch wearing an effort's clothing; offering it is right, and so is not
/// treating it as the top of a ladder.
pub const SPAWN_EFFORT_CHOICES: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

/// The measured facts this adapter is built on, each of which a reader would otherwise get
/// wrong from the vendor's own types.
///
/// Stated here because every one of them **fails silently**: the parse succeeds and produces
/// a plausible record. `docs/agents.md` § `codex` carries them as prose; this is the
/// index from the code side.
///
/// 1. **The era discriminant is `session_meta.payload.history_mode`, never `cli_version`.**
///    One CLI version appears in both eras, and the store holds files written by the newest
///    version present in the *old* vocabulary — so a version comparison picks the wrong
///    parser on a file the vendor still produces. Absence of the field reads as legacy.
///    See [`native::Era`].
/// 2. **The two content streams are mutually exclusive per file**, so "both must be read" is
///    right about the store and wrong about a record. Reading both in a paginated file
///    double-counts every assistant message. Only the oldest files hold both.
///    See [`native::Era`].
/// 3. **`SubAgentActivity.kind` is `started` or `interacted` only.** `interrupted` and
///    `completed` are declared and never emitted, so **a delegation has no close record
///    here** and its end must come from the turn containing it.
/// 4. **`agent_thread_id` is the *other party*, not the child.** In a parent's rollout it
///    names the child; in the child's own rollout an `interacted` record names the
///    **parent**. Mapping it to `span` unconditionally inverts the edge, producing a tree
///    that is backwards rather than incomplete — and nothing downstream can detect that.
///    See [`native::delegation_edge`].
/// 5. **`CollabAgentToolCall` carries no lineage.** Its `receiver_thread_ids` and
///    `agents_states` are empty and its `status` is a *wait's*, not a delegate's. It marks
///    a barrier.
/// 6. **`custom_tool_call` does not supersede `function_call`.** Both appear in the same
///    files, and `function_call` is still how every collaboration tool is issued, so
///    switching shapes on a version stops reading the calls that matter.
/// 7. **The `spawn_agent` brief is ciphertext** in current versions, so a delegation's name
///    is unrecoverable from the parent's record and must be omitted rather than guessed.
///    See [`native::BRIEF_IS_CIPHERTEXT`].
pub const fn measured_facts() -> &'static str {
    "seven measured facts; each is stated at the code that acts on it"
}

/// The `codex` adapter.
#[derive(Debug, Clone, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Facets for a native-store-only watcher. An envagent may replace the
    /// control facet when it has authenticated a live app-server socket.
    pub fn native_link_facets() -> LinkFacets {
        LinkFacets {
            observe: Facet::available(),
            control: Facet::unavailable(UnavailableReason::NotOursToDrive),
        }
    }

    /// Whether the vendor CLI is on this machine's `PATH`.
    pub fn is_installed() -> bool {
        crate::on_path(AGENT_NAME)
    }

    /// The spawn-time options this agent declares.
    ///
    /// `model` lists no choices for the same reason `claude`'s does not: the real catalogue
    /// is account-dependent and only knowable once the CLI has answered `model/list`, so an
    /// empty list means "free-form, ask the session" rather than "no models".
    fn flags() -> sylph_protocol::FlagSchema {
        let mut flags = sylph_protocol::FlagSchema::new();
        flags.insert(
            "model".to_string(),
            FlagSpec {
                choices: vec![],
                description: "Model for this session.".to_string(),
            },
        );
        flags.insert(
            "effort".to_string(),
            FlagSpec {
                choices: SPAWN_EFFORT_CHOICES
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                description: "Reasoning effort.".to_string(),
            },
        );
        flags.insert(
            "approvalPolicy".to_string(),
            FlagSpec {
                choices: APPROVAL_POLICIES
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                description: "When to ask before running a command.".to_string(),
            },
        );
        flags.insert(
            "sandbox".to_string(),
            FlagSpec {
                choices: SANDBOX_MODES
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                description: "Filesystem and network sandbox.".to_string(),
            },
        );
        flags
    }

    /// The argv `run` uses, split out from the spawn so it is assertable without a process —
    /// the same reason `claude`'s is.
    ///
    /// # `app-server` on stdio is what `run` speaks, and the socket is a different mode
    ///
    /// **This function used to append `--listen unix://<placeholder>` unconditionally, and
    /// that was wrong for `run`.** The two transports carry the same JSON-RPC vocabulary
    /// under different framing: stdio is NDJSON, and the unix listener is WebSocket. `run`
    /// owns the process it started, so it already has its stdio — reaching for the socket
    /// would mean a hand-written RFC6455 upgrade (below) to talk to a child this adapter
    /// spawned. So the socket belongs to the case where the daemon is *somebody else's*,
    /// which is an `adopt`/`watch` concern and not this one.
    ///
    /// What the socket measurement settles for `run` is only the negative: **`remote-control`
    /// is not the door.** *Measured on 0.150.1:* it is gated twice — `remote control requires
    /// ChatGPT authentication; API key auth is not supported`, **plus** a managed standalone
    /// install — so an envagent reaching for it would work on one class of account and fail
    /// on another with a message about authentication rather than about the transport. Its
    /// pairing flow is still the shape `docs/environments.md` models itself on.
    ///
    /// Three constraints on that socket are kept here rather than in a probe's notes, for
    /// whoever wires the second mode, because each one closes the connection with no usable
    /// diagnosis:
    ///
    /// - **The unix listener speaks WebSocket; stdio speaks NDJSON.** Same JSON-RPC
    ///   vocabulary, different framing. An NDJSON frame on the socket gets the connection
    ///   closed with *no data at all* and `failed to upgrade control socket websocket
    ///   connection: … invalid token` in the daemon's log. The upgrade is accepted on any
    ///   path, so the path carries no meaning.
    /// - **codex's HTTP parser rejects a mainstream websocket client.** The `websockets`
    ///   package (16.0) fails the handshake on the extra headers it sends; an upgrade
    ///   carrying exactly `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`,
    ///   `Sec-WebSocket-Key` is answered `101`. That is a real constraint on the envagent.
    /// - **The listen path's parent must be a real directory.** On macOS `/tmp` is a symlink
    ///   to `/private/tmp` and codex refuses with `socket directory path exists and is not a
    ///   directory: /tmp`. Canonicalize first; codex creates the parent `0700` and the
    ///   socket `0600`.
    ///
    /// Returns the argv **and** any warning worth logging, rather than logging inside:
    /// substituting a policy silently is exactly the help an adapter must not give.
    pub fn argv_for(spec: &RunSpec) -> (Vec<String>, Vec<String>) {
        let argv: Vec<String> = vec![AGENT_NAME.to_string(), "app-server".to_string()];
        let mut warnings = Vec::new();

        // Identity is the vendor's, **structurally**: `thread/start` has no `threadId`
        // parameter at all and the id comes back at `result.thread.id`. So there is no flag
        // to carry a dictated id, and a `Dictate` reaching here is a protocol value this
        // vendor cannot honour — reported rather than silently ignored, because a server
        // that believes it named the session would then hold an id no rollout has.
        match &spec.native_id {
            RequestedNativeId::New => {}
            RequestedNativeId::Resume(id) => {
                // Resume is a `thread/resume` request over the socket rather than an argv
                // entry, and that is the whole reason `RequestedNativeId` exists: on
                // `claude` dictate and resume are two flags, here one is impossible and the
                // other is a method call. An adapter that could not tell them apart could
                // not choose.
                warnings.push(format!(
                    "codex resumes over the app-server, not the command line; \
                     thread/resume {} runs after the handshake",
                    id.as_str()
                ));
            }
            RequestedNativeId::Dictate(id) => {
                warnings.push(format!(
                    "codex assigns its own thread id and reports it, so the dictated id {} \
                     cannot be honoured; `dictates_native_id` is false for this agent",
                    id.as_str()
                ));
            }
        }

        // Everything else this vendor takes as *thread* parameters rather than as argv:
        // `cwd`, `approvalPolicy`, `sandbox`, `model`, `effort` all belong to `thread/start`
        // (`appserver::thread_start_params`). Passing them twice is how a launch flag and a
        // runtime value come to disagree — the `claude` `--permission-prompt-tool` lesson,
        // avoided here by there being exactly one place each value is set.
        if spec.inject_identity && spec.identity_preamble.is_some() {
            // `thread/start` takes `developerInstructions` **and** `baseInstructions`, and
            // the distinction matters: `baseInstructions` *replaces* the agent's entire
            // system prompt (measured — `session_meta.base_instructions.text` is 17730
            // characters of the vendor's own prompt), while `developerInstructions` is
            // additive. Identity injection must be additive, or Sylph's preamble would
            // delete the agent.
            warnings.push(
                "codex takes the identity preamble as thread/start developerInstructions, \
                 not as an argv flag"
                    .to_string(),
            );
        }

        // Nothing further. Every other value this vendor takes is a `thread/start`
        // parameter (`appserver::thread_start_params`), which is what keeps a launch flag
        // and a runtime value from having two places to disagree.
        (argv, warnings)
    }
}

/// Where a socket path would land in an argv, for the mode that attaches to a daemon
/// somebody else started.
///
/// A marker rather than an empty string, for the reason `claude`'s preamble placeholder is
/// one: a caller that forgets to substitute fails visibly instead of asking the vendor to
/// listen on `""`. The path itself must be canonical — see [`CodexAdapter::argv_for`].
///
/// **Not used by [`CodexAdapter::argv_for`]**, which builds the stdio argv `run` spawns.
/// Kept because the constraints on that path are measured and would otherwise have to be
/// rediscovered.
pub const LISTEN_PATH_PLACEHOLDER: &str = "unix://<sylph-codex-socket>";

#[async_trait]
impl Adapter for CodexAdapter {
    fn version_floor(&self) -> Option<&'static str> {
        Some(VERSION_FLOOR)
    }

    async fn probe(&self) -> Result<AgentInfo> {
        if !Self::is_installed() {
            // Names the CLI, because that is the whole fix.
            return Err(anyhow!(
                "the `codex` CLI is not on this machine's PATH; install codex-cli to use \
                 this agent"
            ));
        }
        Ok(AgentInfo {
            name: AGENT_NAME.to_string(),
            version: version().await,
            flags: Self::flags(),
            // **The control facet declares itself full, and this is the widest of the
            // three.** *Measured on 0.150.1:* the app-server's own generated schema
            // declares 153 client request methods, and `turn/interrupt` and `turn/steer`
            // were both watched driving a live turn a second client had not subscribed to.
            // So all three capabilities are real rather than inferred from a method's
            // existence — a declared type is not a promise that anything emits it
            // (`docs/session-protocol.md` § Three rules about reading a vendor's answer),
            // which is why only the measured three are claimed out of the declared 153.
            //
            // `Queue` is claimed on the same evidence standard: `thread/queue/add` was
            // *measured*, and its refusal naming `clientUserMessageId` is what proves the
            // vendor reached the handler rather than ignoring the method. Its five sibling
            // verbs — `list`, `update`, `delete`, `reorder`, `start` — are declared and not
            // claimed, because `Session` has one queueing operation.
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
                Capability::Queue,
                Capability::SetConfig,
            ],
            // *Measured:* `thread/start` has no `threadId` parameter, so this is structural
            // rather than observed. The id arrives at `result.thread.id`.
            dictates_native_id: false,
            // Flushed at item completion, verified mid-turn: 18 distinct rollout appends
            // over a 26.5 s turn, the first at 0.1 s.
            native_fidelity: native::NATIVE_FIDELITY,
        })
    }

    /// Hold a live session over `codex app-server`.
    ///
    /// **This vendor reports its own id**, so the one thing this must not do is answer with
    /// a session whose `native_id` Sylph chose or synthesized — see [`live::run`].
    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
        if !Self::is_installed() {
            return Err(anyhow!(
                "the `codex` CLI is not on this machine's PATH; install codex-cli to use \
                 this agent"
            ));
        }
        live::run(spec).await
    }

    async fn adopt(&self, id: &NativeId) -> Result<EventStream> {
        native::adopt(id)
    }

    async fn watch(&self, id: &NativeId) -> Result<EventStream> {
        native::watch(id)
    }

    async fn enumerate(&self, filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        native::enumerate(filter)
    }
}

/// The CLI's own version, or `None` when it did not report one.
///
/// Never synthesized: a fabricated version would make an unsupported install look
/// supported.
async fn version() -> Option<String> {
    let output = tokio::process::Command::new(AGENT_NAME)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // `codex --version` answers `codex-cli 0.150.1`, so the version is the *last*
    // whitespace-separated token rather than the first — the opposite of `claude`, whose
    // `2.1.250 (Claude Code)` puts it first. Measured on both, which is why neither module
    // shares this function.
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next_back()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_store_observation_does_not_advertise_control() {
        let facets = CodexAdapter::native_link_facets();
        assert!(facets.observe.available);
        assert_eq!(
            facets.control.reason,
            Some(UnavailableReason::NotOursToDrive)
        );
    }
    use sylph_protocol::NativeId;

    fn spec(native_id: RequestedNativeId) -> RunSpec {
        RunSpec {
            workdir: "/w".into(),
            native_id,
            inject_identity: true,
            identity_preamble: Some("I am Sylph.".into()),
            flags: [
                ("model".to_string(), "gpt-5.6-sol".to_string()),
                ("effort".to_string(), "high".to_string()),
            ]
            .into_iter()
            .collect(),
            permission_mode: None,
        }
    }

    /// **The argv is `codex app-server` and nothing else.**
    ///
    /// This pins the correction: the function used to append `--listen unix://…`
    /// unconditionally, which is the WebSocket transport for attaching to a daemon somebody
    /// else started. `run` owns the process it spawned and speaks NDJSON on its stdio, so a
    /// `--listen` here would have made the vendor open a socket nothing connected to while
    /// the handshake went to a stdout the daemon does not answer on.
    #[test]
    fn the_argv_is_the_stdio_app_server_and_carries_no_settings() {
        let (argv, _) = CodexAdapter::argv_for(&spec(RequestedNativeId::New));
        assert_eq!(argv, vec!["codex".to_string(), "app-server".to_string()]);

        // Every value this vendor takes is a `thread/start` parameter, so none of them may
        // also appear on the command line — that is the `claude`
        // `--permission-prompt-tool` lesson, avoided here by there being one place each
        // value is set.
        for absent in ["--listen", "--model", "--effort", "-m", "resume"] {
            assert!(
                !argv.iter().any(|arg| arg == absent),
                "{absent} must not be on the command line: {argv:?}"
            );
        }
    }

    /// Resume and dictate are both reported as warnings rather than silently dropped, and
    /// they say different things — one is a method call after the handshake, the other is
    /// impossible.
    #[test]
    fn a_resume_is_deferred_to_the_socket_and_a_dictated_id_is_reported_impossible() {
        let id = NativeId::new("01a0460c-c359-7ed3-9e13-212c0a33d6c0");

        let (argv, warnings) = CodexAdapter::argv_for(&spec(RequestedNativeId::Resume(id.clone())));
        assert_eq!(argv, vec!["codex".to_string(), "app-server".to_string()]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("thread/resume")),
            "a resume must name the method that performs it: {warnings:?}"
        );

        let (_, warnings) = CodexAdapter::argv_for(&spec(RequestedNativeId::Dictate(id)));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("dictates_native_id")),
            "a dictated id must be reported as unhonourable, not ignored: {warnings:?}"
        );

        // Identity is reported as a thread parameter too, because it is additive there and
        // `baseInstructions` would replace the agent's whole system prompt.
        let (_, warnings) = CodexAdapter::argv_for(&spec(RequestedNativeId::New));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("developerInstructions")));
    }
}
