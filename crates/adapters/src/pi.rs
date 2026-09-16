//! Pi process controls and raw source discovery.
//!
//! The process transport retains exact bytes before routing replies or operational
//! notices. Trace interpretation belongs to the pure mapper in `crates/trace/`.
//! `docs/unified-trace-pi.md` owns the vendor acquisition and mapping contract.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, Facet, FlagSpec, LinkFacets, NativeId, NativeSource,
    NativeSummary, RunSpec, Session, StoreFilter, UnavailableReason,
};

pub use rpc::{model_key, split_model_key, ConfigId, MODEL_KEY_SEPARATOR};
pub use session::PiSession;

/// The name this adapter answers to, and the `PATH` binary that decides availability.
///
/// One constant for both because for this vendor they are the same string. The *store*
/// path is not derivable from either and is stated separately in [`native`].
pub const AGENT_NAME: &str = "pi";

/// Minimum pi version covered by the adapter's measurements.
///
/// The capability matrix in `docs/agents.md` was measured against 0.84.3 and this is
/// therefore the minimum version the adapter can currently verify.
pub const VERSION_FLOOR: &str = "0.84.3";

/// The thinking levels the CLI accepts at launch, from `pi --help` on 0.84.3.
///
/// **Deliberately not the set a live session offers.** `get_available_thinking_levels`
/// answers for the *currently selected* model and really does answer less — the model
/// selected during measurement offered only `off` and `high`, not these seven. So this is
/// the launch flag's vocabulary and nothing else may be derived from it; the live surface
/// is re-read from the vendor after every model change
/// ([`rpc::ConfigSurface::after_model_change`]).
pub const LAUNCH_THINKING_LEVELS: &[&str] =
    &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The `pi` adapter.
#[derive(Debug, Clone, Default)]
pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The link facets for a session that was started outside Sylph.
    ///
    /// Pi exposes no second-party control channel: its RPC transport is stdio owned by the
    /// process that spawned it. A native-store watcher can therefore observe a terminal-born
    /// session, but it must not advertise `interrupt`, `steer`, queueing, or config writes.
    /// Keeping this answer beside the adapter's measured control surface gives the envagent a
    /// source-specific fact to map onto the protocol's two facets without weakening the live
    /// `PiSession` surface used for sessions Sylph itself launched.
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
    /// `model` lists no choices because the catalogue depends on which providers the user
    /// configured — 372 entries on the measured install — and is only knowable from a
    /// live session, where it becomes a [`sylph_protocol::ConfigOption`] instead. An empty
    /// list means "free-form, ask the session", not "no models".
    fn flags() -> sylph_protocol::FlagSchema {
        let mut flags = sylph_protocol::FlagSchema::new();
        flags.insert(
            "model".to_string(),
            FlagSpec {
                choices: vec![],
                description: "Model for this session, as `provider/id` or a pattern.".to_string(),
            },
        );
        flags.insert(
            "thinking".to_string(),
            FlagSpec {
                choices: LAUNCH_THINKING_LEVELS
                    .iter()
                    .map(|level| level.to_string())
                    .collect(),
                description: "Thinking level. Which of these apply depends on the model."
                    .to_string(),
            },
        );
        flags
    }

    /// The argv one [`Adapter::run`] would use.
    ///
    /// Split out from the spawn so it is assertable without a process, the same shape
    /// `claude`'s module uses and for the same reason: an argv bug is invisible to
    /// anything below argv construction.
    ///
    /// Returns the argv **and** any warning worth logging rather than logging inside.
    /// Substituting something the user asked for silently is exactly the help an adapter
    /// must not give, so the caller reports it.
    pub fn argv_for(spec: &RunSpec) -> (Vec<String>, Vec<String>) {
        let mut argv: Vec<String> = [AGENT_NAME, "--mode", "rpc"]
            .iter()
            .map(|part| part.to_string())
            .collect();
        let mut warnings = Vec::new();

        match &spec.native_id {
            // **`--session-id` is both spellings of the same flag on this vendor.** It
            // uses an exact project session id and *creates it if missing*, which is
            // precisely resume-or-start — so dictate and resume produce the same argv
            // here, and that is a property of this CLI rather than of the protocol.
            // `RequestedNativeId` still keeps them apart because the *server*'s checks
            // differ: dictating a new id must refuse if one already exists, resuming
            // requires that it does.
            sylph_protocol::RequestedNativeId::Dictate(id)
            | sylph_protocol::RequestedNativeId::Resume(id) => {
                argv.push("--session-id".into());
                argv.push(id.as_str().to_string());
            }
            // Unreachable for this vendor: `dictates_native_id` is true, so the **envagent**
            // names an id (`crates/envagent/src/runtime.rs`, `requested_native_id_for_new`)
            // — it is the process that holds this adapter and builds the `RunSpec`, and it
            // reads that field rather than a list of agent names. Handled rather than
            // panicking because a protocol value an adapter cannot honour is a refusal, not
            // a crash — and **this adapter must not mint one itself**. The v1 adapter did (
            // `uuid.uuid4()`), which is the synthesized-identity failure
            // `sylph_protocol::NativeId` forbids: the id would look like the vendor's and
            // be Sylph's.
            //
            // Worth knowing how long this went unimplemented: every adapter asserted the
            // caller "always names an id" while both `RunSpec` sites hardcoded `New`, so
            // this arm was the *only* one production ever reached. The refusal below is what
            // surfaced it, as a `502` on every `pi` session; `claude` warns and runs
            // instead, so the same gap there produced a degraded session and no complaint.
            sylph_protocol::RequestedNativeId::New => {
                // **Says what is true of the argv, and no more.** An earlier wording claimed
                // this made the session "unresumable", which overstates its own reason: pi
                // reports a `sessionId` from `get_state`, so an adapter *could* read one back
                // — `codex` is built that way. What is actually the case is narrower and is
                // the whole justification for `session::PiSession::open` refusing this
                // variant: this vendor takes `--session-id`, `dictates_native_id` is true, so
                // the envagent names one and `New` is unreachable here rather than
                // impossible in principle. A doc that overstates its reason is how the next
                // reader concludes something stronger than what was measured.
                warnings.push(
                    "pi takes --session-id and the envagent names one, so opening \
                     without an id is unreachable for this agent; no argv id was emitted"
                        .to_string(),
                );
            }
        }

        if let Some(model) = spec.flags.get("model") {
            argv.push("--model".into());
            argv.push(model.clone());
        }
        if let Some(thinking) = spec.flags.get("thinking") {
            // Validated here rather than forwarded, because the CLI parses this flag
            // strictly and a rejected argument exits before the RPC channel opens — which
            // presents as "pi did not answer in time", a timeout message for an argument
            // error. Same shape as `claude`'s permission-mode substitution.
            if LAUNCH_THINKING_LEVELS.contains(&thinking.as_str()) {
                argv.push("--thinking".into());
                argv.push(thinking.clone());
            } else {
                warnings.push(format!(
                    "pi does not accept --thinking {thinking:?}; leaving the model's own \
                     default. Accepted: {}",
                    LAUNCH_THINKING_LEVELS.join(", ")
                ));
            }
        }

        // Identity injection, decided by the **server**. `--append-system-prompt` takes
        // text *or* a file path and is additive either way, so the agent's own rules and
        // the project's `AGENTS.md` survive.
        if spec.inject_identity && spec.identity_preamble.is_some() {
            argv.push("--append-system-prompt".into());
            argv.push(IDENTITY_PREAMBLE_PLACEHOLDER.into());
        }

        (argv, warnings)
    }
}

/// Where the identity preamble's temporary path lands in an argv.
///
/// The text arrives in the [`RunSpec`] and has to become a file for this flag, so
/// [`PiAdapter::argv_for`] — which is pure, and therefore cannot write one — emits this
/// marker and `run` substitutes the path it wrote. A marker rather than an empty string so
/// a caller that forgets to substitute fails visibly instead of passing an empty prompt
/// the vendor would append verbatim.
pub const IDENTITY_PREAMBLE_PLACEHOLDER: &str = "<sylph-identity-preamble>";

#[async_trait]
impl Adapter for PiAdapter {
    fn version_floor(&self) -> Option<&'static str> {
        Some(VERSION_FLOOR)
    }

    async fn probe(&self) -> Result<AgentInfo> {
        if !Self::is_installed() {
            // Names the CLI, because that is the whole fix. The mechanism this replaced
            // could only say an adapter was not found.
            return Err(anyhow!(
                "the `pi` CLI is not on this machine's PATH; install pi to use this agent"
            ));
        }
        Ok(AgentInfo {
            name: AGENT_NAME.to_string(),
            version: version().await,
            flags: Self::flags(),
            // Native priority insertion has no atomic turn precondition. Keep it as
            // QueueSteering, separately from turn-scoped Steer and follow-up Queue.
            supports: vec![
                Capability::Interrupt,
                Capability::QueueSteering,
                Capability::Queue,
                Capability::SetConfig,
            ],
            // Measured 2026-08-27: `--session-id` uses an exact id and creates the
            // session if missing, so identity is known before the first event.
            dictates_native_id: true,
            // Its store flushes at every `message_end`, which is per assistant message
            // *and* per tool result. See `native::NATIVE_FIDELITY` for why that is
            // `Messages` and not something finer.
            native_fidelity: native::NATIVE_FIDELITY,
        })
    }

    async fn run(
        &self,
        spec: RunSpec,
        capture: std::sync::Arc<dyn sylph_protocol::RawCaptureSink>,
    ) -> Result<Box<dyn Session>> {
        let session = session::PiSession::open(spec, capture).await?;
        Ok(Box::new(session))
    }

    async fn native_sources(&self, id: &NativeId) -> Result<Vec<NativeSource>> {
        let id = id.clone();
        tokio::task::spawn_blocking(move || native::sources(&id)).await?
    }

    async fn enumerate(&self, filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || native::enumerate(&filter)).await?
    }

    async fn lookup(&self, id: &NativeId) -> Result<Option<NativeSummary>> {
        let id = id.clone();
        tokio::task::spawn_blocking(move || native::lookup(&id)).await?
    }
}

/// The CLI's own version, or `None` when it did not report one.
///
/// Never synthesized: a fabricated version is a lie a journal makes permanent, and it
/// would make an unsupported install look supported ([`AgentInfo::version`]).
async fn version() -> Option<String> {
    let output = tokio::process::Command::new(AGENT_NAME)
        .arg("--version")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // `pi --version` answers a bare `0.84.3` with no trailing prose, unlike `claude`.
    // Measured rather than assumed, and the whitespace split is harmless either way.
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

    fn spec() -> RunSpec {
        RunSpec {
            workdir: "/tmp".into(),
            native_id: RequestedNativeId::Dictate(NativeId::new(
                "019f364f-c1e5-7e4f-a3e5-7c7ed13b17aa",
            )),
            inject_identity: false,
            identity_preamble: None,
            flags: BTreeMap::new(),
            permission_mode: None,
        }
    }

    fn flag_after(argv: &[String], flag: &str) -> Option<String> {
        argv.iter()
            .position(|part| part == flag)
            .and_then(|at| argv.get(at + 1).cloned())
    }

    /// RPC mode is the whole channel. Without it `pi` starts a TUI and nothing on stdout
    /// is protocol.
    #[test]
    fn the_argv_always_asks_for_rpc_mode() {
        let (argv, warnings) = PiAdapter::argv_for(&spec());
        assert_eq!(flag_after(&argv, "--mode").as_deref(), Some("rpc"));
        assert!(warnings.is_empty());
    }

    /// One flag serves both, and that is this CLI's own semantics rather than the two
    /// cases being conflated: `--session-id` creates the session when it is missing.
    #[test]
    fn dictate_and_resume_both_become_session_id_on_this_vendor() {
        for requested in [
            RequestedNativeId::Dictate(NativeId::new("abc")),
            RequestedNativeId::Resume(NativeId::new("abc")),
        ] {
            let mut spec = spec();
            spec.native_id = requested;
            let (argv, _) = PiAdapter::argv_for(&spec);
            assert_eq!(flag_after(&argv, "--session-id").as_deref(), Some("abc"));
        }
    }

    /// The adapter must **not** mint an id of its own, which the Python original did.
    /// A synthesized id looks like the vendor's and is Sylph's, which is the two-id-spaces
    /// failure arriving inside one id space.
    #[test]
    fn opening_without_an_id_is_refused_rather_than_answered_with_an_invented_one() {
        let mut spec = spec();
        spec.native_id = RequestedNativeId::New;
        let (argv, warnings) = PiAdapter::argv_for(&spec);
        assert!(
            !argv.iter().any(|part| part == "--session-id"),
            "no id may be invented: {argv:?}"
        );
        assert_eq!(warnings.len(), 1);
        // Asserted on the claim the warning is *entitled* to make. It used to say
        // "unresumable", which overstates its reason — pi reports a `sessionId` from
        // `get_state`, so an adapter could read one back and `codex` does. What is true is
        // that the envagent names an id for this agent, so the case is unreachable
        // here rather than impossible in principle.
        assert!(
            warnings[0].contains("unreachable"),
            "the warning must claim only what it can: {:?}",
            warnings[0]
        );
        assert!(
            !warnings[0].contains("unresumable"),
            "reachability is the claim, not resumability: {:?}",
            warnings[0]
        );
    }

    /// A thinking level the CLI rejects exits before the RPC channel opens, so the
    /// failure reads as a timeout in the link layer. Substituted and reported instead.
    #[test]
    fn an_unaccepted_thinking_level_is_dropped_and_reported() {
        let mut spec = spec();
        spec.flags.insert("thinking".into(), "banana".into());
        let (argv, warnings) = PiAdapter::argv_for(&spec);
        assert!(!argv.iter().any(|part| part == "--thinking"));
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("banana"),
            "the warning must name what was refused: {warnings:?}"
        );

        let mut good = spec.clone();
        good.flags.insert("thinking".into(), "high".into());
        let (argv, warnings) = PiAdapter::argv_for(&good);
        assert_eq!(flag_after(&argv, "--thinking").as_deref(), Some("high"));
        assert!(warnings.is_empty());
    }

    /// The launch flag's seven levels are the CLI's; a live session offers what the
    /// *selected model* supports, which is measurably fewer. Nothing may derive one from
    /// the other.
    #[test]
    fn the_launch_levels_are_not_the_live_surface() {
        assert_eq!(LAUNCH_THINKING_LEVELS.len(), 7);
        assert!(LAUNCH_THINKING_LEVELS.contains(&"xhigh"));
        // The measured live answer for one real model, which this list must never be
        // used to widen.
        let live = ["off", "high"];
        assert!(live
            .iter()
            .all(|level| LAUNCH_THINKING_LEVELS.contains(level)));
        assert!(live.len() < LAUNCH_THINKING_LEVELS.len());
    }

    /// Identity injection is the server's decision, so the flag appears only when it was
    /// asked for and there is text to write.
    #[test]
    fn identity_injection_is_only_argv_when_the_server_asked_for_it() {
        let mut without = spec();
        without.inject_identity = false;
        without.identity_preamble = Some("ignored".into());
        let (argv, _) = PiAdapter::argv_for(&without);
        assert!(!argv.iter().any(|part| part == "--append-system-prompt"));

        let mut with = spec();
        with.inject_identity = true;
        with.identity_preamble = Some("# Sylph".into());
        let (argv, _) = PiAdapter::argv_for(&with);
        assert_eq!(
            flag_after(&argv, "--append-system-prompt").as_deref(),
            Some(IDENTITY_PREAMBLE_PLACEHOLDER)
        );
    }
}

/// The live RPC surface: what a config value means, and what the vendor takes.
///
/// `~/.local/lib/node_modules/@earendil-works/pi-coding-agent/docs/rpc.md` is the vendor's
/// own documentation and every constant here was checked against it *and* against a real
/// install, because on this vendor the two have disagreed.
///
/// # Why this is process-free
///
/// Nothing here spawns anything or holds a socket. The three round trips a config surface
/// needs — `get_state`, `get_available_models`, `get_available_thinking_levels` — are
/// *described* by [`rpc::ConfigSurface`] and performed by the live session in step 4, so the
/// encoding, the validation, and the two refusal codes are all assertable today. The
/// vendor-key defect this vendor is most famous for lives in exactly these functions.
pub mod rpc {
    use serde_json::{json, Value};
    use sylph_protocol::{
        AdapterError, ConfigChoice, ConfigKind, ConfigOption, RefusalCode, SessionCapabilities,
    };

    /// The delimiter between a model's provider and its id in a [`ConfigChoice::value`].
    ///
    /// **A space, and it is load-bearing rather than stylistic.** *Measured on pi 0.84.3:*
    /// 350 of 372 model ids already contain a `/` (`anthropic/claude-opus-5`,
    /// `stealth/ox-alpha`), so splitting on the first or last slash mis-parses the majority
    /// of the catalogue. No provider and no id contains a space.
    ///
    /// A `ConfigChoice.value` is **opaque to the client by contract** — a token to echo
    /// back, never to parse — which is what lets this encoding change without a server or
    /// UI change. Only this module knows it.
    pub const MODEL_KEY_SEPARATOR: char = ' ';

    /// The config options this adapter can set, and the only ids it answers to.
    ///
    /// An enum rather than string matching so a new option cannot be half-added: a variant
    /// that exists must be handled in [`Self::parse`] and in the surface builder, and the
    /// compiler says so. `pi`'s queue modes (`set_steering_mode`, `set_follow_up_mode`) are
    /// deliberately **absent**: they are real vendor commands, and Sylph has no word for
    /// the discipline they select, so offering them would be a picker whose values mean
    /// nothing to any consumer. `AGENTS.md` § Compatibility — a declared-only entry ships a
    /// branch that never fires.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ConfigId {
        /// `set_model`, which takes `provider` **and** `modelId`.
        Model,
        /// `set_thinking_level`, which takes `level`.
        Thinking,
    }

    impl ConfigId {
        /// The wire id, which is also what a [`ConfigOption::id`] carries.
        pub fn as_str(&self) -> &'static str {
            match self {
                Self::Model => "model",
                Self::Thinking => "thinking",
            }
        }

        /// Read an id a client sent, or `None` for one this agent has no such option for.
        ///
        /// `None` becomes [`RefusalCode::UnsupportedField`] and nothing else. That is the
        /// distinction the whole error path turns on: **an error that names the wrong layer
        /// sends the investigation to the wrong layer**, and answering a malformed *value*
        /// with "'model' is not settable on a pi session" says the option does not exist
        /// when the option was fine.
        pub fn parse(id: &str) -> Option<Self> {
            match id {
                "model" => Some(Self::Model),
                "thinking" => Some(Self::Thinking),
                _ => None,
            }
        }
    }

    /// One catalogue entry, as `get_available_models` and `get_state` both report it.
    ///
    /// Field names are the vendor's camelCase wire spellings, checked against
    /// `pi-coding-agent/docs/rpc.md` § Model: `{id, name, api, provider, baseUrl, reasoning, input,
    /// contextWindow, maxTokens, cost}`. Only what a surface needs is read; the rest is
    /// left in the payload rather than mirrored into a struct that would then have to be
    /// kept equal to it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Model {
        pub id: String,
        /// The provider, which is **half the model's identity** and not decoration.
        ///
        /// `Option` because the catalogue may report an entry without one, and a value must
        /// then stand alone rather than gaining an empty prefix that [`split_model_key`]
        /// would have to special-case.
        pub provider: Option<String>,
        /// The vendor's display name. Absent falls back to the id at render time rather
        /// than here, so nothing invents a name (`sylph_protocol::Event::name`).
        pub name: Option<String>,
        /// Reported per model, which is why `contextWindow` is a real facet for this agent
        /// where `claude` correctly publishes none.
        pub context_window: Option<i64>,
    }

    impl Model {
        /// Read one entry off a vendor payload, or `None` when it is unusable.
        ///
        /// An entry with no `id` is dropped rather than becoming a row that cannot be
        /// selected. A dropped row is a visible absence; an unselectable one is a control
        /// that appears to do nothing.
        pub fn from_payload(payload: &Value) -> Option<Self> {
            let id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())?;
            Some(Self {
                id: id.to_string(),
                provider: text(payload, "provider"),
                name: text(payload, "name"),
                context_window: payload.get("contextWindow").and_then(Value::as_i64),
            })
        }

        /// This entry's stable, unambiguous config value.
        pub fn key(&self) -> String {
            model_key(self.provider.as_deref(), &self.id)
        }
    }

    /// Read a string field, treating `""` as absent.
    ///
    /// One helper so an empty string and an omitted key cannot be handled differently in
    /// two branches — every decision downstream wants the same answer for both.
    fn text(payload: &Value, key: &str) -> Option<String> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// The stable value for one catalogue entry: `provider` + a space + `id`.
    ///
    /// **A model's identity is `(provider, id)`, never the id alone.** *Measured on pi
    /// 0.84.3:* of 372 offered models, `deepseek-v4-pro` and `glm-5.2` are each offered by
    /// two providers, and three display names are duplicated (two "Claude Opus 5", one
    /// direct and one via openrouter). A bare id was ambiguous three separate ways, all
    /// real: `checked` is computed as `value == current_value` so **both** rows showed a
    /// checkmark; one of each pair was *unreachable*, because a by-id lookup can only
    /// answer with one provider; and pi accepts either provider for those ids and reports
    /// back which it used, so the surface could disagree with the request that built it.
    ///
    /// A provider-less entry yields the bare id, which is correct rather than a degraded
    /// case: there is nothing to disambiguate, and `set_model` will refuse it loudly
    /// ([`split_model_key`]) rather than sending a request the vendor answers with a
    /// message about a field the user never saw.
    pub fn model_key(provider: Option<&str>, id: &str) -> String {
        match provider.filter(|provider| !provider.is_empty()) {
            Some(provider) => format!("{provider}{MODEL_KEY_SEPARATOR}{id}"),
            None => id.to_string(),
        }
    }

    /// `(provider, model_id)` out of a value this module issued.
    ///
    /// Split **once from the left**: a provider cannot contain the separator and an id can
    /// contain anything but it. A value carrying no separator is a bare id from a catalogue
    /// that reported no provider — `set_model` needs both fields, so that is reported as
    /// unusable rather than sent.
    ///
    /// Returns [`RefusalCode::InvalidValue`], never `UnsupportedField`: the option exists
    /// and this value does not fit it.
    pub fn split_model_key(value: &str) -> Result<(String, String), AdapterError> {
        let unusable = || {
            AdapterError::invalid_value(format!(
                "{value:?} does not name a provider and a model. Values for this option are \
                 issued by the pi adapter and must be echoed back unchanged."
            ))
        };
        let (provider, id) = value.split_once(MODEL_KEY_SEPARATOR).ok_or_else(unusable)?;
        if provider.is_empty() || id.is_empty() {
            return Err(unusable());
        }
        Ok((provider.to_string(), id.to_string()))
    }

    /// One RPC command to send, as `{type, …fields}`.
    ///
    /// Described rather than sent, so the *shape* of every command is assertable without a
    /// process. This is where the most expensive vendor-key defect in this repo's history
    /// lived, and a test over these payloads is what catches its return.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Command(pub Value);

    impl Command {
        fn new(command_type: &str, fields: Value) -> Self {
            let mut payload = json!({"type": command_type});
            if let (Some(object), Some(extra)) = (payload.as_object_mut(), fields.as_object()) {
                for (key, value) in extra {
                    object.insert(key.clone(), value.clone());
                }
            }
            Self(payload)
        }

        pub fn as_value(&self) -> &Value {
            &self.0
        }
    }

    /// The command that applies one config change, or the refusal that stops it.
    ///
    /// # `set_model` takes `provider` + `modelId`, and both are mandatory
    ///
    /// *Measured against pi 0.84.3, and this is the third instance of the vendor-key
    /// family and the most expensive:* sending `model=` is answered
    /// `Model not found: undefined/undefined` — pi interpolating the two fields it looked
    /// for and found absent — and `provider=` plus `model=` narrows it to
    /// `anthropic/undefined`, which is what identifies *which* field is misspelled. The
    /// refusal came back as a failed RPC that nothing surfaced, so a correct picker looked
    /// broken and the investigation went to the UI twice before reaching the adapter.
    ///
    /// The provider is **looked up from the catalogue**, never parsed out of the id — which
    /// is why it travels inside the value this module issued rather than being recovered
    /// from a slash the majority of ids already contain.
    pub fn command_for(id: &str, value: &Value) -> Result<Command, AdapterError> {
        let Some(config) = ConfigId::parse(id) else {
            // No such option: the one refusal that justifies disabling a control.
            return Err(AdapterError::unsupported_field(format!(
                "'{id}' is not settable on a pi session"
            )));
        };
        let Some(text) = value.as_str() else {
            return Err(AdapterError::invalid_value(format!(
                "'{id}' takes a string value, and this was {}",
                kind_of(value)
            )));
        };
        match config {
            ConfigId::Model => {
                let (provider, model_id) = split_model_key(text)?;
                Ok(Command::new(
                    "set_model",
                    json!({"provider": provider, "modelId": model_id}),
                ))
            }
            // Deliberately **not** validated against `LAUNCH_THINKING_LEVELS`: the levels a
            // live session accepts depend on the selected model, and that launch list is
            // strictly wider. Validating against it would accept a level this model
            // refuses; validating against the model's own answer is what
            // `ConfigSurface::accepts` does, from what the vendor enumerated.
            ConfigId::Thinking => Ok(Command::new("set_thinking_level", json!({"level": text}))),
        }
    }

    /// What a non-string value was, for an error a person reads.
    fn kind_of(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "a list",
            Value::Object(_) => "an object",
        }
    }

    /// The three round trips that build a config surface, named so the order is a fact
    /// rather than a convention.
    ///
    /// *Measured:* 372 models on a real install, so the catalogue is worth caching for a
    /// session's life — but the **current values are not**, and neither are the thinking
    /// levels. See [`ConfigSurface::after_model_change`].
    pub const SURFACE_ROUND_TRIPS: &[&str] = &[
        // Current model and thinking level, plus the selected model's own entry — which is
        // where `contextWindow` comes from. Read once and *held*: a second `get_state` is
        // both an extra round trip and a second answer that can disagree with the one the
        // options were built from, which would put a context window under a model the
        // options never mentioned.
        "get_state",
        "get_available_models",
        // Answers for the *currently selected* model. This is why `set_config` returns a
        // refreshed array rather than an acknowledgement.
        "get_available_thinking_levels",
    ];

    /// A live session's config surface, built from what the vendor just said.
    ///
    /// # Current values come from the vendor, never from what was last set
    ///
    /// *This is a shipped defect being structurally prevented rather than a preference.*
    /// The Python original answered `currentValue: self.current_model`, a field only
    /// `surface()` ever populated — so any surface built by another path (a re-attach, a
    /// `set_config` reply) reported `null` while the session plainly had a model, and the
    /// composer chip fell back to "agent default" with the checkmark on no row.
    ///
    /// Two separate claims sit behind that. The vendor is the only thing that knows whether
    /// a `set` was *honoured*; and a session can be steered by pi's own TUI besides, so
    /// even a set this adapter watched succeed can be stale. There is therefore no field
    /// here that any code path other than [`Self::from_vendor`] can write.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ConfigSurface {
        /// The whole catalogue, in the order the vendor listed it.
        pub models: Vec<Model>,
        /// The selected model's own catalogue entry, as `get_state` reported it.
        pub current: Option<Model>,
        /// What the *currently selected* model supports, not the launch flag's seven.
        pub thinking_levels: Vec<String>,
        pub current_thinking: Option<String>,
    }

    impl ConfigSurface {
        /// Build from the three answers, in the order [`SURFACE_ROUND_TRIPS`] names.
        ///
        /// Takes the payloads rather than performing the calls, which is what makes the
        /// whole projection testable with no process — and the reason the surface bugs this
        /// vendor produced were invisible to an end-to-end check.
        pub fn from_vendor(state: &Value, models: &Value, levels: &Value) -> Self {
            let models = models
                .get("models")
                .and_then(Value::as_array)
                .map(|entries| entries.iter().filter_map(Model::from_payload).collect())
                .unwrap_or_default();
            Self {
                models,
                // `get_state`'s `model` is a full Model object **or null**, per
                // `pi-coding-agent/docs/rpc.md`. Null is a real state — a session with no model selected —
                // and must stay absent rather than becoming an empty row.
                current: state.get("model").and_then(Model::from_payload),
                thinking_levels: levels
                    .get("levels")
                    .and_then(Value::as_array)
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|level| !level.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                current_thinking: text(state, "thinkingLevel"),
            }
        }

        /// The typed options, which is what [`sylph_protocol::Session::set_config`] answers
        /// with.
        ///
        /// An option whose catalogue is empty is **omitted entirely** rather than published
        /// with no choices: a `Select` that enumerated nothing means "free-form, send any
        /// string" (`ConfigOption::accepts`), which is the opposite of what an empty
        /// catalogue means here — a vendor that listed no models cannot accept an arbitrary
        /// one.
        pub fn options(&self) -> Vec<ConfigOption> {
            let mut options = Vec::new();

            if !self.models.is_empty() {
                options.push(ConfigOption {
                    id: ConfigId::Model.as_str().to_string(),
                    name: "Model".to_string(),
                    description: None,
                    category: Some("model".to_string()),
                    kind: ConfigKind::Select,
                    current_value: self
                        .current
                        .as_ref()
                        .map(|model| Value::String(model.key())),
                    options: self
                        .models
                        .iter()
                        .map(|model| ConfigChoice {
                            value: model.key(),
                            // The id, not a synthesized label, when the vendor named none.
                            name: model.name.clone().unwrap_or_else(|| model.id.clone()),
                            // **The provider is what disambiguates two rows with the same
                            // display name**, which measurably happens: two "Claude Opus 5"
                            // (anthropic direct and via openrouter) and two each of
                            // "DeepSeek V4 Pro" and "GLM-5.2". Without this the rows are
                            // indistinguishable on screen even though their values differ.
                            description: model.provider.clone(),
                        })
                        .collect(),
                });
            }

            if !self.thinking_levels.is_empty() {
                options.push(ConfigOption {
                    id: ConfigId::Thinking.as_str().to_string(),
                    name: "Thinking".to_string(),
                    description: None,
                    category: Some("effort".to_string()),
                    kind: ConfigKind::Select,
                    current_value: self.current_thinking.clone().map(Value::String),
                    options: self
                        .thinking_levels
                        .iter()
                        .map(|level| ConfigChoice {
                            value: level.clone(),
                            name: capitalize(level),
                            description: None,
                        })
                        .collect(),
                });
            }

            options
        }

        /// The whole surface, for an open or a full refresh.
        ///
        /// `subagents` is empty and `commands` comes from `get_commands`, which is a fourth
        /// round trip a caller makes separately — this takes what it read rather than
        /// pretending to have read it.
        pub fn capabilities(
            &self,
            commands: Vec<sylph_protocol::CommandSpec>,
        ) -> SessionCapabilities {
            SessionCapabilities {
                commands,
                // Empty, and this is a **report**: pi exposes no delegation on the RPC wire
                // in any form, so there is nothing to declare. `AGENTS.md` — an empty
                // surface means "Sylph cannot reach it" in general, and here it means
                // "the vendor has none", which is why the *link* facet is what says which.
                subagents: Vec::new(),
                config: self.options(),
                // Absent, not invented. `pi` has no permission mode at all: a tool runs
                // when the model calls it, so publishing one would promise a policy that
                // does not exist.
                permission_mode: None,
                // Real for this agent, unlike `claude`, because pi reports a per-model
                // window. Inventing a number would put a fabricated denominator under a
                // usage meter, which is worse than showing none.
                context_window: self.current.as_ref().and_then(|model| model.context_window),
            }
        }

        /// Whether this surface would accept `value` for `id`.
        ///
        /// Validated against **what the vendor enumerated**, which is the only sound
        /// authority: the launch flag's seven thinking levels are strictly wider than what
        /// a given model accepts, so validating against that list would pass a value the
        /// session then refuses — a refused write indistinguishable from an applied one.
        pub fn accepts(&self, id: &str, value: &Value) -> bool {
            self.options()
                .iter()
                .find(|option| option.id == id)
                .is_some_and(|option| option.accepts(value))
        }

        /// Why the thinking levels must be re-read after a model change.
        ///
        /// *Measured:* `get_available_thinking_levels` answers for the currently selected
        /// model, and the model selected during measurement offered only `off` and `high`
        /// against the launch flag's seven. So a cached list goes stale the moment the model
        /// changes, which is why `set_config` answers with the refreshed **array** and the
        /// reply mentions only the `config` facet.
        pub const fn after_model_change() -> &'static str {
            "re-read get_available_thinking_levels: the levels depend on the selected model"
        }
    }

    /// `off` → `Off`. ASCII-only by construction: these are the vendor's seven level names.
    fn capitalize(text: &str) -> String {
        let mut chars = text.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }

    /// The refusal a bad set earns, as a code rather than a message.
    ///
    /// Exposed so a caller can assert on the code without matching prose. The text is this
    /// adapter's and may be reworded; the code is declared protocol
    /// (`sylph_protocol::AdapterError`).
    pub fn refusal_code(id: &str, value: &Value) -> Option<RefusalCode> {
        command_for(id, value).err().map(|error| error.code)
    }

    /// One `get_commands` entry, projected.
    ///
    /// **Every command pi reports is a prompt, and that is the vendor's own doing rather
    /// than a classification this adapter makes.** `pi-coding-agent/docs/rpc.md` § get_commands states that
    /// built-in TUI commands are excluded because they "would not execute if sent via
    /// prompt" — so there is nothing left to separate. Model and thinking are config
    /// options here, not commands, which is why `CONTROL_COMMANDS` has no analogue in this
    /// module while `claude`'s needs one.
    pub fn command_specs(payload: &Value) -> Vec<sylph_protocol::CommandSpec> {
        payload
            .get("commands")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let name = text(entry, "name")?;
                        Some(sylph_protocol::CommandSpec {
                            name,
                            description: text(entry, "description").unwrap_or_default(),
                            arg_hint: None,
                            // `extension`, `prompt`, or `skill`, carried through as pi's own
                            // word rather than remapped: it is advisory grouping, and
                            // inventing a synonym would only lose information.
                            source: text(entry, "source").or_else(|| Some("prompt".to_string())),
                            kind: sylph_protocol::CommandKind::Prompt,
                            config_id: None,
                            aliases: Vec::new(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The one inversion `pi` has: an extension asking a human something.
///
/// **Not a permission channel.** `pi` ships no tool-approval callback, so nothing here ever
/// becomes a [`sylph_protocol::RequestKind::Approval`] — these arrive as `choice`, `dialog`,
/// or `elicitation`, which matches what the adapter declared it can raise.
/// `pi-coding-agent/docs/rpc.md` § Extension UI Protocol governs.
pub mod ui {
    use serde_json::{json, Value};
    use sylph_protocol::{
        Field, FieldKind, Form, Request, RequestKind, RequestOption, Resolution, SelectMode,
    };

    /// The vendor methods that expect **no** reply.
    ///
    /// From `pi-coding-agent/docs/rpc.md`: dialog methods block until an `extension_ui_response` with a
    /// matching id arrives; these do not. Answering one sends pi a response it is not
    /// waiting for, and ignoring the display is explicitly allowed.
    pub const FIRE_AND_FORGET: &[&str] = &[
        "notify",
        "setStatus",
        "setWidget",
        "setTitle",
        "set_editor_text",
    ];

    /// What one `extension_ui_request` becomes.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Inversion {
        /// A blocking dialog, to publish and wait on.
        Ask(Request),
        /// A `notify`, worth surfacing as a `notice` event and nothing more.
        Notice(String),
        /// Fire-and-forget with nothing to show. Not an error.
        Ignore,
        /// A method this build does not know.
        ///
        /// **Cancelled, never left parked.** An unknown dialog method still blocks pi, so
        /// silence stalls the turn until pi's own timeout lapses — which presents as a
        /// session that stopped for no reason. The payload to send back is
        /// [`cancel_response`].
        Unsupported { method: String, vendor_id: String },
    }

    /// Project one `extension_ui_request` onto a Sylph request.
    ///
    /// `now_ms` is a parameter because a deadline computed against a global clock is not assertable,
    /// and pi reports a `timeout` as a *duration* that only becomes an instant here.
    pub fn inversion(payload: &Value, now_ms: i64) -> Inversion {
        let method = payload
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let vendor_id = payload
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if FIRE_AND_FORGET.contains(&method) {
            if method == "notify" {
                let text = string_field(payload, "message")
                    .or_else(|| string_field(payload, "title"))
                    .unwrap_or_default();
                return Inversion::Notice(text);
            }
            return Inversion::Ignore;
        }

        // **pi auto-resolves a dialog when its own timeout lapses**, so the deadline is
        // forwarded rather than dropped: a client that cannot show it is racing a clock it
        // cannot see, and the user experiences a dialog that answers itself. Advisory on
        // Sylph's side — the server never acts on it (`sylph_protocol::Request`).
        let expires_at_ms = payload
            .get("timeout")
            .and_then(Value::as_f64)
            .map(|timeout| now_ms + timeout as i64);

        match method {
            "select" => {
                let offered = offered_options(payload);
                let title = string_field(payload, "title")
                    .unwrap_or_else(|| "pi has a question".to_string());
                Inversion::Ask(Request {
                    id: vendor_id,
                    kind: RequestKind::Choice,
                    tool_use_id: None,
                    tool: None,
                    title: title.clone(),
                    detail: Some(json!({"options": offered})),
                    // **Declared as a form rather than as options-that-are-answers**, so the
                    // pick and the act of sending it are separable — which is what lets a
                    // user change their mind before committing. `RequestOption` is a verb;
                    // overloading it as data is what forced picking to submit immediately.
                    form: Some(Form {
                        fields: vec![Field {
                            id: CHOICE_FIELD.to_string(),
                            kind: FieldKind::Select {
                                mode: SelectMode::One,
                                choices: offered
                                    .iter()
                                    .map(|label| sylph_protocol::Choice {
                                        value: label.clone(),
                                        label: None,
                                        description: None,
                                    })
                                    .collect(),
                                // pi matches on the literal string it sent, so an answer
                                // outside the list is nothing it can act on — see
                                // `value_response`, which turns one into a dismissal.
                                allow_other: false,
                                other_label: None,
                                notes: None,
                            },
                            label: title,
                            description: None,
                            required: true,
                        }],
                    }),
                    options: vec![submit("Send"), cancel()],
                    default: Some(CANCEL.to_string()),
                    risk: None,
                    allow_persistent: true,
                    expires_at_ms,
                })
            }
            "confirm" => Inversion::Ask(Request {
                id: vendor_id,
                kind: RequestKind::Dialog,
                tool_use_id: None,
                tool: None,
                title: string_field(payload, "title").unwrap_or_else(|| "pi asks".to_string()),
                detail: Some(json!({"message": payload.get("message")})),
                // No form: a confirm's whole answer *is* which verb was chosen, so a field
                // would be an input with nothing to collect.
                form: None,
                options: vec![
                    RequestOption {
                        id: ALLOW.to_string(),
                        label: "Yes".to_string(),
                        persistent: false,
                        description: None,
                    },
                    RequestOption {
                        id: DENY.to_string(),
                        label: "No".to_string(),
                        persistent: false,
                        description: None,
                    },
                ],
                default: Some(DENY.to_string()),
                risk: None,
                // pi has no persistent-grant concept at all, so offering "always" would
                // promise a lasting decision nothing records.
                allow_persistent: false,
                expires_at_ms,
            }),
            "input" | "editor" => {
                let multiline = method == "editor";
                let title = string_field(payload, "title")
                    .unwrap_or_else(|| "pi needs some text".to_string());
                Inversion::Ask(Request {
                    id: vendor_id,
                    kind: RequestKind::Elicitation,
                    tool_use_id: None,
                    tool: None,
                    title: title.clone(),
                    detail: Some(json!({
                        "placeholder": payload.get("placeholder"),
                        "prefill": payload.get("prefill"),
                        "multiline": multiline,
                    })),
                    // **The whole purpose of this kind is free text**, and before a form was
                    // declared there was nowhere to type: a client guessing from `detail`'s
                    // keys found none it recognized and rendered the placeholder as
                    // read-only JSON. `prefill` is carried because an `editor` dialog is
                    // usually a draft being revised, and discarding it loses the draft.
                    form: Some(Form {
                        fields: vec![Field {
                            id: TEXT_FIELD.to_string(),
                            kind: FieldKind::Text {
                                multiline,
                                prefill: string_field(payload, "prefill"),
                                placeholder: string_field(payload, "placeholder"),
                            },
                            label: title,
                            description: None,
                            required: false,
                        }],
                    }),
                    options: vec![submit("Submit"), cancel()],
                    default: Some(CANCEL.to_string()),
                    risk: None,
                    allow_persistent: false,
                    expires_at_ms,
                })
            }
            other => Inversion::Unsupported {
                method: other.to_string(),
                vendor_id,
            },
        }
    }

    /// The option ids this module issues, and the only ones [`response_for`] answers to.
    pub const SUBMIT: &str = "submit";
    pub const CANCEL: &str = "cancel";
    pub const ALLOW: &str = "allow";
    pub const DENY: &str = "deny";

    /// The form field ids, which a resolution's answer is read out of.
    pub const CHOICE_FIELD: &str = "choice";
    pub const TEXT_FIELD: &str = "text";

    fn submit(label: &str) -> RequestOption {
        RequestOption {
            id: SUBMIT.to_string(),
            label: label.to_string(),
            persistent: false,
            description: None,
        }
    }

    fn cancel() -> RequestOption {
        RequestOption {
            id: CANCEL.to_string(),
            label: "Cancel".to_string(),
            persistent: false,
            description: None,
        }
    }

    fn string_field(payload: &Value, key: &str) -> Option<String> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// The literal option strings pi offered.
    ///
    /// Kept so a picked value can be checked against **what pi sent**, since pi matches on
    /// the string rather than on an index it never saw.
    pub fn offered_options(payload: &Value) -> Vec<String> {
        payload
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .map(|option| match option {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `extension_ui_response` for one answered dialog.
    ///
    /// # The three shapes are not interchangeable
    ///
    /// From `pi-coding-agent/docs/rpc.md` § Extension UI Responses: a `select`/`input`/`editor` wants
    /// `value`, a `confirm` wants `confirmed`, and a dismissal wants `cancelled`. pi
    /// distinguishes **"the user said no" from "the user dismissed it"** — an extension
    /// receives `false` for the first and `undefined` for the second — so collapsing them
    /// would hand an extension a decision the user never made.
    ///
    /// `offered` is what pi listed, or `None` for a dialog that offered nothing (an `input`
    /// or `editor`). **An answer naming nothing pi offered becomes a dismissal**, not a
    /// guess: pi matches on the literal string, so a value it never sent is not an answer it
    /// can act on, and picking the nearest would be inventing a choice.
    pub fn response_for(resolution: &Resolution, offered: Option<&[String]>) -> Value {
        let vendor_id = &resolution.request_id;
        match resolution.option.as_str() {
            CANCEL => cancel_response(vendor_id),
            DENY => json!({
                "type": "extension_ui_response",
                "id": vendor_id,
                "confirmed": false,
            }),
            ALLOW => json!({
                "type": "extension_ui_response",
                "id": vendor_id,
                "confirmed": true,
            }),
            // `submit` on a select, an input, or an editor.
            _ => {
                let value = answer_text(resolution.input.as_ref());
                if let Some(offered) = offered {
                    if !offered.iter().any(|option| option == &value) {
                        return cancel_response(vendor_id);
                    }
                }
                json!({
                    "type": "extension_ui_response",
                    "id": vendor_id,
                    "value": value,
                })
            }
        }
    }

    /// The dismissal payload, which is also what an unknown method is answered with.
    pub fn cancel_response(vendor_id: &str) -> Value {
        json!({
            "type": "extension_ui_response",
            "id": vendor_id,
            "cancelled": true,
        })
    }

    /// The text out of a resolution's `input`, whichever shape a client sent.
    ///
    /// A declared form resolves to `{values: {…}}`, and only one field is ever declared
    /// here — so whichever of the two known ids is present is the answer. The loose
    /// fallbacks exist because a client may answer a request it did not render, and an
    /// answer that reaches the vendor as `"{}"` is worse than one read out of an
    /// unexpected key.
    pub fn answer_text(input: Option<&Value>) -> String {
        let Some(input) = input else {
            return String::new();
        };
        if let Some(text) = input.as_str() {
            return text.to_string();
        }
        if let Some(values) = input.get("values") {
            for key in [CHOICE_FIELD, TEXT_FIELD] {
                if let Some(text) = values.get(key).and_then(Value::as_str) {
                    return text.to_string();
                }
            }
            if let Some(object) = values.as_object() {
                if let Some(text) = object.values().find_map(Value::as_str) {
                    return text.to_string();
                }
            }
        }
        for key in ["value", "text", "input"] {
            if let Some(text) = input.get(key).and_then(Value::as_str) {
                return text.to_string();
            }
        }
        input.to_string()
    }
}

/// Native source metadata; semantic interpretation is performed after capture.
pub mod native {
    use anyhow::Result;
    use sylph_protocol::{Fidelity, NativeId, NativeSource, NativeSummary, StoreFilter};

    pub const NATIVE_FIDELITY: Fidelity = Fidelity::Messages;

    pub fn enumerate(filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        crate::native::inventory::enumerate(super::AGENT_NAME, filter)
    }

    pub fn lookup(id: &NativeId) -> Result<Option<NativeSummary>> {
        crate::native::inventory::lookup(super::AGENT_NAME, id)
    }

    pub fn sources(id: &NativeId) -> Result<Vec<NativeSource>> {
        crate::native::inventory::sources(super::AGENT_NAME, id)
    }
}

/// Held RPC process, configuration, human requests, and operational state.
pub mod session {
    use crate::now_ms;
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::{anyhow, Context, Result};
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sylph_protocol::{
        AdapterError, ConfigOption, ControlNotice, ControlStream, NativeId, RawCaptureSink,
        RequestedNativeId, Resolution, RunSpec, Session, SessionCapabilities, SessionMessage,
        TurnInput,
    };
    use tokio::sync::{mpsc, Mutex};
    use tracing::{debug, warn};

    // `STANDARD` rather than one of the URL-safe or unpadded variants: pi's `ImageContent.data`
    // is the same base64 a data URL carries, so `+`/`/` with padding is the alphabet the vendor
    // decodes. A variant is not a style choice — `STANDARD_NO_PAD` produces a string the
    // vendor's own decoder rejects.
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    use crate::process::{self, Child, Spawn};

    use super::{rpc, ui};
    use sylph_protocol::trace::ObservationRef;

    /// What pi offered for one open dialog, keyed by the vendor's own request id.
    ///
    /// The value is `Some(options)` for a `select` and **`None` for every other dialog**,
    /// and the distinction is load-bearing rather than tidiness: [`ui::response_for`] turns
    /// an answer naming nothing in the offered list into a dismissal, so passing
    /// `Some(&[])` for an `input` would turn every submitted string into a cancel. A
    /// dialog that offered nothing has to be `None`, not empty.
    type Dialogs = Arc<Mutex<HashMap<String, Option<Vec<String>>>>>;

    /// One held `pi --mode rpc` process, as a [`Session`].
    pub struct PiSession {
        native_id: NativeId,
        identity_evidence: ObservationRef,
        child: Arc<Child>,
        /// Yielded **once** by [`Session::messages`]. Two consumers of one vendor's stdio
        /// would each see half the turn, which is the same reason
        /// [`crate::process::Child`] reads stdout in exactly one task.
        stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
        /// The catalogue, cached for the session's life.
        ///
        /// *Measured:* 372 entries on a real install, and it is a property of the user's
        /// configured providers rather than of the session — so re-reading it per
        /// `capabilities()` would pay for a listing that cannot have changed. **Only the
        /// catalogue is cached.** Current values are read from the vendor every time
        /// ([`PiSession::read_surface`]).
        models: Arc<Mutex<Option<Value>>>,
        dialogs: Dialogs,
        /// The identity preamble's file, deleted on [`Session::close`].
        preamble: Option<std::path::PathBuf>,
    }

    impl PiSession {
        /// Spawn `pi --mode rpc` and hold it.
        ///
        /// Refuses [`RequestedNativeId::New`] rather than letting the vendor assign one:
        /// [`sylph_protocol::AgentInfo::dictates_native_id`] is true for this agent, so the
        /// envagent names an id, and the alternative — minting one here — is the
        /// synthesized-identity failure [`NativeId`] forbids and the Python original
        /// committed — v1 minted one with `uuid4()`.
        pub async fn open(spec: RunSpec, capture: Arc<dyn RawCaptureSink>) -> Result<Self> {
            let requested = match &spec.native_id {
                RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => id.clone(),
                RequestedNativeId::New => {
                    return Err(anyhow!(
                        "pi dictates its session id: open it with a Dictate or Resume id, \
                         because an id this adapter minted would look like the vendor's and \
                         be Sylph's"
                    ))
                }
            };

            if spec.permission_mode.is_some() {
                // Reported rather than silently dropped. pi has no permission mode at all —
                // a tool runs when the model calls it — so honouring this would promise a
                // policy the session is not running under.
                warn!("pi has no permission mode; ignoring the one the server asked for");
            }

            // Written before the argv is finalized, because the flag needs a path that
            // exists: `resolvePromptInput` (`core/resource-loader.js:391`) reads its
            // argument as a *file* when the string names one and as literal text otherwise.
            // A file is what makes that unambiguous — a one-line preamble that happened to
            // name a real path would otherwise be read as that file's contents.
            let preamble = match (spec.inject_identity, spec.identity_preamble.as_deref()) {
                (true, Some(text)) if !text.is_empty() => Some(write_preamble(text)?),
                _ => None,
            };

            let (argv, warnings) = super::PiAdapter::argv_for(&spec);
            for warning in warnings {
                // Logged by the caller of `argv_for` rather than inside it: substituting
                // something a user asked for silently is exactly the help an adapter must
                // not give.
                warn!("{warning}");
            }
            let spawn = Spawn {
                program: super::AGENT_NAME.to_string(),
                args: spawn_args(argv, preamble.as_deref()),
                cwd: spec.workdir.clone(),
                env: HashMap::new(),
                env_remove: Vec::new(),
                reply_id: Some(Box::new(reply_id)),
            };
            let attached = Self::attach(spawn, requested, preamble.clone(), capture).await;
            if attached.is_err() {
                // The file is this adapter's, and a failed launch is the one path where
                // nothing else will ever delete it.
                if let Some(path) = &preamble {
                    let _ = std::fs::remove_file(path);
                }
            }
            attached
        }

        /// Hold an already-described process, and start the pump.
        ///
        /// Separate from [`Self::open`] so a test can hand it a **scripted stdin/stdout
        /// peer** instead of the real CLI. That separation is the reason this half is
        /// testable at all: a test that needs `pi` installed, configured, and answering is a
        /// test that does not run — and every classification defect in this adapter's
        /// history was invisible to an end-to-end check, which spends real tokens and sees only the
        /// shapes one live turn happens to produce.
        pub async fn attach(
            spawn: Spawn,
            native_id: NativeId,
            preamble: Option<std::path::PathBuf>,
            capture: Arc<dyn RawCaptureSink>,
        ) -> Result<Self> {
            let (child, lines) = Child::spawn(spawn, capture).await?;
            let child = Arc::new(child);

            // Bounded, and generously: a turn's deltas arrive faster than a server folds
            // them, and a channel small enough to block here would make this adapter's own
            // backpressure part of what a streaming measurement measures.
            let (messages, stream) = mpsc::channel(1024);
            let models = Arc::new(Mutex::new(None));
            let refreshing = Arc::new(Mutex::new(()));
            let dialogs = Arc::new(Mutex::new(HashMap::new()));
            let identified = Arc::new(std::sync::atomic::AtomicBool::new(false));
            tokio::spawn(pump(Pump {
                lines,
                messages,
                child: child.clone(),
                native_id: native_id.clone(),
                identified: identified.clone(),
                dialogs: dialogs.clone(),
                models: models.clone(),
                refreshing: refreshing.clone(),
                snapshot: Arc::new(Mutex::new(SnapshotState::default())),
            }));

            // An argv request is not proof of identity. Only the retained vendor reply
            // establishes which session this process actually opened.
            let verified = async {
                let reply = child
                    .request_with_receipt(|id| json!({"id": id.to_string(), "type": "get_state"}))
                    .await?;
                let state = response_data("get_state", &reply.value)?;
                let actual = state
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("pi get_state omitted its sessionId"))?;
                if actual != native_id.as_str() {
                    return Err(anyhow!(
                        "pi opened session {actual:?}, expected {:?}",
                        native_id.as_str()
                    ));
                }
                Ok::<_, anyhow::Error>(reply.observation)
            }
            .await;
            let identity_evidence = match verified {
                Ok(evidence) => evidence,
                Err(error) => {
                    child.close().await;
                    return Err(error);
                }
            };
            identified.store(true, std::sync::atomic::Ordering::Release);
            let session = Self {
                native_id,
                identity_evidence,
                child,
                stream: Mutex::new(Some(stream)),
                models,
                dialogs,
                preamble,
            };

            Ok(session)
        }

        /// One RPC command, awaiting the reply that carries its id.
        ///
        /// **Reads the discriminant before the payload.** pi answers a refusal with
        /// `success: false` and an `error` string and *no* `data`, so a reader that went
        /// straight for `data` would turn every refusal into the same empty value a
        /// successful no-payload command returns — which is precisely the defect
        /// `AGENTS.md` records against `claude`'s adapter, where selecting a refused
        /// permission mode was recorded as applied and answered 200.
        async fn call(&self, command: &str, fields: Value) -> Result<Value> {
            call(&self.child, command, fields).await
        }

        /// The config surface, read from the vendor.
        ///
        /// **Every current value comes from this call and nothing caches one.** The Python
        /// original answered `currentValue: self.current_model`, a field only one code path
        /// populated, so a surface built by any other path — a re-attach, a `set_config`
        /// reply — reported `null` while the session plainly had a model and the composer
        /// chip fell back to "agent default" with the checkmark on no row. The vendor is
        /// also the only thing that knows whether a set was *honoured*, and pi's own TUI can
        /// steer a session besides.
        async fn read_surface(&self) -> Result<rpc::ConfigSurface> {
            read_surface(&self.child, &self.models).await
        }
    }

    #[async_trait]
    impl Session for PiSession {
        fn native_id(&self) -> Option<NativeId> {
            Some(self.native_id.clone())
        }

        async fn identity_evidence(&self) -> Vec<ObservationRef> {
            vec![self.identity_evidence.clone()]
        }

        /// The whole surface, every field of it read from the vendor.
        ///
        /// `get_commands` is a fourth round trip beyond [`rpc::SURFACE_ROUND_TRIPS`] and is
        /// **not** cached: pi has no "your commands changed" event — the list is polled, not
        /// pushed — so a cache here would go stale the moment an extension loads and there
        /// would be nothing to invalidate it.
        async fn capabilities(&self) -> Result<SessionCapabilities> {
            let surface = self.read_surface().await?;
            let commands = match self.call("get_commands", json!({})).await {
                Ok(payload) => rpc::command_specs(&payload),
                Err(error) => {
                    // Degraded rather than fatal: a session with no command list is usable,
                    // and refusing to report a surface over it would take the model picker
                    // down with the slash menu.
                    warn!("pi did not answer get_commands: {error}");
                    Vec::new()
                }
            };
            Ok(surface.capabilities(commands))
        }

        /// Start a turn, returning once pi has **accepted** it.
        ///
        /// *Measured at `rpc-mode.js:298`:* `prompt` answers as soon as its preflight
        /// succeeds, and a failure *after* acceptance is reported through the event stream
        /// rather than as a second response for the same id. That is exactly the contract —
        /// a turn outlives the call that started it — so the reply is awaited and nothing
        /// waits for the turn.
        async fn send(&self, turn: TurnInput) -> Result<()> {
            // Applied before the prompt, which is why they travel *with* the turn: separate
            // preceding calls left a window where a turn could run under a mix of old and
            // new settings.
            //
            // **pi has no per-turn scope**, so these persist for the session. Recorded here
            // rather than worked around: the vendor offers `set_model` and
            // `set_thinking_level` and nothing narrower, and pretending otherwise would
            // require restoring the previous value afterwards — which would then be wrong
            // for anyone who changed it from pi's own TUI mid-turn.
            for (id, value) in &turn.config {
                if let Err(error) = self.set_config(id, value.clone()).await {
                    // Not fatal: running the turn under the previous value beats losing the
                    // turn over one setting, and the refusal is already the caller's to see
                    // through `set_config` proper.
                    warn!("could not apply {id}={value}: {error}");
                }
            }

            // A command becomes prompt text, which is what pi's own input expansion reads:
            // `/skill:name` and `/template` are expanded before sending, and an extension
            // command runs immediately. A **control** command never reaches here — it is
            // typed state and goes through `set_config` (`sylph_protocol::TurnInput`).
            let text = match &turn.command {
                Some(command) => {
                    let args = turn
                        .args
                        .values()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("/{command} {args}").trim().to_string()
                }
                None => turn.text.clone(),
            };

            let (message, images) = build_message(&text, &turn.attachments);
            let mut fields = json!({"message": message});
            if !images.is_empty() {
                if let Some(object) = fields.as_object_mut() {
                    object.insert("images".to_string(), Value::Array(images));
                }
            }
            self.call("prompt", fields).await?;
            Ok(())
        }

        /// Start a prepared link's first turn.  pi exposes its session id during the
        /// handshake, so the adapter can answer immediately while the native store remains
        /// empty until the prompt actually writes a message.
        async fn send_initial(&self, turn: TurnInput) -> Result<NativeId> {
            self.send(turn).await?;
            Ok(self.native_id.clone())
        }

        /// End the turn with pi's `abort`, **without awaiting its reply**.
        ///
        /// *Measured at `agent-session.js:1171`:* `abort()` calls `waitForIdle()` before
        /// returning, so the response lands *after* `agent_settled`. Awaiting it would tie
        /// this call to however long a running tool takes to notice the abort, and past
        /// [`process::REPLY_TIMEOUT`] it would report a failure for an abort that worked.
        ///
        /// Nothing is synthesized either: `_emitAgentSettled` sits in the `finally` of
        /// `_runAgentPrompt` (`agent-session.js:758`), so an aborted turn emits
        /// `agent_settled` carrying `stopReason: "aborted"` on the `AssistantMessage` before
        /// it. A close written here would be this adapter asserting an outcome the vendor
        /// reports — and `aborted` is a *user stopping a turn*, not a failure, so writing
        /// one would also colour an interrupted turn as a broken one.
        async fn interrupt(&self) -> Result<()> {
            self.child
                .send(json!({"type": "abort"}))
                .await
                .context("pi would not accept an abort")
        }

        // The RPC queue insertion is unconditional (`AgentSession._queueSteer`). A
        // local turn-open read races vendor settlement, even while holding our mutex.
        // Preserve the native primitive under its own delivery semantics.
        async fn steer(&self, _text: &str, _expected_turn_id: Option<&str>) -> Result<()> {
            Err(sylph_protocol::AdapterError::unsupported_field(
                "Pi offers a native steering queue, not turn-scoped steering",
            )
            .into())
        }

        async fn queue_steering(&self, text: &str) -> Result<()> {
            self.call("steer", json!({"message": text})).await?;
            Ok(())
        }

        /// Queue text for after the running turn: this vendor's `follow_up`.
        ///
        /// **The verb is `follow_up`, not `queueUserMessage`.** That name occurs nowhere in
        /// this vendor's tree and is an SDK-side spelling at best — recorded because grepping
        /// for it finds nothing while the feature is plainly there, which reads as an absent
        /// capability rather than as a wrong guess. `dist/modes/rpc/rpc-mode.js:325`.
        ///
        /// Same payload as [`Self::steer`], and the difference is entirely *when* the agent
        /// reads it: pi delivers a follow-up once no tool calls and no steering messages
        /// remain. No turn-open precondition, because queueing with nothing running is a
        /// coherent ask — it is the next turn's opening text.
        async fn queue(&self, text: &str) -> Result<()> {
            self.call("follow_up", json!({"message": text})).await?;
            Ok(())
        }

        /// Set one option and answer with the **refreshed array**.
        ///
        /// Three things happen in order, and each is one of this vendor's measured hazards:
        ///
        /// 1. [`rpc::command_for`] decides the command **and the refusal code**. An unknown
        ///    id is `UNSUPPORTED_FIELD`; a malformed value is `INVALID_VALUE`. The two are
        ///    not interchangeable — one justifies disabling a control, the other must leave
        ///    it usable — and answering a bad *value* with "no such option" sends the
        ///    investigation to the layer that is not at fault.
        /// 2. The value is validated against **what the vendor enumerated**, not against the
        ///    launch flag's seven thinking levels, which are strictly wider than what a
        ///    given model accepts. This is not defensive duplication: *measured at
        ///    `agent-session.js:1290`*, `setThinkingLevel` **clamps** an unsupported level
        ///    to the nearest supported one and answers `success: true`, so without this
        ///    check a refused write would be indistinguishable from an applied one.
        /// 3. The surface is re-read from the vendor afterwards, so what comes back is what
        ///    was **confirmed** rather than what was requested — and re-reading is also what
        ///    makes the answer an array rather than an acknowledgement, because setting the
        ///    model changes which thinking levels exist
        ///    ([`rpc::ConfigSurface::after_model_change`]).
        async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
            let command = rpc::command_for(id, &value).map_err(anyhow::Error::new)?;

            let before = self.read_surface().await?;
            if !before.accepts(id, &value) {
                return Err(anyhow::Error::new(AdapterError::invalid_value(format!(
                    "pi does not currently offer {value} for '{id}'"
                ))));
            }

            let payload = command.as_value();
            let command_type = payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut fields = payload.clone();
            if let Some(object) = fields.as_object_mut() {
                object.remove("type");
            }
            // A vendor refusal is `INVALID_VALUE`: `command_for` already established the
            // option exists, so whatever pi objected to is about this value.
            self.call(&command_type, fields).await.map_err(|error| {
                anyhow::Error::new(AdapterError::invalid_value(error.to_string()))
            })?;

            Ok(self.read_surface().await?.options())
        }

        fn launch_flag_for_config(&self, id: &str, value: &Value) -> Option<(String, String)> {
            let value = value.as_str()?.to_string();
            match id {
                "model" | "thinking" => Some((id.to_string(), value)),
                _ => None,
            }
        }

        /// Answer an `extension_ui_request`, which is the only thing pi ever waits on a
        /// human for.
        ///
        /// The three response shapes are not interchangeable — `value`, `confirmed`,
        /// `cancelled` — and pi distinguishes "the user said no" from "the user dismissed
        /// it", handing an extension `false` for the first and `undefined` for the second.
        /// [`ui::response_for`] holds that mapping; what this method holds is the **offered
        /// list**, which is why the pump remembers one per open dialog: pi matches a
        /// `select` on the literal string it sent, so an answer naming something it never
        /// offered is not an answer it can act on and becomes a dismissal rather than a
        /// guess at the nearest row.
        ///
        /// An id this session has no record of is still answered. The alternative is worse:
        /// pi blocks until its own timeout lapses, which presents as a session that stopped
        /// for no reason, and the vendor validates the id anyway.
        ///
        /// # Answering retires the dialog, and the leak that leaves is deliberate
        ///
        /// The record is **removed**, which bounds the map over a long session. Two
        /// consequences worth stating rather than discovering. A second answer to one id is
        /// no longer filtered against the offered list — acceptable, because pi discards an
        /// `extension_ui_response` whose id it has already resolved
        /// (`rpc-mode.js:617`, `pendingExtensionRequests.get`), so the vendor is the one
        /// enforcing single-answer and this adapter would only be duplicating it. And a
        /// dialog pi **auto-resolved on its own timeout** is never answered here, so its entry
        /// stays: pi does not announce that it timed out, so there is nothing to remove it on.
        /// Bounded by the dialogs one session raises, which is why it is left rather than
        /// swept — a sweep would need a deadline this side cannot observe the expiry of.
        ///
        /// **What a client observes when that happens**, since the sentence above describes the
        /// map rather than the symptom: the dialog stays on screen. Nothing retracts a published
        /// [`sylph_protocol::Request`], and its `expires_at_ms` is advisory — the server never
        /// acts on it. Answering then still succeeds here and pi discards it, having already
        /// resolved the dialog with its own default, so the user's answer is silently not the
        /// one the extension received. The reading that follows for a client is to *render* the
        /// deadline rather than to trust the row to disappear at it.
        async fn resolve(&self, resolution: Resolution) -> Result<()> {
            let offered = self.dialogs.lock().await.remove(&resolution.request_id);
            let response = ui::response_for(&resolution, offered.flatten().as_deref());
            self.child
                .send(response)
                .await
                .context("pi would not accept a dialog answer")
        }

        fn messages(&mut self) -> Option<ControlStream> {
            // `try_lock` because this is the one sync method on an otherwise async trait,
            // and the lock is uncontended by construction: `&mut self` means nothing else
            // holds this session. Taking the receiver is what makes a second call `None`.
            let receiver = self.stream.try_lock().ok()?.take()?;
            Some(Box::pin(futures::stream::unfold(
                receiver,
                |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
            )))
        }

        /// Ask the group to exit, then make sure it did.
        ///
        /// Idempotent and escalating unconditionally, because the idle reaper, an explicit
        /// detach, and shutdown can all reach it — and a `close` the vendor ignores would
        /// otherwise leak a process for the server's life.
        /// [`crate::process::Child::close`] owns the escalation and the process *group*, so
        /// a stop reaches the shell pi spawned for a tool call rather than only pi.
        async fn close(&self) -> Result<()> {
            self.child.close().await;
            if let Some(path) = &self.preamble {
                // Best effort by design: the file is in a temp directory and a failure to
                // unlink it must not make a close report failure.
                let _ = std::fs::remove_file(path);
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct SnapshotState {
        cursor: Option<String>,
        degraded: bool,
    }

    struct Pump {
        lines: mpsc::Receiver<process::Line>,
        messages: mpsc::Sender<Result<SessionMessage>>,
        child: Arc<Child>,
        native_id: NativeId,
        identified: Arc<std::sync::atomic::AtomicBool>,
        dialogs: Dialogs,
        models: Arc<Mutex<Option<Value>>>,
        refreshing: Arc<Mutex<()>>,
        snapshot: Arc<Mutex<SnapshotState>>,
    }

    /// Only operational state crosses this channel. All vendor bytes, including malformed
    /// frames, replies and unknown events, have already been durably captured by `Child`.
    async fn pump(mut state: Pump) {
        let mut active = false;
        let mut last_model = None;
        let mut poll = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        );
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let line = tokio::select! {
                line = state.lines.recv() => line,
                _ = poll.tick() => { refresh_snapshot(&state); continue; }
            };
            let Some(line) = line else {
                break;
            };
            let payload = match line {
                Ok(line) => line.value,
                Err(error) => {
                    if state.messages.send(Err(error)).await.is_err() {
                        break;
                    }
                    continue;
                }
            };
            let notice = match payload.get("type").and_then(Value::as_str) {
                Some("extension_ui_request") => {
                    if !handle_ui_request(&state, &payload, now_ms()).await {
                        break;
                    }
                    None
                }
                Some("agent_start") if !active => {
                    active = true;
                    Some(ControlNotice::Active {
                        native_id: state.native_id.clone(),
                        native_turn_id: None,
                    })
                }
                Some("agent_settled") => {
                    active = false;
                    refresh_snapshot(&state);
                    Some(ControlNotice::Settled {
                        native_id: state.native_id.clone(),
                        native_turn_id: None,
                    })
                }
                Some("message_start" | "message_end") => {
                    if let Some(message) = payload.get("message") {
                        if let (Some(provider), Some(model)) = (
                            message.get("provider").and_then(Value::as_str),
                            message.get("model").and_then(Value::as_str),
                        ) {
                            let observed = rpc::model_key(Some(provider), model);
                            if last_model.as_ref() != Some(&observed) {
                                last_model = Some(observed);
                                refresh_config(&state);
                            }
                        }
                    }
                    None
                }
                Some("response") => {
                    debug!("pi answered an uncorrelated control request");
                    None
                }
                _ => None,
            };
            if let Some(notice) = notice {
                if state
                    .messages
                    .send(Ok(SessionMessage::Control(notice)))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    /// Observe selected history even when navigation appends no native entry. The vendor
    /// has no page-size parameter: its complete response is captured under transport limits.
    /// `since` names a previously observed entry, never a locally invented ordinal.
    fn refresh_snapshot(state: &Pump) {
        if !state.identified.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let Ok(mut cursor) = state.snapshot.clone().try_lock_owned() else {
            return;
        };
        let child = state.child.clone();
        tokio::spawn(async move {
            let fields = cursor
                .cursor
                .as_ref()
                .map(|id| json!({"since": id}))
                .unwrap_or_else(|| json!({}));
            match call(&child, "get_entries", fields).await {
                Ok(data) => {
                    cursor.degraded = false;
                    if let Some(last) = data
                        .get("entries")
                        .and_then(Value::as_array)
                        .and_then(|entries| entries.last())
                        .and_then(|entry| entry.get("id"))
                        .and_then(Value::as_str)
                    {
                        cursor.cursor = Some(last.to_string());
                    }
                }
                Err(error) => {
                    // A rewritten source may retire the cursor. The next observation asks
                    // for a new baseline; the failed response remains retained evidence.
                    cursor.cursor = None;
                    if cursor.degraded {
                        debug!("pi entry snapshot remains unavailable: {error}");
                    } else {
                        warn!("could not capture pi entry snapshot: {error}");
                        cursor.degraded = true;
                    }
                }
            }
        });
    }

    async fn handle_ui_request(state: &Pump, payload: &Value, at_ms: i64) -> bool {
        match ui::inversion(payload, at_ms) {
            ui::Inversion::Ask(request) => {
                let offered = match payload.get("method").and_then(Value::as_str) {
                    Some("select") => Some(ui::offered_options(payload)),
                    _ => None,
                };
                state
                    .dialogs
                    .lock()
                    .await
                    .insert(request.id.clone(), offered);
                state
                    .messages
                    .send(Ok(SessionMessage::Request(request)))
                    .await
                    .is_ok()
            }
            ui::Inversion::Notice(_) | ui::Inversion::Ignore => true,
            ui::Inversion::Unsupported { method, vendor_id } => {
                if let Err(error) = state.child.send(ui::cancel_response(&vendor_id)).await {
                    warn!("could not dismiss unsupported pi dialog {method}: {error}");
                }
                true
            }
        }
    }

    /// Re-read the config surface and publish it as a `config`-only facet.
    ///
    /// # Why this is spawned, and why that is safe
    ///
    /// It costs round trips, and doing them inline would stall every event behind them —
    /// including a `tool_result` a user is watching for. A spawned task cannot stall the
    /// pump, and it cannot deadlock either: [`Child::request`] is woken by the reader task
    /// that owns stdout, not by this pump.
    ///
    /// What a spawn *does* risk is two refreshes publishing out of order, which
    /// the shared refresh lock prevents by covering both the read and publish.
    /// A stream position is not at stake the way it is for a request: a facet is idempotent
    /// state a client reconciles against, not an event that means something different a few
    /// rows earlier or later.
    fn refresh_config(state: &Pump) {
        let child = state.child.clone();
        let models = state.models.clone();
        let messages = state.messages.clone();
        let refreshing = state.refreshing.clone();
        tokio::spawn(async move {
            let _order = refreshing.lock().await;
            match read_surface(&child, &models).await {
                Ok(surface) => {
                    let facets = sylph_protocol::CapabilityFacets::config(surface.options());
                    let _ = messages
                        .send(Ok(SessionMessage::Capabilities(facets)))
                        .await;
                }
                // Degraded, not fatal, and said once per occurrence rather than retried: the
                // surface a client already holds stays correct except for this one change,
                // and `capabilities()` re-reads it on the next attach.
                Err(error) => warn!("could not refresh pi's config surface: {error}"),
            }
        });
    }

    /// The id off a frame that is a **reply to something this side sent**, or `None`.
    ///
    /// # The `type` check is the whole function, and reading only `id` is a silent defect
    ///
    /// pi multiplexes both directions onto one stdout, and `extension_ui_request` is
    /// **vendor-initiated** while carrying an `id` of its own. A predicate reading only `id`
    /// would route one into whichever waiter happened to hold that number: the request would
    /// never reach the event stream, so nobody is ever asked and the turn waits on a human
    /// forever, while the caller that sent `get_state` receives a UI request as its answer.
    /// Both directions fail, and neither reports anything.
    ///
    /// *Measured on `codex`*, whose server-request ids collide with ours outright — `0,1,2`
    /// against `1..5`. pi's are `crypto.randomUUID()` (`rpc-mode.js:50`), so a collision
    /// there is presently improbable rather than certain; the check does not depend on that,
    /// because "improbable today" is a property of one vendor's id minting rather than of the
    /// protocol, and a correlation that is right by coincidence is one nobody re-examines.
    ///
    /// `type: "response"` is pi's own direction marker — each vendor spells it differently,
    /// which is why the knowledge lives here and not in [`crate::process`]. This is also what
    /// keeps replies separate from operational notifications.
    ///
    /// # A string, and deliberately not "a string or a number"
    ///
    /// pi echoes the id **verbatim**, whatever JSON type it arrived as — `rpc-mode.js:31`
    /// builds `{ id, type: "response", … }` from `command.id` with no coercion — and [`call`]
    /// only ever sends `id.to_string()`. So the number case is unreachable from this adapter,
    /// and accepting one would be tolerance for a shape this side does not produce: were pi
    /// ever to answer a number, correlation would break outright and every request would time
    /// out, which is the loud failure `AGENTS.md` § Compatibility asks for over a fallback
    /// that silently absorbs the change.
    fn reply_id(frame: &Value) -> Option<String> {
        if frame.get("type").and_then(Value::as_str) != Some("response") {
            return None;
        }
        frame
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }

    /// One RPC command, awaiting the reply that carries its id.
    ///
    /// A free function because the pump's refresh needs it and the session's methods do too,
    /// and a containment rule — or a discriminant check — written twice is one that will
    /// diverge.
    ///
    /// The reply's `data` is answered, or `{}` for a command that carries none. **`{}` is
    /// only ever reachable from a success**: a refusal returns `Err`, because a shape that
    /// two opposite outcomes can produce is how a refused control reports as applied.
    async fn call(child: &Child, command: &str, fields: Value) -> Result<Value> {
        let reply = child
            .request(|id| {
                let mut payload = json!({"id": id.to_string(), "type": command});
                if let (Some(object), Some(extra)) = (payload.as_object_mut(), fields.as_object()) {
                    for (key, value) in extra {
                        object.insert(key.clone(), value.clone());
                    }
                }
                payload
            })
            .await
            .with_context(|| format!("pi did not answer `{command}`"))?;

        response_data(command, &reply)
    }

    fn response_data(command: &str, reply: &Value) -> Result<Value> {
        // The discriminant first. A refusal carries `error` and no `data`.
        if reply.get("success").and_then(Value::as_bool) != Some(true) {
            let reason = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("pi refused it and said nothing further");
            return Err(sylph_protocol::VendorRefusal {
                message: format!("pi refused `{command}`: {reason}"),
            }
            .into());
        }
        Ok(reply.get("data").cloned().unwrap_or_else(|| json!({})))
    }

    /// The three round trips [`rpc::SURFACE_ROUND_TRIPS`] names, in that order.
    ///
    /// Only the catalogue is cached. `get_state` and `get_available_thinking_levels` are read
    /// every time, because the first holds every current value and the second answers for the
    /// *currently selected model* — the model measured during this vendor's investigation
    /// offered `off` and `high` against the launch flag's seven, so a cached list goes stale
    /// the moment the model changes.
    async fn read_surface(
        child: &Child,
        models: &Arc<Mutex<Option<Value>>>,
    ) -> Result<rpc::ConfigSurface> {
        let state = call(child, "get_state", json!({})).await?;

        let catalogue = {
            let mut held = models.lock().await;
            match held.as_ref() {
                Some(cached) => cached.clone(),
                None => {
                    let fetched = call(child, "get_available_models", json!({})).await?;
                    *held = Some(fetched.clone());
                    fetched
                }
            }
        };

        let levels = call(child, "get_available_thinking_levels", json!({})).await?;
        Ok(rpc::ConfigSurface::from_vendor(&state, &catalogue, &levels))
    }

    /// [`super::PiAdapter::argv_for`]'s output as a [`Spawn::args`], with the preamble
    /// placeholder resolved.
    ///
    /// A named function rather than a chain inside [`PiSession::open`], because the thing it
    /// has to get right is invisible from above: [`super::IDENTITY_PREAMBLE_PLACEHOLDER`]
    /// exists so that a caller which *forgets* to substitute fails visibly rather than
    /// passing the marker to the vendor as prompt text — and the only way to observe that is
    /// to look at the argv, which nothing downstream of a spawn can do.
    fn spawn_args(argv: Vec<String>, preamble: Option<&std::path::Path>) -> Vec<String> {
        argv.into_iter()
            // The program name, which `Spawn` carries separately.
            .skip(1)
            .map(|part| match (preamble, part.as_str()) {
                (Some(path), super::IDENTITY_PREAMBLE_PLACEHOLDER) => {
                    path.to_string_lossy().to_string()
                }
                _ => part,
            })
            .collect()
    }

    /// Write the identity preamble where `--append-system-prompt` can read it.
    ///
    /// A file rather than the text itself, because that flag reads its argument as a *file*
    /// when the string names one (`core/resource-loader.js:391`, `resolvePromptInput`) — so a
    /// preamble that happened to name a real path would be silently replaced by that file's
    /// contents. The name carries a fresh UUID, which is an identifier for a temporary file
    /// and never a session id: minting one of those is what [`NativeId`] forbids.
    fn write_preamble(text: &str) -> Result<std::path::PathBuf> {
        let path = std::env::temp_dir().join(format!("sylph-pi-soul-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&path, text)
            .with_context(|| format!("could not write the identity preamble to {path:?}"))?;
        Ok(path)
    }

    /// One user message, with images inline and every other file named by path.
    ///
    /// pi takes images as `ImageContent` blocks and reads text files perfectly well with its
    /// own `read` tool, so the split is not a convenience: inlining a text file bloats the
    /// message with something the agent can fetch on demand, while a *path* to an image is
    /// something the model cannot see at all.
    ///
    /// An unreadable or unrecognized file is **named rather than dropped**, because the user
    /// attached it deliberately and a silently missing attachment is indistinguishable from
    /// one the agent chose to ignore.
    fn build_message(text: &str, attachments: &[String]) -> (String, Vec<Value>) {
        let mut images = Vec::new();
        let mut named = Vec::new();
        for path in attachments {
            match crate::native::image_mime(std::path::Path::new(path)) {
                Some(mime) => match std::fs::read(path) {
                    Ok(bytes) => images.push(json!({
                        "type": "image",
                        "data": STANDARD.encode(&bytes),
                        "mimeType": mime,
                    })),
                    Err(error) => {
                        warn!("naming rather than inlining an unreadable image {path}: {error}");
                        named.push(path.clone());
                    }
                },
                None => named.push(path.clone()),
            }
        }

        if named.is_empty() {
            return (text.to_string(), images);
        }
        let mut body =
            String::from("Files attached for this turn (absolute paths on local disk):\n");
        for path in &named {
            body.push_str("- ");
            body.push_str(path);
            body.push('\n');
        }
        body.push('\n');
        body.push_str(text);
        (body, images)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::{Path, PathBuf};
        use std::time::Duration;
        use sylph_protocol::RefusalCode;

        /// A scripted `pi` peer: one `sh` process speaking the RPC surface.
        ///
        /// **Deliberately not the real CLI.** A test needing `pi` installed, configured, and
        /// answering — rather than 404ing, which is the failure that prompted this work — is a
        /// test that does not run. And a scripted peer is the only way to assert on the
        /// *outbound* half: it appends every line it receives to `$1`, so a test can assert
        /// that `set_model` carried `provider` and `modelId` rather than merely that the call
        /// returned `Ok`.
        ///
        /// `$2`, when given, is a file of event lines emitted before the read loop, which is
        /// how a test drives the pump.
        ///
        /// Two extraction rules matter. Both keys are read with `grep -o … | head -1` rather
        /// than `sed`, because `sed`'s `.*"type":"` is **greedy** — against a `prompt` carrying
        /// an image block it would read the last `"type"` in the line, `image`, and answer the
        /// wrong command. And `LEVEL` is mutated on `set_thinking_level` to a *different* value
        /// than the one requested, which is this vendor's measured clamping behaviour.
        /// `$3`, when `collide`, makes the peer emit an `extension_ui_request` **carrying the
        /// id of the command it is answering**, just before the real reply. That is the
        /// one-directional hazard [`reply_id`]'s `type` check exists for, and it can only be
        /// reproduced while a waiter is actually registered — an unsolicited request emitted
        /// before any command would land on the stream even with a broken predicate.
        const PEER: &str = r#"
LOG="$1"
EVENTS="$2"
COLLIDE="$3"
if [ -n "$EVENTS" ] && [ -f "$EVENTS" ]; then cat "$EVENTS"; fi
LEVEL=off
MODEL='{"id":"claude-opus-5","provider":"anthropic","name":"Claude Opus 5","contextWindow":200000}'
field() { printf '%s' "$2" | grep -o "\"$1\":\"[^\"]*\"" | head -1 | sed 's/.*:"//;s/"$//'; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG"
  t=$(field type "$line")
  i=$(field id "$line")
  case "$t" in
    get_state)
      if [ "$COLLIDE" = collide ]; then
        printf '{"type":"extension_ui_request","id":"%s","method":"select","title":"Pick","options":["Allow","Block"]}\n' "$i"
      fi
      printf '{"id":"%s","type":"response","command":"get_state","success":true,"data":{"model":%s,"thinkingLevel":"%s","sessionId":"scripted"}}\n' "$i" "$MODEL" "$LEVEL" ;;
    get_entries)
      case "$line" in
        *'"since":"entry-1"'*) ENTRIES='[]' ;;
        *) ENTRIES='[{"id":"entry-1","type":"custom","parentId":null,"data":{"retained":true}}]' ;;
      esac
      printf '{"id":"%s","type":"response","command":"get_entries","success":true,"data":{"entries":%s,"leafId":"entry-1"}}\n' "$i" "$ENTRIES" ;;
    get_available_models)
      printf '{"id":"%s","type":"response","command":"get_available_models","success":true,"data":{"models":[%s,{"id":"deepseek-v4-pro","provider":"deepseek","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-pro","provider":"openrouter","name":"DeepSeek V4 Pro"}]}}\n' "$i" "$MODEL" ;;
    get_available_thinking_levels)
      printf '{"id":"%s","type":"response","command":"get_available_thinking_levels","success":true,"data":{"levels":["off","low","high"]}}\n' "$i" ;;
    get_commands)
      printf '{"id":"%s","type":"response","command":"get_commands","success":true,"data":{"commands":[{"name":"review","description":"Review it","source":"prompt"}]}}\n' "$i" ;;
    set_thinking_level)
      LEVEL=low
      printf '{"id":"%s","type":"response","command":"set_thinking_level","success":true}\n' "$i" ;;
    set_model)
      printf '{"id":"%s","type":"response","command":"set_model","success":true,"data":%s}\n' "$i" "$MODEL" ;;
    prompt)
      printf '{"id":"%s","type":"response","command":"prompt","success":true}\n' "$i" ;;
    steer)
      printf '{"id":"%s","type":"response","command":"steer","success":true}\n' "$i" ;;
    abort)
      : ;;
    extension_ui_response)
      : ;;
    *)
      printf '{"id":"%s","type":"response","command":"%s","success":false,"error":"scripted peer has no %s"}\n' "$i" "$t" "$t" ;;
  esac
done
"#;

        /// A throwaway directory for one test's log and event files.
        ///
        /// The pid in the name makes it unique per run, and the `remove_dir_all` below
        /// makes it idempotent *within* a run. Neither removes it afterwards, so every
        /// `cargo test` used to leave its set behind: 392 of them had accumulated in
        /// `temp_dir()` here, which is also an E2E browse root, where a listing is
        /// silently truncated past `MAX_ENTRIES` (`crates/server/src/api/fs.rs`).
        ///
        /// So sweeping is part of creating one. Age-gated rather than pid-gated on
        /// purpose: a second `cargo test` running concurrently is a different pid, and
        /// deleting its scratch out from under it would fail a passing test for a
        /// reason nothing in its output could explain. An hour is far longer than the
        /// suite takes and far shorter than the gap between runs.
        fn scratch(name: &str) -> PathBuf {
            let temp = std::env::temp_dir();
            if let Ok(entries) = std::fs::read_dir(&temp) {
                let hour = std::time::Duration::from_secs(3600);
                for entry in entries.flatten() {
                    let stale = entry.file_name();
                    let Some(stale) = stale.to_str() else {
                        continue;
                    };
                    if !stale.starts_with("sylph-pi-session-") {
                        continue;
                    }
                    let old = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
                        .is_ok_and(|age| age > hour);
                    if old {
                        let _ = std::fs::remove_dir_all(temp.join(stale));
                    }
                }
            }

            let dir = temp.join(format!("sylph-pi-session-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn spawn_peer(log: &Path, events: Option<&Path>, collide: bool) -> Spawn {
            Spawn {
                program: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    PEER.to_string(),
                    "sh".to_string(),
                    log.to_string_lossy().to_string(),
                    events
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    if collide { "collide" } else { "" }.to_string(),
                ],
                cwd: "/tmp".to_string(),
                env: HashMap::new(),
                env_remove: Vec::new(),
                // The production predicate, not a test-local one: a peer correlating by a
                // rule the real session does not use would test the peer.
                reply_id: Some(Box::new(reply_id)),
            }
        }

        /// A session over the scripted peer, plus its outbound log.
        async fn scripted(name: &str, events: &[&str]) -> (PiSession, PathBuf) {
            scripted_peer(name, events, false).await
        }

        /// As [`scripted`], with the id-collision behaviour enabled. See [`PEER`].
        async fn scripted_colliding(name: &str) -> (PiSession, PathBuf) {
            scripted_peer(name, &[], true).await
        }

        async fn scripted_peer(name: &str, events: &[&str], collide: bool) -> (PiSession, PathBuf) {
            let dir = scratch(name);
            let log = dir.join("out.jsonl");
            std::fs::write(&log, "").unwrap();
            let events_path = dir.join("events.jsonl");
            let path = if events.is_empty() {
                None
            } else {
                std::fs::write(&events_path, format!("{}\n", events.join("\n"))).unwrap();
                Some(events_path.as_path())
            };
            let session = PiSession::attach(
                spawn_peer(&log, path, collide),
                NativeId::new("scripted"),
                None,
                crate::process::test_capture(),
            )
            .await
            .expect("the scripted peer must attach");
            (session, log)
        }

        /// Every line the session sent, once at least `count` of them have arrived.
        ///
        /// Polled rather than slept on: a fixed sleep is either flaky or slow, and the
        /// assertion a test wants is "this line was sent", not "it was sent within 20ms".
        async fn sent(log: &Path, count: usize) -> Vec<Value> {
            for _ in 0..200 {
                let lines: Vec<Value> = std::fs::read_to_string(log)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|line| line["type"] != "get_state" && line["type"] != "get_entries")
                    .collect();
                if lines.len() >= count {
                    return lines;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!(
                "only {:?} reached the peer, wanted {count}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }

        /// The next projected message off the stream, or `None` if none arrives in `ms`.
        ///
        /// A timeout rather than an unbounded await, so a test asserting that **nothing** is
        /// published can say so — which is what pins "a reply is not an event".
        async fn next_message(stream: &mut ControlStream, ms: u64) -> Option<SessionMessage> {
            use futures::StreamExt;
            tokio::time::timeout(Duration::from_millis(ms), async {
                stream.next().await.map(|item|item.expect("the scripted peer emits no read failures here"))
            })
            .await
            .ok()
            .flatten()
        }

        #[tokio::test]
        async fn identity_uses_the_retained_vendor_reply_and_keeps_unknown_bytes() {
            let dir = scratch("identity-capture");
            let log = dir.join("commands.jsonl");
            let events = dir.join("events.jsonl");
            let raw = b"{ \"type\": \"future_event\", \"value\": 1, \"value\": 2 }\r\n";
            std::fs::write(&events, raw).unwrap();
            let capture = Arc::new(crate::process::RecordingCapture::default());
            let session = PiSession::attach(
                spawn_peer(&log, Some(&events), false),
                NativeId::new("scripted"),
                None,
                capture.clone(),
            )
            .await
            .unwrap();
            let evidence = session.identity_evidence().await;
            assert_eq!(session.native_id().unwrap().as_str(), "scripted");
            assert_eq!(evidence.len(), 1);
            {
                let records = capture.records.lock().unwrap();
                let proof = records
                    .iter()
                    .find(|(_, receipt)| receipt == &evidence[0])
                    .unwrap();
                let response: Value = serde_json::from_slice(&proof.0.bytes).unwrap();
                assert_eq!(response["command"], "get_state");
                assert_eq!(response["data"]["sessionId"], "scripted");
                assert!(records.iter().any(|(record, _)| record.bytes == raw));
            }
            session.close().await.unwrap();
        }

        #[tokio::test]
        async fn an_argv_identity_mismatch_refuses_the_link() {
            let dir = scratch("wrong-identity");
            let result = PiSession::attach(
                spawn_peer(&dir.join("commands.jsonl"), None, false),
                NativeId::new("requested-but-not-opened"),
                None,
                crate::process::test_capture(),
            )
            .await;
            let error = result
                .err()
                .expect("the requested identity is not evidence");
            assert!(error.to_string().contains("opened session"));
        }

        #[tokio::test]
        async fn a_missing_vendor_identity_refuses_the_link() {
            let dir = scratch("missing-identity");
            let mut spawn = spawn_peer(&dir.join("commands.jsonl"), None, false);
            spawn.args[1] = PEER.replace(",\"sessionId\":\"scripted\"", "");
            let result = PiSession::attach(
                spawn,
                NativeId::new("scripted"),
                None,
                crate::process::test_capture(),
            )
            .await;
            assert!(result
                .err()
                .expect("identity must be reported")
                .to_string()
                .contains("omitted its sessionId"));
        }

        #[tokio::test]
        async fn inner_starts_keep_one_operational_interval_until_settlement() {
            let (mut session, _) = scripted(
                "outer-interval",
                &[
                    r#"{"type":"agent_start"}"#,
                    r#"{"type":"agent_end"}"#,
                    r#"{"type":"agent_start"}"#,
                    r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}"#,
                    r#"{"type":"agent_settled"}"#,
                ],
            )
            .await;
            let mut stream = session.messages().unwrap();
            assert!(matches!(
                next_message(&mut stream, 1000).await,
                Some(SessionMessage::Control(ControlNotice::Active {
                    native_turn_id: None,
                    ..
                }))
            ));
            assert!(matches!(
                next_message(&mut stream, 1000).await,
                Some(SessionMessage::Control(ControlNotice::Settled {
                    native_turn_id: None,
                    ..
                }))
            ));
            assert!(next_message(&mut stream, 100).await.is_none());
            session.close().await.unwrap();
        }

        #[tokio::test]
        async fn snapshots_retain_the_head_and_resume_from_a_vendor_entry() {
            let dir = scratch("snapshot-cursor");
            let capture = Arc::new(crate::process::RecordingCapture::default());
            let session = PiSession::attach(
                spawn_peer(&dir.join("commands.jsonl"), None, false),
                NativeId::new("scripted"),
                None,
                capture.clone(),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let observed = capture.records.lock().unwrap().iter().any(|(record, _)| {
                        serde_json::from_slice::<Value>(&record.bytes).is_ok_and(|value| {
                            value["type"] == "get_entries" && value["since"] == "entry-1"
                        })
                    });
                    if observed {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the next snapshot must use the vendor entry cursor");
            {
                let records = capture.records.lock().unwrap();
                assert!(records.iter().any(|(record, _)| {
                    serde_json::from_slice::<Value>(&record.bytes).is_ok_and(|value| {
                        value["command"] == "get_entries"
                            && value["data"]["leafId"] == "entry-1"
                            && value["data"]["entries"][0]["data"]["retained"] == true
                    })
                }));
                let state_at = records
                    .iter()
                    .position(|(record, _)| {
                        serde_json::from_slice::<Value>(&record.bytes)
                            .is_ok_and(|value| value["command"] == "get_state")
                    })
                    .unwrap();
                let entries_at = records
                    .iter()
                    .position(|(record, _)| {
                        serde_json::from_slice::<Value>(&record.bytes)
                            .is_ok_and(|value| value["type"] == "get_entries")
                    })
                    .unwrap();
                assert!(
                    state_at < entries_at,
                    "identity must precede entry snapshots"
                );
            }
            session.close().await.unwrap();
        }

        /// **The surface's current values come from the vendor, not from what was set.**
        ///
        /// The named defect on this adapter: `config()` answered `currentValue:
        /// self.current_model`, a field only one code path populated, so a surface built any
        /// other way reported `null` while the session plainly had a model.
        ///
        /// Asserted on a session that has set **nothing** — so every value here can only have
        /// come from `get_state`. A cached-what-we-set implementation answers `None` for both.
        #[tokio::test]
        async fn the_surface_reports_what_the_vendor_says_on_a_session_that_set_nothing() {
            let (session, _log) = scripted("vendor-surface", &[]).await;
            let surface = session.capabilities().await.unwrap();

            let model = surface.config_option("model").expect("a model option");
            assert_eq!(
                model.current_value.as_ref().and_then(Value::as_str),
                Some("anthropic claude-opus-5"),
                "the current model must be the vendor's, provider-qualified"
            );
            let thinking = surface
                .config_option("thinking")
                .expect("a thinking option");
            assert_eq!(
                thinking.current_value.as_ref().and_then(Value::as_str),
                Some("off")
            );
            // The per-model window is real for this agent, and it comes from the *selected
            // model's* catalogue entry rather than from a session-global field.
            assert_eq!(surface.context_window, Some(200_000));
            assert_eq!(surface.commands.len(), 1);
            assert_eq!(surface.commands[0].name, "review");
            // pi has no permission mode at all; publishing one would promise a policy the
            // session is not running under.
            assert_eq!(surface.permission_mode, None);
            // And no delegation, which for this vendor is a report rather than a gap.
            assert!(surface.subagents.is_empty());
        }

        /// **`set_model` carries `provider` and `modelId`, and never `model`.**
        ///
        /// *Measured on pi 0.84.3:* sending `model=` is answered `Model not found:
        /// undefined/undefined`, pi interpolating the two fields it looked for and found
        /// absent. The refusal came back as a failed RPC that nothing surfaced, so a correct
        /// picker looked broken and the investigation went to the UI twice.
        ///
        /// Asserted on the **line that reached the vendor**, which is the only place this is
        /// observable: every layer above returns `Ok` either way.
        #[tokio::test]
        async fn set_model_sends_provider_and_model_id_and_never_a_model_field() {
            let (session, log) = scripted("set-model-fields", &[]).await;
            session
                .set_config("model", json!("anthropic claude-opus-5"))
                .await
                .expect("a value the catalogue offers must be accepted");

            let lines = sent(&log, 1).await;
            let set = lines
                .iter()
                .find(|line| line.get("type").and_then(Value::as_str) == Some("set_model"))
                .expect("a set_model line must have been sent");
            assert_eq!(
                set.get("provider").and_then(Value::as_str),
                Some("anthropic")
            );
            assert_eq!(
                set.get("modelId").and_then(Value::as_str),
                Some("claude-opus-5")
            );
            assert!(
                set.get("model").is_none(),
                "a `model` field is the measured defect: {set}"
            );
        }

        /// **A value is provider-qualified, and the separator is a space rather than a slash.**
        ///
        /// *Measured:* 350 of 372 real model ids already contain a `/`, so splitting on one
        /// mis-parses the majority of the catalogue — and two providers offer the same id, so a
        /// bare id marks two rows current and makes one of them unreachable.
        #[tokio::test]
        async fn a_duplicated_model_id_is_reachable_under_each_provider() {
            let (session, _log) = scripted("duplicate-ids", &[]).await;
            let surface = session.capabilities().await.unwrap();
            let model = surface.config_option("model").unwrap();

            let duplicates: Vec<&str> = model
                .options
                .iter()
                .filter(|choice| choice.value.ends_with("deepseek-v4-pro"))
                .map(|choice| choice.value.as_str())
                .collect();
            assert_eq!(
                duplicates,
                vec!["deepseek deepseek-v4-pro", "openrouter deepseek-v4-pro"],
                "each provider's copy must carry its own reachable value"
            );
            // The display names are identical, so the provider is the only thing that tells
            // the two rows apart on screen.
            assert!(model
                .options
                .iter()
                .filter(|choice| choice.name == "DeepSeek V4 Pro")
                .all(|choice| choice.description.is_some()));

            // And each is separately settable, which a by-id lookup cannot do.
            for value in duplicates {
                session
                    .set_config("model", json!(value))
                    .await
                    .unwrap_or_else(|error| panic!("{value} must be settable: {error}"));
            }
        }

        /// **A vendor `success` is not a promise it did what was asked, so the answer is
        /// re-read.**
        ///
        /// *Measured at `agent-session.js:1290:* `setThinkingLevel` **clamps** an unsupported
        /// level to the nearest supported one and answers `success: true`. The scripted peer
        /// reproduces that exactly — asked for `high`, it confirms `low` — and this pins that
        /// what comes back is the vendor's *confirmation* rather than the request.
        ///
        /// The same shape as `claude` answering success for `manual` while confirming
        /// `default` (`AGENTS.md`): an adapter writing the requested value makes itself the
        /// authority on a fact only the vendor holds, and the surface then reports a policy the
        /// session is not running under.
        #[tokio::test]
        async fn a_clamped_set_answers_with_what_the_vendor_confirmed_not_what_was_asked() {
            let (session, _log) = scripted("clamped-set", &[]).await;
            let options = session
                .set_config("thinking", json!("high"))
                .await
                .expect("`high` is offered, so the set is accepted");

            let thinking = options
                .iter()
                .find(|option| option.id == "thinking")
                .expect("the refreshed array must still carry thinking");
            assert_eq!(
                thinking.current_value.as_ref().and_then(Value::as_str),
                Some("low"),
                "the vendor clamped `high` to `low`; reporting `high` would be the adapter \
                 asserting an outcome only the vendor knows"
            );
        }

        /// **`UNSUPPORTED_FIELD` and `INVALID_VALUE` are not interchangeable.**
        ///
        /// One means "no such option" and justifies disabling a control; the other means "this
        /// value, right now" and must leave it usable. Answering a malformed *value* with "'x'
        /// is not settable on a pi session" says the option does not exist when the option was
        /// fine — and the layer that is not at fault is then the one that gets debugged.
        #[tokio::test]
        async fn the_two_refusals_stay_distinct_and_travel_as_typed_codes() {
            let (session, _log) = scripted("refusal-codes", &[]).await;

            let unknown = session
                .set_config("permissionMode", json!("plan"))
                .await
                .expect_err("pi has no permission mode");
            // Recovered by **type**, never by parsing the message: the text is the adapter's
            // and may be reworded, while the code is declared protocol.
            assert_eq!(
                unknown
                    .downcast_ref::<AdapterError>()
                    .map(|error| error.code),
                Some(RefusalCode::UnsupportedField)
            );

            // A real option whose value the vendor does not currently offer. `xhigh` is one of
            // the launch flag's seven and is *not* in this model's three, which is exactly why
            // validating against `LAUNCH_THINKING_LEVELS` would be wrong.
            let bad_value = session
                .set_config("thinking", json!("xhigh"))
                .await
                .expect_err("a level this model does not offer must be refused");
            assert_eq!(
                bad_value
                    .downcast_ref::<AdapterError>()
                    .map(|error| error.code),
                Some(RefusalCode::InvalidValue),
                "the control must stay usable: its other values work"
            );

            // A value shaped wrong for the option is also the value's fault, not the option's.
            let unparsable = session
                .set_config("model", json!("claude-opus-5"))
                .await
                .expect_err("a bare id names no provider");
            assert_eq!(
                unparsable
                    .downcast_ref::<AdapterError>()
                    .map(|error| error.code),
                Some(RefusalCode::InvalidValue)
            );
        }

        /// **A refused set must not reach the vendor at all when the surface can say so.**
        ///
        /// The companion to the test above: it asserts the *code*, this asserts that nothing
        /// was sent. Forwarding a level the model does not offer would have pi clamp it, answer
        /// success, and silently change the session to a third value nobody asked for.
        #[tokio::test]
        async fn a_value_the_vendor_does_not_offer_is_never_sent() {
            let (session, log) = scripted("unoffered-not-sent", &[]).await;
            let _ = session.set_config("thinking", json!("xhigh")).await;

            // The surface read happened, so there is traffic to wait for before concluding
            // nothing else was sent.
            let lines = sent(&log, 1).await;
            assert!(
                !lines
                    .iter()
                    .any(|line| line.get("type").and_then(Value::as_str)
                        == Some("set_thinking_level")),
                "a refused value must not be forwarded: {lines:?}"
            );
        }

        /// **A reply is not an event.**
        ///
        /// A `set_model` acknowledgement in the transcript is a row nothing emitted.
        /// [`Child`] removes a correlated reply from the stream; this pins the other half —
        /// a `response` that reaches the pump *uncorrelated* is logged rather than published,
        /// because it is this adapter's bug rather than a fact about the session.
        #[tokio::test]
        async fn an_uncorrelated_reply_is_not_published_as_an_event() {
            let events = [
                r#"{"type":"response","command":"abort","success":true}"#,
                r#"{"type":"agent_start"}"#,
            ];
            let (mut session, _log) = scripted("reply-not-event", &events).await;
            let mut stream = session.messages().unwrap();

            let first = next_message(&mut stream, 2000)
                .await
                .expect("control notice");
            assert!(matches!(
                first,
                SessionMessage::Control(ControlNotice::Active { .. })
            ));
            assert!(next_message(&mut stream, 100).await.is_none());
        }

        /// **A malformed line is forwarded and does not end the stream.**
        ///
        /// One bad line must not fail a turn, and it must not be silently dropped either, or
        /// the record cannot explain a gap. pi's own store is truncate-rewritten with no lock,
        /// so a half-written payload is a real state rather than a hypothetical.
        #[tokio::test]
        async fn a_malformed_line_is_reported_and_the_stream_continues() {
            let events = ["not json at all", r#"{"type":"agent_start"}"#];
            let (mut session, _log) = scripted("malformed", &events).await;
            let mut stream = session.messages().unwrap();

            use futures::StreamExt;
            let first = tokio::time::timeout(Duration::from_millis(2000), stream.next())
                .await
                .unwrap()
                .unwrap();
            assert!(first.is_err(), "the parse failure must be reported");

            let second = next_message(&mut stream, 2000)
                .await
                .expect("the stream must continue past a bad line");
            assert!(matches!(
                second,
                SessionMessage::Control(ControlNotice::Active { .. })
            ));
        }

        /// **The stream is yielded once**, because two consumers of one vendor's stdio would
        /// each see half the turn. Fan-out is the server's job, which already does it.
        #[tokio::test]
        async fn the_event_stream_is_yielded_exactly_once() {
            let (mut session, _log) = scripted("yield-once", &[]).await;
            assert!(session.messages().is_some());
            assert!(
                session.messages().is_none(),
                "a second subscription is a bug, not a second reader"
            );
        }

        /// **An answer naming nothing pi offered becomes a dismissal, not a guess.**
        ///
        /// pi matches a `select` on the literal string it sent, so a value it never sent is not
        /// an answer it can act on. This is the session half's job because the offered list has
        /// to be *remembered* from the request — and the sibling hazard is the empty-slice one
        /// [`Dialogs`] records: an `input` offered nothing, so passing `Some(&[])` for it would
        /// turn every typed answer into a cancel.
        #[tokio::test]
        async fn an_answer_outside_the_offered_list_is_sent_as_a_dismissal() {
            // Three distinct dialogs rather than two answers to one. A resolution *retires*
            // its dialog — pi discards a second `extension_ui_response` for an id it has
            // already resolved — so re-answering one would be testing double-answer
            // semantics instead of the offered-list filter.
            let events = [
                r#"{"type":"extension_ui_request","id":"uuid-1","method":"select","title":"Pick","options":["Allow","Block"]}"#,
                r#"{"type":"extension_ui_request","id":"uuid-2","method":"select","title":"Pick again","options":["Allow","Block"]}"#,
                r#"{"type":"extension_ui_request","id":"uuid-3","method":"input","title":"Name it"}"#,
            ];
            let (mut session, log) = scripted("dialog-answers", &events).await;
            let mut stream = session.messages().unwrap();

            // Every request must be published before answering, so the offered lists are
            // recorded.
            for expected in ["uuid-1", "uuid-2", "uuid-3"] {
                let message = next_message(&mut stream, 2000)
                    .await
                    .unwrap_or_else(|| panic!("{expected} must be published"));
                let SessionMessage::Request(request) = message else {
                    panic!("expected a request, got {message:?}");
                };
                assert_eq!(request.id, expected);
            }

            // Offered → carried as `value`.
            session
                .resolve(Resolution {
                    request_id: "uuid-1".to_string(),
                    option: ui::SUBMIT.to_string(),
                    input: Some(json!({"values": {ui::CHOICE_FIELD: "Allow"}})),
                })
                .await
                .unwrap();
            // Not offered → dismissed rather than guessed at.
            session
                .resolve(Resolution {
                    request_id: "uuid-2".to_string(),
                    option: ui::SUBMIT.to_string(),
                    input: Some(json!({"values": {ui::CHOICE_FIELD: "Maybe"}})),
                })
                .await
                .unwrap();
            // An `input` offered nothing, so **any** text is a real answer.
            session
                .resolve(Resolution {
                    request_id: "uuid-3".to_string(),
                    option: ui::SUBMIT.to_string(),
                    input: Some(json!({"values": {ui::TEXT_FIELD: "anything at all"}})),
                })
                .await
                .unwrap();

            let lines = sent(&log, 3).await;
            let answers: Vec<&Value> = lines
                .iter()
                .filter(|line| {
                    line.get("type").and_then(Value::as_str) == Some("extension_ui_response")
                })
                .collect();
            assert_eq!(answers.len(), 3, "{lines:?}");
            assert_eq!(
                answers[0].get("value").and_then(Value::as_str),
                Some("Allow")
            );
            assert_eq!(
                answers[1].get("cancelled").and_then(Value::as_bool),
                Some(true),
                "an unoffered pick is a dismissal: {:?}",
                answers[1]
            );
            assert_eq!(
                answers[2].get("value").and_then(Value::as_str),
                Some("anything at all"),
                "a dialog that offered nothing must not have its answers filtered away"
            );
        }

        /// **An unknown dialog method is cancelled, never left parked.**
        ///
        /// It still blocks pi, so silence stalls the turn until the vendor's own timeout lapses
        /// — which presents as a session that stopped for no reason. The original request and
        /// dismissal stay in raw capture without creating a semantic transcript event.
        #[tokio::test]
        async fn an_unknown_dialog_method_is_dismissed_without_a_semantic_event() {
            let events = [
                r#"{"type":"extension_ui_request","id":"uuid-9","method":"holograph","title":"?"}"#,
            ];
            let (mut session, log) = scripted("unknown-dialog", &events).await;
            let mut stream = session.messages().unwrap();

            assert!(next_message(&mut stream, 100).await.is_none());

            let lines = sent(&log, 1).await;
            assert_eq!(
                lines[0].get("cancelled").and_then(Value::as_bool),
                Some(true),
                "pi must be released rather than left blocking: {lines:?}"
            );
            assert_eq!(lines[0].get("id").and_then(Value::as_str), Some("uuid-9"));
        }

        /// **`interrupt` does not await pi's reply, and synthesizes no close.**
        ///
        /// *Measured at `agent-session.js:1171`:* `abort()` awaits `waitForIdle()`, so its
        /// response arrives after the turn settles. The scripted peer answers `abort` with
        /// **nothing at all**, which is the worst case of that: a `request` here would block
        /// until [`process::REPLY_TIMEOUT`] and then report a failure for an abort that worked.
        ///
        /// The close is the vendor's to report — `_emitAgentSettled` sits in the `finally` of
        /// `_runAgentPrompt`, so an aborted turn does emit one — and `aborted` is a user
        /// stopping a turn rather than a failure, so writing one here would also colour an
        /// interrupted turn as a broken one.
        #[tokio::test]
        async fn interrupt_returns_without_a_reply_and_publishes_nothing_itself() {
            let (mut session, log) = scripted("interrupt", &[]).await;
            let mut stream = session.messages().unwrap();

            tokio::time::timeout(Duration::from_secs(2), session.interrupt())
                .await
                .expect("interrupt must not wait on a reply pi sends after settling")
                .expect("the abort must be accepted");

            let lines = sent(&log, 1).await;
            assert_eq!(lines[0].get("type").and_then(Value::as_str), Some("abort"));
            assert!(
                next_message(&mut stream, 300).await.is_none(),
                "the close is the vendor's to report; a synthesized one is a second record"
            );
        }

        /// **`send` returns on acceptance, not on completion.**
        ///
        /// *Measured at `rpc-mode.js:298`:* `prompt` answers as soon as its preflight succeeds,
        /// and a failure after acceptance is reported through the event stream rather than as a
        /// second response. The peer emits no events at all here, so a `send` that waited for a
        /// turn would hang — which is what makes this observable.
        #[tokio::test]
        async fn send_returns_once_the_turn_is_accepted() {
            let (session, log) = scripted("send-accept", &[]).await;
            tokio::time::timeout(
                Duration::from_secs(2),
                session.send(TurnInput {
                    text: "hello".to_string(),
                    ..Default::default()
                }),
            )
            .await
            .expect("send must not await the turn")
            .expect("the prompt must be accepted");

            let lines = sent(&log, 1).await;
            assert_eq!(lines[0].get("type").and_then(Value::as_str), Some("prompt"));
            assert_eq!(
                lines[0].get("message").and_then(Value::as_str),
                Some("hello")
            );
        }

        #[tokio::test]
        async fn native_queue_priority_is_available_without_a_turn_scoped_promise() {
            let (session, log) = scripted("steer-queue", &[]).await;
            session
                .steer("guarded", Some("turn-7"))
                .await
                .expect_err("Pi cannot bind a steer to a turn");
            session
                .steer("unguarded", None)
                .await
                .expect_err("absence of a lease cannot change delivery semantics");
            session.queue_steering("priority").await.unwrap();
            let lines = sent(&log, 1).await;
            assert_eq!(lines[0]["type"], "steer");
            assert_eq!(lines[0]["message"], "priority");
        }

        /// **The vendor reporting a different model publishes a `config`-only facet.**
        ///
        /// pi's own TUI can steer a session and a `/model` typed as prose changes it too, so a
        /// client that only ever learns the model from its own `set_config` reply shows a stale
        /// one indefinitely — announcing arrival without announcing departure, applied to a
        /// value. The update names **only** `config`: an unmentioned facet must not be
        /// clearable, which is the 46-erased-commands defect `CapabilityFacets` exists for.
        #[tokio::test]
        async fn a_model_the_vendor_reports_publishes_a_config_only_facet() {
            let events = [
                r#"{"type":"message_start","message":{"role":"assistant","model":"claude-opus-5","provider":"anthropic"}}"#,
            ];
            let (mut session, _log) = scripted("model-observed", &events).await;
            let mut stream = session.messages().unwrap();

            let mut facets = None;
            for _ in 0..10 {
                match next_message(&mut stream, 2000).await {
                    Some(SessionMessage::Capabilities(update)) => {
                        facets = Some(update);
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
            let facets = facets.expect("a model change must be published");
            assert!(facets.config.is_some(), "the config facet is what changed");
            assert!(
                facets.commands.is_none()
                    && facets.subagents.is_none()
                    && facets.permission_mode.is_none()
                    && facets.context_window.is_none(),
                "an unmentioned facet must not be clearable: {facets:?}"
            );
            let config = facets.config.unwrap();
            let model = config
                .iter()
                .find(|option| option.id == "model")
                .expect("the refreshed array carries the model");
            assert_eq!(
                model.current_value.as_ref().and_then(Value::as_str),
                Some("anthropic claude-opus-5"),
                "and its value is read back from the vendor, not taken from the event"
            );
        }

        /// **Which base64 variant an image is encoded with**, which is this adapter's choice
        /// rather than the crate's behaviour.
        ///
        /// Deliberately not a re-test of RFC 4648 conformance — `base64 0.22` has its own vectors
        /// for that, and asserting them here would only pin that the dependency still works.
        /// What is ours to get wrong is the *engine*: `STANDARD_NO_PAD` and `URL_SAFE` are one
        /// token away in the same namespace, both compile, and both produce a string pi's own
        /// decoder rejects — a silently empty image rather than an error.
        ///
        /// So the two assertions are exactly the two things a variant changes: padding, and the
        /// `+`/`/` pair. `0xfb 0xff` is chosen because it is the shortest input that reaches both
        /// high alphabet indices, where `URL_SAFE` would answer `-_8=`.
        #[test]
        fn an_image_is_encoded_as_padded_standard_base64_and_not_a_url_safe_variant() {
            assert_eq!(
                STANDARD.encode([0xfb, 0xff]),
                "+/8=",
                "the standard alphabet and its padding are what pi's decoder expects"
            );
            // A one-byte tail, the other padding length, so a `NO_PAD` engine cannot pass on the
            // case above alone.
            assert_eq!(STANDARD.encode(b"f"), "Zg==");
        }

        /// **An image is inlined and every other file is named by path.**
        ///
        /// A path to an image is something the model cannot see, and an inlined text file bloats
        /// the message with what pi's own `read` tool would fetch on demand. The type is sniffed
        /// from **magic bytes** rather than the extension, so this fixture deliberately gives a
        /// PNG the wrong suffix — a mislabelled image sent as an image block is a provider error
        /// a user cannot act on.
        #[tokio::test]
        async fn an_image_is_inlined_by_magic_bytes_and_other_files_are_named() {
            let dir = scratch("attachments");
            let png = dir.join("screenshot.txt");
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(b"not really a png body");
            std::fs::write(&png, &bytes).unwrap();
            let notes = dir.join("notes.md");
            std::fs::write(&notes, "# notes").unwrap();

            let (message, images) = build_message(
                "look at these",
                &[
                    png.to_string_lossy().to_string(),
                    notes.to_string_lossy().to_string(),
                ],
            );

            assert_eq!(images.len(), 1, "only the image is inlined");
            assert_eq!(
                images[0].get("mimeType").and_then(Value::as_str),
                Some("image/png"),
                "sniffed from magic bytes despite the .txt suffix"
            );
            assert_eq!(
                images[0].get("data").and_then(Value::as_str),
                Some(STANDARD.encode(&bytes).as_str())
            );
            assert!(
                message.contains("notes.md") && !message.contains("# notes"),
                "a text file is named, not inlined: {message}"
            );
            assert!(
                !message.contains("screenshot.txt"),
                "an inlined image must not also be named as a file: {message}"
            );
            assert!(message.ends_with("look at these"), "{message}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// **A vendor-initiated request whose id collides with an in-flight one must reach the
        /// stream, not a waiter.**
        ///
        /// pi multiplexes both directions onto one stdout and `extension_ui_request` carries an
        /// `id` of its own, so a predicate reading only `id` routes one into whichever waiter
        /// holds that number. Both directions then fail silently: the request never reaches a
        /// client, so nobody is asked and the turn waits on a human forever, while the caller
        /// that sent `get_state` receives a UI request as its answer.
        ///
        /// The peer reproduces it exactly — the dialog carries the id of the command it is
        /// answering — and both halves are asserted, because either alone passes on a broken
        /// predicate: the caller must get its real reply, **and** the dialog must be published.
        #[tokio::test]
        async fn a_vendor_request_sharing_an_in_flight_id_reaches_the_stream_not_the_waiter() {
            let (mut session, _log) = scripted_colliding("id-collision").await;
            let mut stream = session.messages().expect("the stream is yielded once");

            // The caller's own reply, which a broken predicate hands to the dialog instead.
            let surface = session
                .capabilities()
                .await
                .expect("get_state's reply must reach the caller that sent it");
            assert_eq!(
                surface
                    .config_option("model")
                    .and_then(|option| option.current_value.as_ref())
                    .and_then(Value::as_str),
                Some("anthropic claude-opus-5")
            );

            // And the dialog, which a broken predicate swallows into that same waiter.
            let mut published = None;
            for _ in 0..10 {
                match next_message(&mut stream, 2000).await {
                    Some(SessionMessage::Request(request)) => {
                        published = Some(request);
                        break;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
            let published = published.expect(
                "the dialog must be published; swallowed, pi waits on a human nobody asked",
            );
            assert_eq!(published.kind, sylph_protocol::RequestKind::Choice);
            assert_eq!(published.title, "Pick");
        }
        /// **Opening without a dictated id is refused rather than answered with an invented
        /// one.**
        ///
        /// `dictates_native_id` is true for this agent, so the envagent names an id. The
        /// v1 adapter minted one with `uuid4()`, which is the
        /// synthesized-identity failure [`NativeId`] forbids: the id would look like the
        /// vendor's and be Sylph's, and every comparison against the vendor's own store would
        /// then be silently always false.
        ///
        /// Refused **before the spawn**, which is what makes this assertable without pi
        /// installed — and is also the right order: a process started under an identity nobody
        /// can resume is worse than one that never started.
        #[tokio::test]
        async fn opening_without_a_dictated_id_is_refused_before_anything_is_spawned() {
            let spec = RunSpec {
                workdir: "/tmp".to_string(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: Default::default(),
                permission_mode: None,
            };
            let error = PiSession::open(spec, crate::process::test_capture())
                .await
                .err()
                .expect("pi dictates its id; opening without one must fail");
            assert!(
                error.to_string().contains("Dictate") || error.to_string().contains("Resume"),
                "the error must say what to open it with: {error}"
            );
        }

        /// **The preamble placeholder is substituted with a file that exists.**
        ///
        /// `--append-system-prompt` reads its argument as a *file* when the string names one and
        /// as literal text otherwise (`core/resource-loader.js:391`, `resolvePromptInput`), so a
        /// one-line preamble that happened to name a real path would be silently replaced by
        /// that file's contents. A file makes it unambiguous.
        ///
        /// Both halves are asserted, because either alone passes on a broken implementation:
        /// the marker must be **gone** — otherwise the vendor is handed
        /// `<sylph-identity-preamble>` as a system prompt, which is exactly what the marker
        /// exists to make visible — and the path that replaced it must hold the text.
        #[test]
        fn the_preamble_placeholder_becomes_a_readable_file() {
            let path = write_preamble("# Sylph\nYou are running inside Sylph.").unwrap();
            let argv = vec![
                "pi".to_string(),
                "--mode".to_string(),
                "rpc".to_string(),
                "--append-system-prompt".to_string(),
                crate::pi::IDENTITY_PREAMBLE_PLACEHOLDER.to_string(),
            ];
            let args = spawn_args(argv, Some(&path));

            assert_eq!(args.first().map(String::as_str), Some("--mode"));
            assert!(
                !args
                    .iter()
                    .any(|part| part == crate::pi::IDENTITY_PREAMBLE_PLACEHOLDER),
                "an unsubstituted marker reaches the vendor as prompt text: {args:?}"
            );
            let written = args.last().expect("the flag's value");
            assert_eq!(written, &path.to_string_lossy().to_string());
            assert_eq!(
                std::fs::read_to_string(written).unwrap(),
                "# Sylph\nYou are running inside Sylph."
            );

            // Nothing is substituted when the server asked for no identity, and the marker is
            // then absent from `argv_for`'s output rather than left for this function to strip.
            let plain = spawn_args(vec!["pi".to_string(), "--mode".to_string()], None);
            assert_eq!(plain, vec!["--mode".to_string()]);
            let _ = std::fs::remove_file(&path);
        }
    }
}
