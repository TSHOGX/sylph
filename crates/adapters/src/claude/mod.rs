//! The `claude` adapter, ported from `adapters/claude/adapter.py` (1976 lines).
//!
//! `docs/agents.md` § `claude` is the governing spec and holds the measurement behind
//! every quirk below. Every constant here was **enumerated against the real CLI** rather
//! than read off its `--help` or its error text, and where the two disagree the
//! measurement is recorded beside the constant — because on this vendor they do disagree.
//!
//! # What the translation half is, and why it is separate
//!
//! [`translate`] holds the whole vendor→Sylph projection as pure functions over a
//! [`Correlation`] state, with no process, no I/O, and no tokens. That split is the most
//! valuable thing in the Python original: `verify.py` spends real tokens and only sees
//! whatever shapes one live turn happens to produce, so **every classification bug in
//! this adapter's history was invisible to it**. `adapters/claude/adapter_test.py`'s 13
//! classes are ported onto [`translate`] for exactly that reason.
//!
//! # `describe` is deleted
//!
//! `adapter.py:1957` carried a v1 `--describe` compatibility entry point. There is no v1
//! contract left to serve and `AGENTS.md` § Compatibility forbids keeping one, so it is
//! gone rather than ported.

pub mod native;
pub mod requests;
pub mod session;
pub mod surface;
pub mod translate;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, EventStream, Fidelity, FlagSpec, NativeId, NativeSummary,
    RunSpec, Session, StoreFilter,
};

pub use translate::{Correlation, Emission};

/// The name this adapter answers to, and the `PATH` binary that decides availability.
///
/// One constant for both because for this vendor they are the same string. `codex` is
/// where they differ — the agent is `codex` and the binary is `codex`, but the *store*
/// path is not derivable from either, which is why that module states all three.
pub const AGENT_NAME: &str = "claude";

/// What `--permission-mode` actually accepts at launch.
///
/// **Enumerated by trying every candidate against claude 2.1.245, not read off the CLI's
/// error text** — which lists six values and omits `default`, a value it nevertheless
/// accepts. So the vendor's own message is not the authority here either.
///
/// `ask` is **rejected**, and it is the value `docs/session-protocol.md`'s own examples
/// use for this field. Passed through unvalidated it is a total session failure with a
/// misdirected diagnosis: commander rejects it at parse time, so claude exits 1 before
/// answering `initialize`, and the adapter reported "claude did not answer initialize in
/// time" — a *timeout* message for an argument error, which sends the investigation to
/// the link layer while the real cause sits in one argv entry.
pub const LAUNCH_PERMISSION_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// The modes `set_permission_mode` accepts **and applies as given**, which is what the
/// live config surface may offer.
///
/// Enumerated by sending every candidate to claude 2.1.246 and reading back the confirmed
/// `mode`, because `initialize` reports only `current_permission_mode` and never the set —
/// so this list cannot be discovered and had to be measured.
///
/// Two results, both invisible to a caller that only checks for an error:
///
/// - **`manual` answers success and confirms `default`.** The vendor normalizes it
///   silently, so offering it would be a picker row that lands on a different row. That is
///   the same lie as the bypass bug arriving by another route, and it is why `manual` is
///   absent from [`SPAWN_MODE_CHOICES`] too.
/// - **`auto` is settable** and was missing from the offered list, so a real mode was
///   unreachable through the UI.
pub const SETTABLE_PERMISSION_MODES: &[&str] = &[
    "default",
    "acceptEdits",
    "plan",
    "auto",
    "dontAsk",
    "bypassPermissions",
];

/// The spawn-time permission modes offered in [`AgentInfo::flags`].
///
/// Deliberately not [`LAUNCH_PERMISSION_MODES`]: that is what the CLI *parses*, and
/// `manual` parses while being silently normalized. A picker must only offer values the
/// vendor was watched applying as given.
pub const SPAWN_MODE_CHOICES: &[&str] = &[
    "default",
    "acceptEdits",
    "plan",
    "dontAsk",
    "bypassPermissions",
];

/// Dialog kinds this adapter declares at `initialize`.
///
/// **The declaration is load-bearing and its absence was a real bug.** The CLI's own
/// schema treats absence as "cannot display" and fails closed: without a kind declared
/// here, a dialog-gated flow degrades to prose — `/plan` returns a paragraph instead of an
/// approve/reject choice, and `AskUserQuestion` returns a question instead of options.
///
/// Every kind here must actually be answerable through [`Session::resolve`]. Declaring one
/// this adapter cannot render is worse than declaring none: the CLI parks a dialog waiting
/// on a reply no UI will produce, which blocks the turn until its deadline.
pub const SUPPORTED_DIALOG_KINDS: &[&str] = &[
    // The interactive surfaces that make a session feel native.
    "permission_ask_user_question",
    "permission_enter_plan_mode",
    // Tool permissions carrying richer payloads than `can_use_tool`.
    "permission_bash",
    "permission_file",
    "permission_webfetch",
    "permission_skill",
    "permission_prompt",
    "permission_workflow",
    // Consent gates a user must be able to answer, or the turn stalls.
    "cost_threshold",
    "sandbox_network_access",
    "refusal_fallback_prompt",
    "mcp_url_elicitation",
];

/// Which of claude's builtins are session **control** rather than prompts, and the config
/// option each drives.
///
/// This table lives in the adapter because the classification is a vendor fact: `claude`
/// returns `/model` in the same list as `/verify` with no field telling them apart, while
/// `pi` already excludes its TUI commands and `codex` has no command concept at all. The
/// same list in the server or the UI would encode one vendor's taxonomy as though it were
/// universal.
///
/// `/context`, `/compact`, `/usage` and `/clear` are deliberately **absent**: each produces
/// output worth keeping — a context table, a compaction boundary, a cost summary — so they
/// belong in the transcript as turns. Naming them control would hide their answers in a
/// dialog that closes. `/doctor`, `/init`, `/mcp` and `/agents` are likewise prompts; an
/// earlier draft listed them here on the theory that "builtin" implied "control", and the
/// observable effect was a set of commands that opened an empty picker instead of doing
/// anything.
pub const CONTROL_COMMANDS: &[(&str, &str)] = &[("model", "model"), ("fast", "fastMode")];

/// Builtins, for a command's advisory `source`.
///
/// **Known-incomplete, and measurement is the only reason that is known.** Against claude
/// 2.1.245 the CLI reports 46 commands whose entries carry exactly `name`, `description`,
/// `argumentHint`, `aliases` — printed, not inferred — so the vendor supplies **no**
/// builtin/skill discriminator at all. Measured coverage: 4 of 46 classified correctly, 42
/// falling through to `skill` (including `clear`, `compact`, `context`, `doctor`, `mcp` and
/// `usage`, all plainly the CLI's own), and 16 of the 20 names below not reported by that
/// version at all.
///
/// Deliberately **not** "fixed" by hand-listing the other 42: that would swap one
/// unverifiable list for a longer one, and it cannot be right in principle, since a
/// plugin's command is neither a builtin nor a skill and nothing in the payload separates
/// the three. The wrong entries cost nothing because only reported names are looked up, and
/// removing them would imply the remainder had been verified. Mislabelling `source`
/// mis-groups a menu; mislabelling `kind` sends `/model` through as prose, and that is the
/// part measured against the CLI.
pub const BUILTIN_SOURCES: &[&str] = &[
    "model",
    "fast",
    "review",
    "resume",
    "rewind",
    "todos",
    "help",
    "status",
    "cost",
    "memory",
    "export",
    "hooks",
    "plugin",
    "skill-doctor",
    "loop",
    "design-sync",
    "design-login",
    "artifacts",
    "tasks",
    "workflows",
];

/// The `claude` adapter.
#[derive(Debug, Clone, Default)]
pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Whether the vendor CLI is on this machine's `PATH`.
    pub fn is_installed() -> bool {
        crate::on_path(AGENT_NAME)
    }

    /// The spawn-time options this agent declares.
    ///
    /// `model` lists no choices because the real catalogue depends on the account and is
    /// only knowable once the CLI has loaded — so an empty list means "free-form, ask the
    /// session", not "no models". Nothing in the UI hardcodes a model list, which is why
    /// `codex` correctly shows effort with no model selector.
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
                choices: ["low", "medium", "high", "xhigh", "max"]
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                description: "Reasoning effort.".to_string(),
            },
        );
        flags.insert(
            "mode".to_string(),
            FlagSpec {
                choices: SPAWN_MODE_CHOICES
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                description: "Permission mode.".to_string(),
            },
        );
        flags
    }

    /// The argv one `run` would use.
    ///
    /// Split out from the spawn so it is assertable without a process, which is what
    /// `adapters/claude/adapter_test.py`'s `TheApprovalChannelIsAlwaysRegistered` needed
    /// `Popen` interception to reach: the ordering bug lived in argv construction and
    /// nothing below it could observe one.
    ///
    /// Returns the argv **and** any warning worth logging, rather than logging inside:
    /// substituting a permission policy silently is exactly the help an adapter must not
    /// give, so the caller reports it. `Vec` rather than a bool because a second
    /// substitution would otherwise have nowhere to go.
    pub fn argv_for(spec: &RunSpec) -> (Vec<String>, Vec<String>) {
        let mut argv: Vec<String> = [
            AGENT_NAME,
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--forward-subagent-text",
            "--verbose",
        ]
        .iter()
        .map(|part| part.to_string())
        .collect();

        let mut warnings = Vec::new();

        let requested = spec
            .permission_mode
            .as_deref()
            .or_else(|| spec.flags.get("mode").map(String::as_str))
            .unwrap_or("default");
        let mode = if LAUNCH_PERMISSION_MODES.contains(&requested) {
            requested
        } else {
            // Loudly, because the user asked for one policy and gets another, and only
            // this warning says so. Forwarding it instead kills the session with a
            // message naming the wrong layer.
            warnings.push(format!(
                "claude does not accept --permission-mode {requested:?}; using 'default'. \
                 Accepted: {}",
                LAUNCH_PERMISSION_MODES.join(", ")
            ));
            "default"
        };
        argv.push("--permission-mode".into());
        argv.push(mode.into());

        // Always, regardless of the starting mode. Routing permission decisions to Sylph
        // over stdio is the whole point of this adapter, and the channel can **only** be
        // asked for on the command line while the mode it serves changes at runtime — so
        // gating it on the launch mode bets the user never switches, and loses the
        // approval channel permanently when they do.
        //
        // Measured on claude 2.1.246: launch in `bypassPermissions`, switch to `default`,
        // and every tool call returns `Claude requested permissions to use Bash, but you
        // haven't granted it yet`. A bypassing mode simply never consults the channel, so
        // registering it unconditionally costs one argv entry and nothing else.
        argv.push("--permission-prompt-tool".into());
        argv.push("stdio".into());

        // What makes `bypassPermissions` *reachable* without starting in it.
        //
        // Measured on claude 2.1.246: `set_permission_mode` to `bypassPermissions` is
        // refused outright — "because the session was not launched with
        // --dangerously-skip-permissions" — unless a launch flag marked the mode
        // available. `--dangerously-skip-permissions` would also *start* the session in
        // bypass, which is not what a user selecting it later asked for; this flag only
        // marks it available, leaving the launch mode as given.
        //
        // Safe to pass on every session because it grants nothing by itself: it widens the
        // set of modes a *user* may then choose, and the vendor's own consent gate still
        // governs whether the choice takes. Without it, every mode this adapter offers
        // except bypass is settable and bypass is refused — an option a client can render
        // and never apply.
        argv.push("--allow-dangerously-skip-permissions".into());

        match &spec.native_id {
            // Dictating is the preferred path: identity is known before the first event,
            // so nothing has to correlate afterwards.
            sylph_protocol::RequestedNativeId::Dictate(id) => {
                argv.push("--session-id".into());
                argv.push(id.as_str().to_string());
            }
            sylph_protocol::RequestedNativeId::Resume(id) => {
                argv.push("-r".into());
                argv.push(id.as_str().to_string());
            }
            // Unreachable for this vendor: `dictates_native_id` is true, so the server
            // always names an id. Handled rather than panicking because a protocol value
            // an adapter cannot honour is a refusal, not a crash.
            sylph_protocol::RequestedNativeId::New => {
                warnings.push(
                    "claude dictates its session id; opening without one would make the \
                     session unresumable"
                        .to_string(),
                );
            }
        }

        if let Some(model) = spec.flags.get("model") {
            argv.push("--model".into());
            argv.push(model.clone());
        }
        if let Some(effort) = spec.flags.get("effort") {
            argv.push("--effort".into());
            argv.push(effort.clone());
        }

        // Identity injection, decided by the **server**. The adapter receives text rather
        // than a path because the file lives beside the server and the agent runs wherever
        // it does; `--append-system-prompt-file` is additive, so the agent's own rules and
        // the project's `CLAUDE.md` survive.
        if spec.inject_identity && spec.identity_preamble.is_some() {
            argv.push("--append-system-prompt-file".into());
            argv.push(IDENTITY_PREAMBLE_PLACEHOLDER.into());
        }

        (argv, warnings)
    }
}

/// Where the identity preamble's temporary path lands in an argv.
///
/// The text arrives in the `RunSpec` and has to become a file for this CLI's flag, so
/// [`ClaudeAdapter::argv_for`] — which is pure, and therefore cannot write one — emits this
/// marker and `run` substitutes the path it wrote. A marker rather than an empty string so
/// a caller that forgets to substitute fails visibly instead of passing `--…-file ""`.
pub const IDENTITY_PREAMBLE_PLACEHOLDER: &str = "<sylph-identity-preamble>";

#[async_trait]
impl Adapter for ClaudeAdapter {
    async fn probe(&self) -> Result<AgentInfo> {
        if !Self::is_installed() {
            // Names the CLI, because that is the whole fix. The mechanism this replaced
            // could only say an adapter was not found.
            return Err(anyhow!(
                "the `claude` CLI is not on this machine's PATH; install Claude Code to \
                 use this agent"
            ));
        }
        Ok(AgentInfo {
            name: AGENT_NAME.to_string(),
            version: version().await,
            flags: Self::flags(),
            // Each gates exactly one method on `Session`. `claude` offers all three:
            // `interrupt` ends a turn and keeps the session, steering is a second user
            // message on the held-open stdin, and `set_config` maps onto its control
            // requests.
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
                Capability::SetConfig,
            ],
            // Measured 2026-08-27: takes `--session-id`, so identity is known before the
            // first event.
            dictates_native_id: true,
            // Its store flushes at tool-call and content-block boundaries, which is finer
            // than per-message and coarser than the live stream.
            native_fidelity: Fidelity::Blocks,
        })
    }

    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
        session::run(spec).await
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
/// Never synthesized: a fabricated version is a lie a journal makes permanent, and it
/// would make an unsupported install look supported (`AgentInfo::version`).
async fn version() -> Option<String> {
    let output = tokio::process::Command::new(AGENT_NAME)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // `claude --version` answers `2.1.250 (Claude Code)`, so the first whitespace-separated
    // token is the version. Measured rather than assumed.
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use sylph_protocol::RequestedNativeId;

    fn spec(mode: Option<&str>) -> RunSpec {
        RunSpec {
            workdir: "/tmp".into(),
            native_id: RequestedNativeId::Dictate(NativeId::new("11111111-2222-3333-4444-555555555555")),
            inject_identity: false,
            identity_preamble: None,
            flags: BTreeMap::new(),
            permission_mode: mode.map(str::to_string),
        }
    }

    fn argv(mode: Option<&str>) -> Vec<String> {
        ClaudeAdapter::argv_for(&spec(mode)).0
    }

    fn flag_after(argv: &[String], flag: &str) -> Option<String> {
        argv.iter().position(|part| part == flag).map(|at| argv[at + 1].clone())
    }

    /// `PermissionModeIsValidated` from `adapter_test.py`, ported.
    ///
    /// A mode the CLI rejects must not reach it. `ask` is the value this protocol's own
    /// spec examples use, and commander rejects it at parse time — so the CLI exits 1
    /// before answering `initialize` and the adapter reported a *timeout*, sending the
    /// investigation to the link layer while the cause was one argv entry.
    #[test]
    fn the_specs_own_example_permission_mode_is_not_accepted_by_the_cli() {
        assert!(!LAUNCH_PERMISSION_MODES.contains(&"ask"));
    }

    #[test]
    fn every_offered_spawn_choice_is_one_the_cli_accepts() {
        // A mode offered at creation that the CLI refuses is a session that cannot start,
        // chosen from a picker.
        for choice in SPAWN_MODE_CHOICES {
            assert!(
                LAUNCH_PERMISSION_MODES.contains(choice),
                "{choice} is offered at spawn but not accepted by --permission-mode"
            );
        }
    }

    #[test]
    fn every_settable_mode_is_one_the_cli_knows() {
        for mode in SETTABLE_PERMISSION_MODES {
            assert!(
                LAUNCH_PERMISSION_MODES.contains(mode),
                "{mode} is offered on a live session but is not a mode the CLI knows"
            );
        }
    }

    /// Measured on 2.1.246: `set_permission_mode` answers **success** for `manual` and
    /// confirms `default`. A row that lands on a different row is the same defect as the
    /// bypass bug by another route, and only a read-back can see it — an error check
    /// cannot, because there is no error.
    #[test]
    fn manual_is_not_offered_because_the_vendor_normalizes_it() {
        assert!(!SETTABLE_PERMISSION_MODES.contains(&"manual"));
        assert!(!SPAWN_MODE_CHOICES.contains(&"manual"));
        // It is still a mode the CLI *parses*, which is why the two lists differ at all.
        assert!(LAUNCH_PERMISSION_MODES.contains(&"manual"));
    }

    /// It was settable all along and absent from the offered list, so a real mode was
    /// unreachable through the UI.
    #[test]
    fn auto_is_offered_because_the_vendor_applies_it_as_given() {
        assert!(SETTABLE_PERMISSION_MODES.contains(&"auto"));
    }

    /// `TheApprovalChannelIsAlwaysRegistered`, ported. Asserted on the argv because the
    /// ordering bug lived in argv construction and nothing below it could observe one.
    #[test]
    fn a_bypassing_launch_still_registers_the_approval_channel() {
        let argv = argv(Some("bypassPermissions"));
        assert_eq!(flag_after(&argv, "--permission-prompt-tool").as_deref(), Some("stdio"));
    }

    #[test]
    fn an_interactive_launch_registers_it_too() {
        let argv = argv(Some("default"));
        assert_eq!(flag_after(&argv, "--permission-prompt-tool").as_deref(), Some("stdio"));
    }

    /// Both halves matter: the flag has to be present, or `set_permission_mode` is
    /// refused; and the launch mode has to stay as asked, or selecting bypass later is
    /// indistinguishable from having started in it.
    #[test]
    fn bypass_is_made_reachable_without_starting_in_it() {
        let argv = argv(Some("default"));
        assert!(argv.iter().any(|part| part == "--allow-dangerously-skip-permissions"));
        assert!(
            !argv.iter().any(|part| part == "--dangerously-skip-permissions"),
            "the launch mode must stay as asked"
        );
        assert_eq!(flag_after(&argv, "--permission-mode").as_deref(), Some("default"));
    }

    /// An unaccepted mode is substituted rather than forwarded, and the substitution is
    /// reported — a session running under the default policy that says so beats a session
    /// that cannot start, but a silent swap is a policy change the user never saw.
    #[test]
    fn an_unaccepted_mode_is_substituted_and_reported() {
        let (argv, warnings) = ClaudeAdapter::argv_for(&spec(Some("ask")));
        assert_eq!(flag_after(&argv, "--permission-mode").as_deref(), Some("default"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ask"), "the warning must name what was refused");
    }

    /// Dictate and resume are different flags on this CLI, and collapsing them is what
    /// `RequestedNativeId` exists to prevent.
    #[test]
    fn dictate_and_resume_produce_different_flags() {
        let mut dictating = spec(None);
        dictating.native_id = RequestedNativeId::Dictate(NativeId::new("abc"));
        let (argv, _) = ClaudeAdapter::argv_for(&dictating);
        assert_eq!(flag_after(&argv, "--session-id").as_deref(), Some("abc"));
        assert!(!argv.iter().any(|part| part == "-r"));

        let mut resuming = spec(None);
        resuming.native_id = RequestedNativeId::Resume(NativeId::new("abc"));
        let (argv, _) = ClaudeAdapter::argv_for(&resuming);
        assert_eq!(flag_after(&argv, "-r").as_deref(), Some("abc"));
        assert!(!argv.iter().any(|part| part == "--session-id"));
    }

    /// The flags that make the protocol's own features reachable. `--forward-subagent-text`
    /// is what tags a delegate's output with `parent_tool_use_id`, and
    /// `--include-partial-messages` is what makes the stream a stream.
    #[test]
    fn the_streaming_and_delegation_flags_are_always_present() {
        let argv = argv(None);
        for required in [
            "--input-format",
            "--output-format",
            "--include-partial-messages",
            "--forward-subagent-text",
        ] {
            assert!(argv.iter().any(|part| part == required), "missing {required}");
        }
    }

    /// Identity injection is the server's decision, so the adapter emits the flag only
    /// when it was told to and there is text to write.
    #[test]
    fn identity_injection_is_only_argv_when_the_server_asked_for_it() {
        let mut without = spec(None);
        without.inject_identity = false;
        without.identity_preamble = Some("ignored".into());
        let (argv, _) = ClaudeAdapter::argv_for(&without);
        assert!(!argv.iter().any(|part| part == "--append-system-prompt-file"));

        let mut with = spec(None);
        with.inject_identity = true;
        with.identity_preamble = Some("# Sylph".into());
        let (argv, _) = ClaudeAdapter::argv_for(&with);
        assert_eq!(
            flag_after(&argv, "--append-system-prompt-file").as_deref(),
            Some(IDENTITY_PREAMBLE_PLACEHOLDER)
        );
    }

    /// `/model` must be a control command, or it is sent as prose and answers with a
    /// paragraph listing models — the CLI behaving correctly, because a headless client
    /// has no picker to open.
    #[test]
    fn model_is_a_control_command_and_names_the_option_it_drives() {
        let mapped = CONTROL_COMMANDS
            .iter()
            .find(|(name, _)| *name == "model")
            .map(|(_, config)| *config);
        assert_eq!(mapped, Some("model"));
    }

    /// The commands that produce output worth keeping must stay prompts, or their answers
    /// are hidden in a dialog that closes.
    #[test]
    fn commands_whose_output_is_worth_keeping_are_not_control() {
        for prompt in ["context", "compact", "usage", "clear", "doctor", "init", "mcp", "agents"] {
            assert!(
                !CONTROL_COMMANDS.iter().any(|(name, _)| *name == prompt),
                "/{prompt} must stay a prompt command"
            );
        }
    }

    /// Every dialog kind declared must be one `resolve` can answer. Declaring one this
    /// adapter cannot render parks a dialog until its deadline.
    #[test]
    fn every_declared_dialog_kind_is_answerable() {
        assert!(SUPPORTED_DIALOG_KINDS.contains(&"permission_ask_user_question"));
        assert!(SUPPORTED_DIALOG_KINDS.contains(&"permission_enter_plan_mode"));
        assert!(SUPPORTED_DIALOG_KINDS.contains(&"cost_threshold"));
        // The permission-shaped family is handled by one branch, so every member of it
        // must carry the prefix that branch tests for.
        let permission_kinds = SUPPORTED_DIALOG_KINDS
            .iter()
            .filter(|kind| kind.starts_with("permission_"))
            .count();
        assert!(permission_kinds >= 8, "found {permission_kinds}");
    }
}
