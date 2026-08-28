//! The `codex` adapter. **New work rather than a port**: there has never been a codex
//! adapter in this repo, so there was no Python to carry across — and no ported defects
//! either. Every shape below was read off a printed payload.
//!
//! `docs/agents.md` § `codex` is the governing spec and `docs/measurements/codex/` holds
//! the 15 re-runnable probes plus the redacted frames behind it. **Where this module
//! disagrees with that document, the disagreement is stated at the code**: six of its
//! claims were re-measured while writing this and four of them were wrong. They are
//! collected at [`docs_corrections`] so a reader arriving from the document lands on the
//! measurement rather than on prose.
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
pub mod native;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, EventStream, FlagSpec, NativeId, NativeSummary,
    RequestedNativeId, RunSpec, Session, StoreFilter,
};

/// The name this adapter answers to, and the `PATH` binary that decides availability.
pub const AGENT_NAME: &str = "codex";

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

/// What this module measured that `docs/agents.md` § `codex` gets wrong.
///
/// Recorded as code rather than left in a commit message, because each one is a branch a
/// reader would otherwise write from the document and each one fails silently.
///
/// 1. **The era discriminant is `session_meta.payload.history_mode`, not `cli_version`.**
///    The document says the vocabulary changed "around cli_version 0.147". Measured over
///    1258 rollouts, 0.147.0 appears as `paginated` on 42 files and `legacy` on 21, and
///    `s13_rollout_sample.jsonl` is **0.150.1 with `history_mode: "legacy"` and zero
///    `item_completed`** — the newest version in the store, writing the old vocabulary. See
///    [`native::Era`].
/// 2. **The two content streams are mutually exclusive per file**, so "both must be read" is
///    right about the store and wrong about a record: 72 files hold `item_completed` and no
///    `event_msg/agent_message`, 30 hold the reverse, and only **10 of 1258** hold both —
///    all of them cli_version 0.104–0.115, the *oldest*, not the newest. Reading both in a
///    paginated file double-counts. See [`native::Era`].
/// 3. **`SubAgentActivity.kind` is `started` or `interacted` only.** The document lists
///    `interrupted` and `completed`; both occur **zero** times in 1258 rollouts — the
///    declared-only-event trap in its usual form. So a delegation has no close record here.
/// 4. **`agent_thread_id` is the *other party*, not the child.** In a parent's rollout it
///    names the child; in the child's own rollout, `kind: "interacted"` names the **parent**
///    (`agent_path: "/root"`). Mapping it to `span` unconditionally inverts the edge. See
///    [`native::delegation_edge`].
/// 5. **`CollabAgentToolCall` carries no lineage.** The document presents it as delegation
///    structure; both real instances have `receiver_thread_ids: []` and `agents_states: {}`
///    with `tool: "wait"`, and its `status: "completed"` is the *wait's* — the paired output
///    says `{"message":"Wait timed out.","timed_out":true}`. It marks a barrier.
/// 6. **`custom_tool_call` did not supersede `function_call`.** 26 of the newest 140 files
///    hold both, and `function_call` is still how every collaboration tool is issued.
/// 7. **148 client request methods is 153.** Counted from the schema's own `ClientRequest`
///    union.
///
/// And one the document does not mention at all, which is the most consequential for a
/// delegation card: **from 0.147 the `spawn_agent` brief is ciphertext.** See
/// [`native::BRIEF_IS_CIPHERTEXT`].
pub const fn docs_corrections() -> &'static str {
    "seven measured corrections; each is stated at the code that acts on it"
}

/// The `codex` adapter.
#[derive(Debug, Clone, Default)]
pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
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
                choices: SANDBOX_MODES.iter().map(|value| value.to_string()).collect(),
                description: "Filesystem and network sandbox.".to_string(),
            },
        );
        flags
    }

    /// The argv one `run` would use, split out from the spawn so it is assertable without a
    /// process — the same reason `claude`'s is.
    ///
    /// # `app-server --listen unix://` is the door, and `remote-control` is not
    ///
    /// *Measured on 0.150.1:* `remote-control` is gated twice — `remote control requires
    /// ChatGPT authentication; API key auth is not supported`, **plus** a managed standalone
    /// install — while `app-server --listen unix://PATH` has neither gate. Its pairing flow
    /// is still the shape `docs/environments.md` models itself on; it is simply not the
    /// transport.
    ///
    /// Three constraints on that socket that belong here rather than in a probe's notes,
    /// because each one closes the connection with no usable diagnosis:
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
        let mut argv: Vec<String> = vec![AGENT_NAME.to_string(), "app-server".to_string()];
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

        argv.push("--listen".into());
        argv.push(LISTEN_PATH_PLACEHOLDER.into());
        (argv, warnings)
    }
}

/// Where the socket path lands in an argv.
///
/// A marker rather than an empty string, for the reason `claude`'s preamble placeholder is
/// one: a caller that forgets to substitute fails visibly instead of asking the vendor to
/// listen on `""`. The path itself must be canonical — see [`CodexAdapter::argv_for`].
pub const LISTEN_PATH_PLACEHOLDER: &str = "unix://<sylph-codex-socket>";

#[async_trait]
impl Adapter for CodexAdapter {
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
            // existence — `AGENTS.md` § "A type in a vendor's event union is not a promise
            // that anything emits it" applies to methods as much as to events, which is why
            // only the measured three are claimed out of the declared 153.
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
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

    async fn run(&self, _spec: RunSpec) -> Result<Box<dyn Session>> {
        // Deferred exactly as `claude` and `pi` defer it, with the same wording, because
        // the blocker is the same one: a live session holds the vendor's transport and
        // there is nothing on this side of the tree to hold it yet.
        Err(anyhow!(
            "codex `run` is not wired yet: the live session holds the vendor's app-server \
             socket and lands with the envagent in step 4 of docs/roadmap.md \
             § Implementation order"
        ))
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
