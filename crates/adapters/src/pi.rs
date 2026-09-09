//! The `pi` adapter: the live RPC surface, and the reader for its native store.
//!
//! `docs/agents.md` § `pi` is the governing spec and holds the measurement behind every
//! quirk below. The vendor's own protocol is documented in the installed package at
//! `~/.local/lib/node_modules/@earendil-works/pi-coding-agent/`. Every pointer below names
//! a file in it as `pi-coding-agent/docs/…`, package-relative, so a vendor's document is
//! never mistakable for one of this repo's own. **That unbundled source-mapped install is
//! the evidence to prefer**: `AGENTS.md` records an earlier claim about this vendor that
//! was wrong precisely because it was measured against a minified bundle.
//!
//! # One file, five parts, and only one of them touches a process
//!
//! [`rpc`] holds the command and config surface: what a value means, what `set_model`
//! takes, which refusal a bad one earns. [`ui`] projects pi's one inversion, an extension
//! asking a human something. [`translate`] holds the vendor→Sylph event projection as pure
//! functions over a [`translate::Correlation`], the same split `claude`'s module makes and
//! for the same measured reason: an end-to-end check spends real tokens and only sees
//! whatever shapes one live turn happens to produce, so a mistranslation does not raise —
//! it produces a plausible timeline that misdescribes what the agent did. [`native`] reads
//! `~/.pi/agent/sessions/`.
//!
//! [`session`] is the one part that holds stdio, and it is deliberately thin: it wires the
//! other four to a [`crate::process::Child`] and owns only what genuinely needs a process —
//! reply correlation against outbound traffic, and the answer path for a dialog. Every
//! decision it makes about a *payload* is delegated, which is what keeps the expensive
//! parts assertable without spending a token.
//!
//! # What this adapter does **not** emit, and why that is a report rather than a gap
//!
//! No `approval`, and no `subagent`. `pi` ships no tool-permission callback — a tool runs
//! when the model calls it — and it reports no delegation on the RPC wire in any form.
//! Emitting either would fabricate structure the vendor never reports; a flat `pi`
//! transcript is a true report. `Task`-shaped tools are absent from its catalogue
//! entirely (`read`, `write`, `edit`, `bash`, `find`, `grep`, `ls`, `tree`), so there is
//! nothing to mark as delegating either — see [`translate::SPAN_HAS_NO_VENDOR_SOURCE`].
//!
//! # `describe` is deleted
//!
//! The v1 `--serve` entry point and its NDJSON framing are gone with the
//! subprocess transport, not ported. There is no v1 or v3 contract left to serve, and
//! `AGENTS.md` § Compatibility forbids keeping one.

// The three halves are **inline modules rather than files**, unlike `claude`'s directory.
// One vendor's whole surface is one unit of review here — the config encoding, the event
// projection, and the store parser each depend on the same measured field spellings — and
// splitting them into files would let a reader change the encoding in one and the parser in
// another without seeing both. They are named modules rather than one flat namespace so
// `translate`'s pure functions stay separable from anything that touches a process.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use sylph_protocol::{
    Adapter, AgentInfo, Capability, EventStream, Facet, FlagSpec, LinkFacets, NativeId,
    NativeSummary, RunSpec, Session, StoreFilter, UnavailableReason,
};

pub use rpc::{model_key, split_model_key, ConfigId, MODEL_KEY_SEPARATOR};
pub use session::PiSession;
pub use translate::{Correlation, Emission};

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
            // Unreachable for this vendor: `dictates_native_id` is true, so the server
            // always names an id. Handled rather than panicking because a protocol value
            // an adapter cannot honour is a refusal, not a crash — and **this adapter must
            // not mint one itself**. The v1 adapter did (
            // `uuid.uuid4()`), which is the synthesized-identity failure
            // `sylph_protocol::NativeId` forbids: the id would look like the vendor's and
            // be Sylph's.
            sylph_protocol::RequestedNativeId::New => {
                // **Says what is true of the argv, and no more.** An earlier wording claimed
                // this made the session "unresumable", which overstates its own reason: pi
                // reports a `sessionId` from `get_state`, so an adapter *could* read one back
                // — `codex` is built that way. What is actually the case is narrower and is
                // the whole justification for `session::PiSession::open` refusing this
                // variant: this vendor takes `--session-id`, `dictates_native_id` is true, so
                // the server always names one and `New` is unreachable here rather than
                // impossible in principle. A doc that overstates its reason is how the next
                // reader concludes something stronger than what was measured.
                warnings.push(
                    "pi takes --session-id and the server always names one, so opening \
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
            // Each gates exactly one method on `Session`. `interrupt` is pi's `abort`,
            // steering is pi's own `steer` — the discipline that delivers after the
            // current tool calls and before the next model call, which is what "change
            // course now" means — and `set_config` maps onto `set_model` and
            // `set_thinking_level`.
            //
            // **This vendor offers both `Steer` and `Queue`, which is what makes them two
            // capabilities rather than one.** `steer` reaches the running turn and
            // `follow_up` waits for it, and pi implements them as separate verbs with
            // separate delivery disciplines — so a client offering only one of them is
            // withholding something the session can do.
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
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

    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
        let session = session::PiSession::open(spec).await?;
        Ok(Box::new(session))
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
        // that the server always names an id for this agent, so the case is unreachable
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
    /// `now_ms` is a parameter rather than a call to a clock inside, for the same reason
    /// `translate` takes one: a deadline computed against a global clock is not assertable,
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

/// `pi`'s RPC events, projected onto [`sylph_protocol`] events.
///
/// The vendor→Sylph projection: `_translate`, `_delta` and `flatten`
/// (:458-563). `docs/session-protocol.md` § Events governs.
///
/// # Why this is a separate, process-free module
///
/// The same reason `claude::translate` is: **a mistranslation does not raise.** It produces
/// a plausible timeline that misdescribes what the agent did, and an end-to-end check
/// cannot see one — it spends real tokens and only exercises whatever shapes one live turn
/// happens to produce. So the whole projection is pure functions over one [`Correlation`],
/// with no process, no I/O, and no clock of its own.
///
/// # Time arrives as an argument
///
/// [`sylph_protocol::Event::created_at_ms`] is required, and **pi's RPC events carry no
/// timestamp at all** — checked against `pi-coding-agent/docs/rpc.md` § Events, where every event shape is
/// listed and none holds one. (Its *messages* do, which is what the native store reads; a
/// live `message_update` carries `usage` and `assistantMessageEvent` and nothing else.) So
/// the adapter stamps arrival, and the clock is a parameter: a test that cannot fix the
/// stamps cannot assert on ordering.
pub mod translate {
    use std::collections::HashMap;

    use serde_json::{json, Value};
    use sylph_protocol::{Event, EventKind, Phase, SessionMessage};

    /// Why no event this module emits ever carries a `span`.
    ///
    /// **`pi` reports no delegation on the RPC wire in any form, and that is measured
    /// rather than assumed.** Its event table is 21 kinds with no parent, span, or child
    /// field — `toolCallId` is the only correlation any event carries — and its tool
    /// catalogue (`read`, `write`, `edit`, `bash`, `find`, `grep`, `ls`, `tree`) holds no
    /// delegating tool. Sub-agents exist only for SDK embedders, who build a second
    /// in-process `AgentSession` inside a custom tool; that inner session's events go to
    /// its own `subscribe` callback and never reach this process.
    ///
    /// The one trace such work leaves on the wire is `ToolResultMessage.usage`, documented
    /// as the tool's combined usage for nested LLM calls — an aggregate token count with no
    /// identity, no lifecycle, and no count of delegates, so it cannot name a trajectory
    /// even in principle.
    ///
    /// So a flat `pi` transcript is a **true report** rather than a gap. Synthesizing a
    /// `span` out of a token total, or out of a parser-invented `call:<call_id>` pairing —
    /// which is a tool-call/result link rather than delegation structure — would put a shape
    /// in the timeline that nothing in the session corresponds to.
    pub const SPAN_HAS_NO_VENDOR_SOURCE: &str =
        "pi exposes no delegation on the RPC wire; a span here would be invented";

    /// What one vendor event turned into.
    ///
    /// Mirrors `claude::Emission` field for field where the facts are the same, so a reader
    /// moving between the two modules is reading one shape. `refresh_capabilities` is
    /// absent: **pi has no "your commands changed" event** — `get_commands` is polled, not
    /// pushed — so a flag for it would never be set.
    #[derive(Debug, Default, Clone, PartialEq)]
    pub struct Emission {
        /// Messages to publish, in the order they happened.
        pub messages: Vec<SessionMessage>,
        /// The model the vendor reported it is now running, when a payload said so.
        ///
        /// Reported rather than stored, because a config surface must be built from what the
        /// **vendor** last said and never from what the adapter last set. The model can also
        /// change without anyone asking — pi's own TUI, or a `/model` typed as prose.
        pub observed_model: Option<String>,
        /// The turn just settled, so a caller may stop waiting.
        ///
        /// Named rather than left for a caller to infer from the last event, because the
        /// event that settles a turn is **`agent_settled` and not `agent_end`**, and a caller
        /// reading the wrong one settles a turn while the agent is still working.
        pub settled: bool,
    }

    impl Emission {
        fn push(&mut self, event: Event) {
            self.messages.push(SessionMessage::Event(event));
        }

        /// Events only, for a test that wants to assert on the projection.
        pub fn events(&self) -> Vec<&Event> {
            self.messages
                .iter()
                .filter_map(|message| match message {
                    SessionMessage::Event(event) => Some(event),
                    _ => None,
                })
                .collect()
        }
    }

    /// One in-flight content block, keyed by the vendor's `contentIndex`.
    #[derive(Debug, Clone)]
    struct OpenBlock {
        kind: EventKind,
        stream_id: String,
        /// The deltas seen so far.
        ///
        /// Accumulated because **pi never re-sends the cumulative text**: `pi-coding-agent/docs/rpc.md` is
        /// explicit that `message_update` omits the former cumulative `message` field and a
        /// client needing a partial must assemble it from `contentIndex`. So this is the
        /// fallback for an `_end` that carries no `content` of its own.
        text: String,
    }

    /// What a live `pi` session needs to remember between events.
    ///
    /// Every field is here for one of the only two reasons an event translator needs memory:
    /// a payload arrives *after* the payload that needed it, or *without* a field an earlier
    /// one carried.
    #[derive(Debug, Default)]
    pub struct Correlation {
        /// `contentIndex` → the in-flight block.
        ///
        /// **Keyed by the vendor's index alone, and that is sound *here* only because pi
        /// multiplexes nothing onto it.** `claude` numbers content blocks per message and
        /// forwards two concurrent subagents' blocks on one stream, so an index-keyed map
        /// made each delegate's deltas append to the other's text — data corruption
        /// (`docs/session-protocol.md` § Keying rules). pi has no delegate stream at all
        /// ([`SPAN_HAS_NO_VENDOR_SOURCE`]), and its indices are scoped to the message that
        /// `message_start` cleared. If pi ever forwards a second stream, this key is the
        /// first thing that must grow.
        blocks: HashMap<u64, OpenBlock>,
        /// How many messages this session has seen, for a `stream_id` that stays unique
        /// across them.
        ///
        /// The index alone is **not** unique: `message_start` resets the block map, so
        /// message 2's index 0 is a different block from message 1's index 0. A `stream_id`
        /// that collided across messages would make the history fold consolidate two
        /// unrelated blocks into one row.
        message_seq: u64,
        message_id: String,
        /// Whether a turn is open, so `agent_start` does not open a second one.
        ///
        /// `agent_start` is emitted per low-level agent run and pi can start another within
        /// one settled turn — a retry, a compaction, a queued continuation — so a second
        /// `turn`/`started` would bracket a turn that is already bracketed.
        turn_open: bool,
        /// The outcome the vendor last reported, held until `agent_settled` closes the turn.
        ///
        /// **`agent_settled` carries no payload** — its declared shape is `{type}` alone
        /// (`dist/core/agent-session.d.ts`) — while the outcome sits on the `AssistantMessage`
        /// that `message_end` delivers, as `stopReason` plus an optional `errorMessage`. So
        /// the settling event and the outcome arrive on different payloads, which is the
        /// second of the only two reasons this struct has memory at all.
        ///
        /// `None` means the vendor stated no outcome, and that is published as no stop
        /// reason rather than as a successful one.
        outcome: Option<Outcome>,
    }

    /// What the vendor said about how a response ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Outcome {
        /// One of pi's seven `StopReason` values, verbatim.
        pub stop_reason: String,
        /// The provider's own sentence, when the response carried one.
        pub error_message: Option<String>,
    }

    impl Outcome {
        /// Read an outcome off an `AssistantMessage`, or `None` when it states none.
        ///
        /// Both spellings of both fields are read through `native::defect_field_spelling`'s
        /// tables, so the live path and the store path resolve one field the same way.
        pub fn read(message: &Value) -> Option<Self> {
            let stop_reason = super::native::STOP_REASON
                .read(message)
                .and_then(|(value, _)| value.as_str())?
                .to_string();
            Some(Self {
                stop_reason,
                error_message: super::native::ERROR_MESSAGE
                    .read(message)
                    .and_then(|(value, _)| value.as_str())
                    .map(str::to_string),
            })
        }

        /// Whether this outcome is a failure.
        ///
        /// **Only `error` is.** `aborted` is a user stopping a turn, and colouring that red
        /// reports an interrupted turn as a broken one (`docs/session-protocol.md`
        /// § Outcomes). `pending` and `deferred` are neither, and the remaining three —
        /// `stop`, `length`, `toolUse` — are ordinary ends.
        pub fn is_error(&self) -> bool {
            self.stop_reason == "error"
        }
    }

    impl Correlation {
        pub fn new() -> Self {
            Self {
                // Matches the Python's initial `self.message_id`, so a content block arriving
                // before any `message_start` still synthesizes a stable `stream_id` rather
                // than one holding an empty segment.
                message_id: "pi".to_string(),
                ..Default::default()
            }
        }

        /// Whether a turn is currently open.
        ///
        /// Exposed because the session half needs it and a second copy of the flag there
        /// would be a second answer to one question.
        pub fn turn_is_open(&self) -> bool {
            self.turn_open
        }

        /// One vendor event, projected.
        ///
        /// An unrecognized `type` produces an empty [`Emission`] rather than an error: pi
        /// emits 21 kinds and adds more between releases, and one unknown line must not fail
        /// a turn.
        ///
        /// # What is deliberately not here
        ///
        /// `response` and `extension_ui_request` belong to the session half. A response has
        /// to wake the waiter that sent the request, and a UI request has to become a
        /// [`sylph_protocol::Request`] whose answer routes back over the same stdin — both
        /// are correlation against *outbound* traffic this module never sees. Splitting them
        /// out is also what keeps the projection testable without a process.
        pub fn translate(&mut self, payload: &Value, at_ms: i64) -> Emission {
            let mut out = Emission::default();
            match payload.get("type").and_then(Value::as_str) {
                Some("agent_start") => {
                    if !self.turn_open {
                        self.turn_open = true;
                        out.push(Event::new(EventKind::Turn, Some(Phase::Started), at_ms));
                    }
                }
                Some("message_start") => {
                    // A new message means a new index space, so the old blocks are gone.
                    // Any block still open here was never closed by the vendor, which for
                    // this event means the message ended without an `_end` — dropping it is
                    // right, because its text is already on the wire as deltas.
                    self.message_seq += 1;
                    self.message_id = format!("pi-{}", self.message_seq);
                    self.blocks.clear();
                    // The model the vendor says it is running, when the message names one.
                    // Read here rather than tracked, because it is the vendor's claim.
                    out.observed_model = observed_model(payload.get("message"));
                }
                Some("message_update") => self.delta(payload, at_ms, &mut out),
                Some("tool_execution_start") => {
                    // **Emitted already `completed`, because pi sends whole arguments.**
                    // There is no streaming of a tool's input on this vendor:
                    // `tool_execution_start` carries the full `args` object. A `started`
                    // phase would be an ephemeral projection the journal never keeps
                    // (`sylph_protocol::Event::derive_stability`), so the call itself would
                    // vanish on reload.
                    //
                    // Note what `completed` means on a `tool_call`: its *arguments* finished
                    // streaming, never that the tool finished. The outcome is the
                    // `tool_result` below.
                    let mut event = Event::new(EventKind::ToolCall, Some(Phase::Completed), at_ms);
                    event.stream_id = tool_call_id(payload);
                    event.name = string_field(payload, "toolName");
                    let args = payload.get("args").cloned().unwrap_or(json!({}));
                    event.text = compact(&args);
                    event.data = Some(json!({"input": args}));
                    out.push(event);
                }
                Some("tool_execution_end") => {
                    let mut event =
                        Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
                    event.stream_id = tool_call_id(payload);
                    event.name = string_field(payload, "toolName");
                    event.text = flatten(
                        payload
                            .get("result")
                            .and_then(|result| result.get("content"))
                            .unwrap_or(&Value::Null),
                    );
                    // `isError` is the vendor's own flag and is what escalates a failure
                    // upward. Absent is not an error — a missing key would otherwise report
                    // every result as a failure the moment the vendor renamed it.
                    event.is_error = payload
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    out.push(event);
                }
                // `compaction_start` is deliberately silent: a compaction that has begun and
                // not finished has no boundary to draw yet, and an event per half would put
                // two rows in the timeline where one thing happened.
                Some("compaction_end") => {
                    let mut event = Event::new(EventKind::Compact, Some(Phase::Completed), at_ms);
                    event.text = "Context was compacted".to_string();
                    // The vendor's reason — `manual`, `threshold`, or `overflow` — is worth
                    // keeping: "you ran out of room" and "you asked for this" read very
                    // differently at the same point in a transcript.
                    if let Some(reason) = string_field(payload, "reason") {
                        event.data = Some(json!({"reason": reason}));
                    }
                    out.push(event);
                }
                Some("agent_settled") => {
                    // **`agent_settled`, not `agent_end`.** Both events genuinely exist and
                    // both reach the wire (`agent_start` at
                    // `pi-agent-core/dist/agent-loop.js:49,67`, `agent_end` at
                    // `:110,157,172`, funnelled through `dist/modes/rpc/rpc-mode.js:265-266`
                    // — verified against the unbundled install, and `AGENTS.md` records an
                    // earlier claim that they had zero emit sites as **wrong**).
                    //
                    // The distinction is the vendor's own: `agent_end` is one low-level run
                    // completing and may be followed by a retry, a compaction, or a queued
                    // continuation, while `agent_settled` is emitted once nothing remains.
                    // Closing on `agent_end` settles a turn while the agent is still
                    // working, and the remaining output then arrives with no turn to belong
                    // to.
                    self.turn_open = false;
                    self.blocks.clear();
                    out.settled = true;
                    out.push(self.close_turn(at_ms));
                }
                Some("extension_error") => {
                    let mut event = Event::new(EventKind::Status, Some(Phase::Completed), at_ms);
                    event.text = string_field(payload, "error")
                        .unwrap_or_else(|| "extension error".to_string());
                    event.is_error = true;
                    out.push(event);
                }
                Some("message_end") => {
                    // **Publishes nothing, and remembers the one thing only it carries.** Its
                    // blocks are the `_end` deltas already reported, so emitting them again
                    // would duplicate every block — but its `message` is the authoritative
                    // `AssistantMessage`, and `stopReason` and `errorMessage` appear nowhere
                    // else on the wire. `agent_settled` then closes the turn with it.
                    if let Some(outcome) = payload.get("message").and_then(Outcome::read) {
                        out.observed_model = observed_model(payload.get("message"));
                        self.outcome = Some(outcome);
                    }
                }
                // Every other kind pi emits. `turn_start`/`turn_end` are deliberately here
                // rather than bracketing anything: pi's "turn" is one assistant response
                // plus its tool calls, so a multi-tool exchange emits several of them
                // inside one Sylph turn — treating them as brackets would open and close a
                // turn per model call. `queue_update`, `bash_execution_update`, the retry
                // family, and `tool_execution_update` carry progress a Sylph event has no
                // shape for.
                _ => {}
            }
            out
        }

        /// The `turn`/`completed` that closes a settled turn.
        ///
        /// **Reports the vendor's outcome, and states none when the vendor stated none.** A
        /// hardcoded `stopReason` here asserted a clean finish over every failure: pi answering
        /// `404` on every request produced a turn that opened, closed successfully, and emitted
        /// no assistant block — a session that looks like it worked and simply said nothing,
        /// which is the hardest outcome to debug because the record cannot distinguish it from
        /// a model that chose not to answer or an adapter that dropped the stream.
        ///
        /// So the reason is read from what the vendor reported, `errorMessage` becomes the
        /// event's text where the failure is legible, and `is_error` follows the reason rather
        /// than a literal. An absent outcome publishes no `data` at all: a turn must still
        /// close, and a synthesized reason would be a lie the journal makes permanent.
        fn close_turn(&mut self, at_ms: i64) -> Event {
            let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), at_ms);
            let Some(outcome) = self.outcome.take() else {
                return event;
            };
            event.is_error = outcome.is_error();
            let mut detail = json!({"stopReason": outcome.stop_reason});
            if let Some(text) = outcome.error_message {
                if let Some(object) = detail.as_object_mut() {
                    object.insert("errorMessage".to_string(), Value::String(text.clone()));
                }
                event.text = text;
            }
            event.data = Some(detail);
            event
        }

        /// One streaming delta, projected onto Sylph's event kinds.
        ///
        /// pi's delta types are `{text,thinking,toolcall}_{start,delta,end}`, so the kind
        /// comes from the prefix and the phase from the suffix. A type this build does not
        /// know is dropped rather than guessed at — an unknown *kind* has no event to become.
        fn delta(&mut self, payload: &Value, at_ms: i64, out: &mut Emission) {
            let Some(delta) = payload.get("assistantMessageEvent") else {
                return;
            };
            let Some(delta_type) = delta.get("type").and_then(Value::as_str) else {
                return;
            };
            let index = delta
                .get("contentIndex")
                .and_then(Value::as_u64)
                .unwrap_or(0);

            // `rsplit_once` rather than a prefix test, because `toolcall_start` and
            // `text_start` share a suffix and nothing else.
            let Some((prefix, suffix)) = delta_type.rsplit_once('_') else {
                return;
            };
            let kind = match prefix {
                "text" => EventKind::Assistant,
                "thinking" => EventKind::Thinking,
                "toolcall" => EventKind::ToolCall,
                _ => return,
            };

            // Unique across messages, not merely across indices — see
            // [`Correlation::message_seq`].
            let stream_id = format!("{}:{}", self.message_id, index);

            match suffix {
                "start" => {
                    self.blocks.insert(
                        index,
                        OpenBlock {
                            kind: kind.clone(),
                            stream_id: stream_id.clone(),
                            text: String::new(),
                        },
                    );
                    let mut event = Event::new(kind, Some(Phase::Started), at_ms);
                    event.stream_id = Some(stream_id);
                    // `toolcall_start` carries `id` and `toolName` per `pi-coding-agent/docs/rpc.md`, and the
                    // name is worth publishing at open: a tool call whose name arrives only
                    // at `_end` renders as an unnamed spinner for however long its arguments
                    // take to stream.
                    event.name = string_field(delta, "toolName");
                    out.push(event);
                }
                "delta" => {
                    let text = delta
                        .get("delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if text.is_empty() {
                        return;
                    }
                    if let Some(block) = self.blocks.get_mut(&index) {
                        block.text.push_str(text);
                    }
                    let mut event = Event::new(kind, Some(Phase::Delta), at_ms);
                    event.stream_id = Some(stream_id);
                    event.text = text.to_string();
                    out.push(event);
                }
                "end" => {
                    let block = self.blocks.remove(&index);
                    // **`content` on the end event is authoritative where pi sends it**; the
                    // accumulated deltas are the fallback. `pi-coding-agent/docs/rpc.md` says to treat
                    // `message_end.message` as authoritative for the same reason: a provider
                    // may correct or complete a block the deltas only approximated.
                    let text = delta
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| block.as_ref().map(|block| block.text.clone()))
                        .unwrap_or_default();
                    // A block whose kind at `_end` disagrees with the one recorded at
                    // `_start` is the vendor contradicting itself. The opening kind wins,
                    // because that is what the `started` and `delta` events already carried
                    // — a `completed` under a different kind would consolidate into a
                    // different block than the phases it closes.
                    let kind = block
                        .as_ref()
                        .map(|block| block.kind.clone())
                        .unwrap_or(kind);
                    let stream_id = block.map(|block| block.stream_id).unwrap_or(stream_id);
                    let mut event = Event::new(kind.clone(), Some(Phase::Completed), at_ms);
                    event.stream_id = Some(stream_id);
                    event.text = text;
                    if kind == EventKind::ToolCall {
                        // `toolcall_end.toolCall` holds the completed call, so the arguments
                        // and the name are both authoritative here.
                        if let Some(call) = delta.get("toolCall") {
                            event.name = string_field(call, "name");
                            event.data = Some(json!({"input": call.get("arguments")}));
                        }
                    }
                    out.push(event);
                }
                _ => {}
            }
        }
    }

    /// The `toolCallId` a tool event correlates on, or `None`.
    ///
    /// **Absent rather than a placeholder.** The Python original substituted the literal
    /// `"pi-tool"` in v1, which for two concurrent tool calls would make
    /// both events share one `stream_id` and fold into one row — and for one call would pair
    /// a result to a call it does not belong to. `AGENTS.md`: a guess written into a journal
    /// is permanent, and `Option`/`null` is the only value that stays honest.
    fn tool_call_id(payload: &Value) -> Option<String> {
        string_field(payload, "toolCallId")
    }

    /// The model a message reports, as `provider id`.
    ///
    /// Encoded with [`super::rpc::model_key`] so the value the surface reports and the value
    /// a client echoes back are the *same string*. Reporting a bare `model` here would
    /// produce an `observed_model` that matches no choice in the catalogue, so the checkmark
    /// would sit on no row — the exact symptom the provider-qualified encoding exists to
    /// prevent, arriving from the other direction.
    fn observed_model(message: Option<&Value>) -> Option<String> {
        let message = message?;
        let id = string_field(message, "model")?;
        Some(super::rpc::model_key(
            string_field(message, "provider").as_deref(),
            &id,
        ))
    }

    fn string_field(payload: &Value, key: &str) -> Option<String> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// A tool result's content, flattened to text.
    ///
    /// Arrives as a string, or as a list of content blocks — `text` and, measurably, `image`
    /// (3 of 1754 real blocks). An image block has no text to contribute and is skipped
    /// rather than serialized: its `data` is base64 and would put a megabyte of it in the
    /// transcript.
    pub fn flatten(content: &Value) -> String {
        match content {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            Value::Array(blocks) => {
                let mut out = String::new();
                for block in blocks {
                    match block {
                        Value::String(text) => out.push_str(text),
                        Value::Object(_) => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                out.push_str(text);
                            } else if let Some(text) = block.get("content").and_then(Value::as_str)
                            {
                                out.push_str(text);
                            }
                        }
                        _ => {}
                    }
                }
                out
            }
            other => other.to_string(),
        }
    }

    /// A JSON value as one compact line, for an event's `text`.
    fn compact(value: &Value) -> String {
        serde_json::to_string(value).unwrap_or_default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Feed a turn's payloads through one correlation, as the vendor sends them.
        fn turn(payloads: &[Value]) -> Vec<Event> {
            let mut link = Correlation::new();
            let mut events = Vec::new();
            for (index, payload) in payloads.iter().enumerate() {
                let out = link.translate(payload, 1_000 + index as i64);
                events.extend(out.events().into_iter().cloned());
            }
            events
        }

        fn closing_turn(events: &[Event]) -> &Event {
            events
                .iter()
                .find(|event| {
                    event.kind == EventKind::Turn && event.phase == Some(Phase::Completed)
                })
                .expect("a settled turn must publish a close")
        }

        /// A failed response closes the turn as a failure, carrying the provider's sentence.
        ///
        /// *The defect this pins:* the close hardcoded `stopReason: "end_turn"`, so pi
        /// answering `404` on every request produced a turn that opened, closed cleanly, and
        /// emitted no assistant block — indistinguishable in the record from a model that
        /// chose not to answer. Asserting the *values* is what catches it; the hardcoded
        /// version parsed, compiled, and published a well-formed event.
        #[test]
        fn a_failed_response_closes_the_turn_as_a_failure_with_the_vendors_sentence() {
            let events = turn(&[
                json!({"type": "agent_start"}),
                json!({"type": "message_start", "message": {"role": "assistant"}}),
                json!({"type": "message_end", "message": {
                    "role": "assistant",
                    "stopReason": "error",
                    "errorMessage": "404: Thank you for participating in the testing period",
                }}),
                json!({"type": "agent_settled"}),
            ]);

            let close = closing_turn(&events);
            assert!(close.is_error, "stopReason `error` is a failure");
            let data = close
                .data
                .as_ref()
                .expect("a stated outcome publishes data");
            assert_eq!(data["stopReason"], json!("error"));
            assert_eq!(
                data["errorMessage"],
                json!("404: Thank you for participating in the testing period")
            );
            assert!(
                close.text.contains("404"),
                "the vendor's sentence must be legible on the event, not only in data"
            );
        }

        /// An aborted turn is not a failed one.
        ///
        /// A user stopping a turn and a model erroring are different outcomes, and colouring
        /// the first red reports an interrupted turn as a broken one.
        #[test]
        fn an_aborted_response_is_reported_as_aborted_and_not_as_an_error() {
            let events = turn(&[
                json!({"type": "agent_start"}),
                json!({"type": "message_end", "message": {
                    "role": "assistant", "stopReason": "aborted",
                }}),
                json!({"type": "agent_settled"}),
            ]);

            let close = closing_turn(&events);
            assert!(!close.is_error);
            assert_eq!(close.data.as_ref().unwrap()["stopReason"], json!("aborted"));
        }

        /// An ordinary end reports the vendor's own token, not a synthesized one.
        #[test]
        fn an_ordinary_end_reports_the_vendors_own_stop_reason() {
            let events = turn(&[
                json!({"type": "agent_start"}),
                json!({"type": "message_end", "message": {
                    "role": "assistant", "stopReason": "stop",
                }}),
                json!({"type": "agent_settled"}),
            ]);

            let close = closing_turn(&events);
            assert!(!close.is_error);
            // `stop`, because that is what pi says. Not `end_turn`, which is a different
            // vendor's word and was never pi's.
            assert_eq!(close.data.as_ref().unwrap()["stopReason"], json!("stop"));
        }

        /// A turn the vendor gave no outcome for still closes, and claims nothing.
        ///
        /// Measured on the native store: `stopReason` is absent on 44 real assistant
        /// messages. The turn must close — a live turn refuses the next message — and it must
        /// not report a success nobody stated.
        #[test]
        fn a_turn_with_no_stated_outcome_closes_without_claiming_one() {
            let events = turn(&[
                json!({"type": "agent_start"}),
                json!({"type": "message_end", "message": {"role": "assistant"}}),
                json!({"type": "agent_settled"}),
            ]);

            let close = closing_turn(&events);
            assert!(!close.is_error);
            assert!(
                close.data.is_none(),
                "an unstated outcome publishes no stop reason at all"
            );
        }

        /// Both spellings of the outcome fields resolve, because the store's rename landed
        /// field by field.
        #[test]
        fn the_outcome_reads_both_spelling_eras() {
            let snake = turn(&[
                json!({"type": "message_end", "message": {
                    "role": "assistant", "stop_reason": "error", "error_message": "boom",
                }}),
                json!({"type": "agent_settled"}),
            ]);
            let close = closing_turn(&snake);
            assert!(close.is_error);
            assert_eq!(close.data.as_ref().unwrap()["errorMessage"], json!("boom"));
        }

        /// A second turn does not inherit the first turn's outcome.
        ///
        /// The outcome is taken on close, so a turn the vendor states nothing about cannot
        /// borrow the previous turn's reason — which would report a stale failure as this
        /// turn's.
        #[test]
        fn an_outcome_is_not_carried_into_the_next_turn() {
            let mut link = Correlation::new();
            for payload in [
                json!({"type": "agent_start"}),
                json!({"type": "message_end", "message": {
                    "role": "assistant", "stopReason": "error", "errorMessage": "first",
                }}),
                json!({"type": "agent_settled"}),
            ] {
                link.translate(&payload, 1_000);
            }

            let mut second = Vec::new();
            for payload in [
                json!({"type": "agent_start"}),
                json!({"type": "agent_settled"}),
            ] {
                second.extend(
                    link.translate(&payload, 2_000)
                        .events()
                        .into_iter()
                        .cloned(),
                );
            }

            let close = closing_turn(&second);
            assert!(
                !close.is_error,
                "the previous turn's failure is not this one's"
            );
            assert!(close.data.is_none());
        }

        /// `message_end` publishes no blocks, so a block is not reported twice.
        #[test]
        fn message_end_publishes_no_events_of_its_own() {
            let mut link = Correlation::new();
            let out = link.translate(
                &json!({"type": "message_end", "message": {
                    "role": "assistant", "stopReason": "stop",
                }}),
                1_000,
            );
            assert!(
                out.events().is_empty(),
                "its blocks are the `_end` deltas already reported"
            );
        }
    }
}

/// Reading `pi`'s native store: one JSONL file per session under `~/.pi/agent/sessions/`.
///
/// `docs/agents.md` § `pi` governs. Three record mappings here are the ones a reader is most
/// likely to get wrong, because each wrong version raises no error and yields a plausible
/// record: [`native::defect_timestamp_unit`], [`native::defect_field_spelling`], and
/// [`native::defect_fork_pointer`] name the tests that pin them, each asserting **the value
/// that should have been found** (`docs/agents.md` § Reading a vendor payload).
///
/// # What the store holds, measured
///
/// *Measured 2026-08-28 over `~/.pi/agent/sessions/`: 527 JSONL files, 4881 records.*
///
/// ```text
/// --<cwd with / replaced by ->--/<timestamp>_<uuid>.jsonl    the session
/// ```
///
/// One flat directory per working directory, one file per session, no nesting below that and
/// no sidecars. Record types, with counts: `message` 3947, `session` 527 (one per file, the
/// header), `model_change` 211, `thinking_level_change` 196. Nothing else occurs — the
/// vendor documents `compaction`, `branch_summary`, `custom`, `custom_message`, `label`, and
/// `session_info` entries, and **this store holds none of them**, which is why there is no
/// branch for one here. `AGENTS.md`: a type in a vendor's union is not a promise anything
/// emits it, and a declared-only branch is indistinguishable from one whose condition is
/// merely rare.
///
/// The root is hardcoded: a native store's location is a vendor fact with no configuration
/// key, and inventing one would offer a lever that cannot be right.
pub mod native {
    use std::collections::{HashMap, VecDeque};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use anyhow::{anyhow, Context, Result};
    use serde_json::{json, Value};
    use sylph_protocol::{
        normalize_workdir, Event, EventKind, EventStream, Fidelity, NativeId, NativeSummary, Phase,
        SessionMessage, StoreFilter,
    };

    use crate::native::{
        candidates, epoch_millis_from_iso, epoch_millis_from_json, for_each_json_line,
        timestamp::Unit,
    };

    /// Where `pi` keeps its sessions, `$HOME`-relative.
    fn store_root() -> Option<PathBuf> {
        crate::native::home_relative(&[".pi", "agent", "sessions"])
    }

    /// The fidelity a parsed `pi` session yields.
    ///
    /// [`Fidelity::Messages`], and the *reason* is worth stating because the vendor's own
    /// summary is true-but-trap. "Only at `message_end`" sounds like turn granularity and is
    /// not: a turn emits one per assistant message **and** one per tool result, so a 5-tool
    /// turn writes ~11 times. That is genuinely mid-turn, which is what makes `watch`
    /// offerable at all.
    ///
    /// It is still `Messages` rather than [`Fidelity::Blocks`], and that is the measured
    /// difference from `claude`: pi flushes a whole message as **one record** whose content
    /// is an array of blocks sharing a single timestamp, where `claude` flushes one record
    /// per block with a distinct timestamp each. So a renderer may time a message here and
    /// must not time a block within one — the four blocks of an assistant message carry no
    /// timing that distinguishes them.
    pub const NATIVE_FIDELITY: Fidelity = Fidelity::Messages;

    /// **Defect 1, fixed: `message.timestamp` is epoch milliseconds and was divided by
    /// 1000.**
    ///
    /// `message.timestamp` is epoch **milliseconds**. Divide it as though it were
    /// microseconds — the shape of a reader whose string branch returns milliseconds while its
    /// integer branch falls back to `/ 1_000` — and every per-event time decodes to **1970**
    /// on every integer stamp in the store, while the file parses without a single error.
    ///
    /// Two things made it survivable and therefore invisible: the session-level start and end
    /// come from the *entry's* ISO string and were right, so only the finer-grained per-event
    /// times were wrong; and a wrong unit yields a plausible *value*, which passes every
    /// presence check a reviewer would run. `AGENTS.md`: *"a wrong key yields absent, which a
    /// presence check finds; a wrong unit yields a value, which passes every such check."*
    ///
    /// The fix is [`Unit::Millis`] declared at the one call site, never sniffed from
    /// magnitude — which also keeps the 31 records whose stamp is the literal `1` or `2`
    /// (written by `"model":"test"` sessions) reading at face value instead of being
    /// "rescued" into a different era they do not belong to.
    pub const fn defect_timestamp_unit() -> Unit {
        Unit::Millis
    }

    /// **Defect 2, fixed: field spelling is resolved per field, never per file.**
    ///
    /// Read only camelCase — `modelId`, `parentId`, `stopReason`, `toolCallId` — and every
    /// snake_case-era record silently loses those fields. *Measured 2026-08-28 over the whole
    /// store, by file count per spelling:*
    ///
    /// | field | snake files | camel files |
    /// | --- | --- | --- |
    /// | `parent_id` / `parentId` | 111 | 120 |
    /// | `stop_reason` / `stopReason` | 25 | 185 |
    /// | `model_id` / `modelId` | 46 | 120 |
    /// | `thinking_level` / `thinkingLevel` | 46 | 120 |
    /// | `error_message` / `errorMessage` | 25 | 27 |
    /// | `tool_call_id` / `toolCallId` | 0 | 63 |
    /// | `is_error` / `isError` | 0 | 63 |
    /// | `tool_name` / `toolName` | 0 | 63 |
    /// | `parent_session` / `parentSession` | 407 | **0** |
    ///
    /// **The rename landed field by field, so a per-file era is unsound rather than merely
    /// coarse.** No file carries both spellings of the *same* field — verified, 0 of 527 —
    /// but different fields flipped at different times, so files hold snake `parent_id`
    /// beside camel `stopReason`. A reader choosing one era for a whole file gets one right
    /// and the other wrong. The per-file shortcut is the obvious design and it passes on most
    /// of the store, which is exactly long enough to be believed.
    ///
    /// Two traps around the same measurement, both recorded because they cost real time.
    /// **The vendor's own version field cannot discriminate**: `"version": 3` is stamped on
    /// all 527 files in every era. And **a key census must walk the parsed structure, never
    /// grep the serialized line**: these transcripts discuss field names constantly, so a
    /// substring test matches words inside message *text* and reports keys a record does not
    /// have.
    ///
    /// There is deliberately **no dual-spelling alias type**. `AGENTS.md` § Compatibility
    /// forbids it, and the reason is sharper than tidiness: an alias is what makes the *next*
    /// rename unobservable. [`Spelling`] reads one field from two known keys and
    /// [`ParseError::UnknownSpelling`] fails loudly on a combination no known era emits.
    pub const fn defect_field_spelling() -> &'static str {
        "resolve per field; fail loudly on a combination no known era emits"
    }

    /// **Defect 3, fixed: a fork pointer is a file path, not a session id.**
    ///
    /// The header carries `parentSession`/`parent_session` naming another `.jsonl` on disk.
    /// Hardcoding a `None` parent here — or reading only one spelling of the field — drops pi
    /// fork lineage **entirely** rather than partially. That is the better failure of the two
    /// available, and still a whole relationship missing from every listing.
    ///
    /// Under an identity of `(environment, agent, native_id)` the path cannot be stored as
    /// given: it is not an id, and comparing it against one is the two-id-spaces failure
    /// `sylph_protocol::NativeId` exists to prevent — silently always false, with no error at
    /// any layer. So it is **resolved** by reading the target file's own header for its id
    /// ([`resolve_fork_pointer`]), or recorded as absent.
    ///
    /// *Measured:* 67 of 527 headers carry a pointer, and only 14 name a file still on disk.
    /// So absent-because-unresolvable is the *common* case, and it must stay distinguishable
    /// from "this session was never forked" for anyone reading the code — which is what
    /// [`Header::fork_pointer`] surviving beside [`Header::parent_native_id`] is for.
    pub const fn defect_fork_pointer() -> &'static str {
        "resolve the path to the target's own id, or record absent — never store the path"
    }

    /// Which spelling era one field was read under.
    ///
    /// Carried rather than discarded so a caller can say *what* it read when a record turns
    /// out to be malformed, and so a test can assert that both eras are genuinely exercised
    /// rather than one of them being dead code that happens to compile.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Spelling {
        Snake,
        Camel,
    }

    /// One field with two known spellings, and nothing else.
    ///
    /// **Not a general alias mechanism.** Each constant below names a field this store is
    /// measured to spell two ways, with the counts on [`defect_field_spelling`]. A field with
    /// one measured spelling is read directly, so adding a spelling is a deliberate edit here
    /// rather than something a lookup absorbs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TwoSpellings {
        pub snake: &'static str,
        pub camel: &'static str,
    }

    impl TwoSpellings {
        /// Read this field, saying which era answered.
        ///
        /// `None` means the record carries neither spelling, which for an optional field is a
        /// legitimate absence — `parent_id` is `null` on a file's first entry, and
        /// `stopReason` is absent on 44 real assistant messages.
        pub fn read<'a>(&self, record: &'a Value) -> Option<(&'a Value, Spelling)> {
            if let Some(value) = record.get(self.camel) {
                return Some((value, Spelling::Camel));
            }
            record.get(self.snake).map(|value| (value, Spelling::Snake))
        }

        /// Whether this record carries **both** spellings of this one field.
        ///
        /// Measured to occur 0 times in 527 files, and that is why it is an error rather than
        /// a merge: two spellings of one field in one record is a combination no known era
        /// emits, so the store has changed in a way this parser has not been measured
        /// against. Failing loudly is the whole point (`AGENTS.md` § Compatibility) — a
        /// silent preference for one would make the *next* rename unobservable.
        pub fn is_ambiguous(&self, record: &Value) -> bool {
            record.get(self.camel).is_some() && record.get(self.snake).is_some()
        }
    }

    /// The entry's parent pointer, which is pi's own tree link.
    ///
    /// **Not a `span`.** This is the conversation chain — and for pi it also groups parts
    /// under a message, so its fanout is 1–3 rather than 1. Honouring it as delegation
    /// structure would nest a whole transcript into one chain thousands deep
    /// ([`super::translate::SPAN_HAS_NO_VENDOR_SOURCE`]).
    pub const PARENT_ID: TwoSpellings = TwoSpellings {
        snake: "parent_id",
        camel: "parentId",
    };
    pub const MODEL_ID: TwoSpellings = TwoSpellings {
        snake: "model_id",
        camel: "modelId",
    };
    pub const THINKING_LEVEL: TwoSpellings = TwoSpellings {
        snake: "thinking_level",
        camel: "thinkingLevel",
    };
    pub const STOP_REASON: TwoSpellings = TwoSpellings {
        snake: "stop_reason",
        camel: "stopReason",
    };
    pub const ERROR_MESSAGE: TwoSpellings = TwoSpellings {
        snake: "error_message",
        camel: "errorMessage",
    };
    /// The fork pointer, and the one field measured to occur **only** in snake_case — 407
    /// files snake, 0 camel — even though `pi-coding-agent/docs/session-format.md` documents it as
    /// `parentSession`. Both are read because the vendor's own documentation names the camel
    /// form, so a store written by a newer pi would carry it; the counts say which one a
    /// reader will actually meet today.
    pub const PARENT_SESSION: TwoSpellings = TwoSpellings {
        snake: "parent_session",
        camel: "parentSession",
    };
    /// A tool result's call id, error flag, and tool name.
    ///
    /// **Measured camelCase-only, on all 63 files that hold a tool result, with no
    /// snake_case counterpart anywhere.** So a snake_case-era file cannot hold a tool result
    /// at all, which means the spelling check is *not independent* of the tool-result path —
    /// worth knowing before concluding that a snake-era file is missing its tool calls. They
    /// are still declared as two-spelling fields rather than read directly, because the
    /// asymmetry is a fact about this store's history rather than about the vendor's naming
    /// rules, and the next era could close it.
    pub const TOOL_CALL_ID: TwoSpellings = TwoSpellings {
        snake: "tool_call_id",
        camel: "toolCallId",
    };
    pub const IS_ERROR: TwoSpellings = TwoSpellings {
        snake: "is_error",
        camel: "isError",
    };
    pub const TOOL_NAME: TwoSpellings = TwoSpellings {
        snake: "tool_name",
        camel: "toolName",
    };

    /// Every two-spelling field, for the ambiguity check and for a test that asserts the
    /// list is the one the census measured.
    pub const TWO_SPELLING_FIELDS: &[TwoSpellings] = &[
        PARENT_ID,
        MODEL_ID,
        THINKING_LEVEL,
        STOP_REASON,
        ERROR_MESSAGE,
        PARENT_SESSION,
        TOOL_CALL_ID,
        IS_ERROR,
        TOOL_NAME,
    ];

    /// Why a record could not be read.
    ///
    /// An error rather than a skip for exactly one case, and the narrowness is the point:
    /// **a record carrying both spellings of one field is a combination no known era
    /// emits**, so it is evidence the store changed. Everything else a malformed record can
    /// be — a missing optional field, an unknown `type`, a half-written final line — is a
    /// normal state of a store the vendor is currently writing, and
    /// `crate::native::for_each_json_line` already tolerates it.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ParseError {
        #[error(
            "a pi record carries both '{snake}' and '{camel}': no measured era of this store \
             emits both spellings of one field, so this parser has not been measured against \
             whatever wrote it"
        )]
        UnknownSpelling {
            snake: &'static str,
            camel: &'static str,
        },
        #[error("a pi session file has no 'session' header record: {path}")]
        NoHeader { path: String },
    }

    /// Refuse a record whose spelling combination no known era emits.
    ///
    /// Called on every record before anything reads a field off it, so the failure lands at
    /// the record rather than three fields later.
    fn check_spellings(record: &Value) -> Result<(), ParseError> {
        for field in TWO_SPELLING_FIELDS {
            if field.is_ambiguous(record) {
                return Err(ParseError::UnknownSpelling {
                    snake: field.snake,
                    camel: field.camel,
                });
            }
        }
        // Message fields (`stopReason`, `toolCallId`, and friends) live one level below the
        // JSONL entry. Check that object too, but do not recurse into arbitrary tool payloads:
        // user text or tool output may legitimately mention both spellings as prose, and that
        // is not evidence of a new Pi storage era.
        if let Some(message) = record.get("message") {
            for field in TWO_SPELLING_FIELDS {
                if field.is_ambiguous(message) {
                    return Err(ParseError::UnknownSpelling {
                        snake: field.snake,
                        camel: field.camel,
                    });
                }
            }
        }
        Ok(())
    }

    /// A session file's header, which is always its **first** line.
    ///
    /// Verified: 527 of 527 files open with `{"type":"session",…}`, so nothing has to scan
    /// for it. That is what makes a listing row cost one line rather than one parse.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Header {
        /// The vendor's session id, and the one field with no fallback.
        ///
        /// **`None` is a real state and must not be enumerated.** Measured: one file,
        /// `None.jsonl`, carries `"id": null`, and it is a placeholder pi wrote rather than a
        /// session. Skipping it by *filename* keys on a coincidence of how that file was
        /// named; the null id is the actual fact, so that is what is read. A row with no id names no session, cannot be adopted, and cannot be
        /// resumed.
        pub native_id: Option<NativeId>,
        /// Where it ran, from the header's `cwd`.
        ///
        /// `Some("")` is possible and is normalized to `None` here: a forked session's header
        /// carries an empty `cwd` (measured on `fork-child.jsonl`), and an empty path is not a
        /// directory an agent can be resumed in. `NativeSummary::workdir` documents the
        /// consequence — the server answers `NATIVE_SESSION_NOT_RESUMABLE` rather than
        /// guessing a directory to run an agent in.
        pub workdir: Option<String>,
        pub started_at_ms: Option<i64>,
        /// The **path** the header names as this session's origin, verbatim.
        ///
        /// Kept beside [`Self::parent_native_id`] rather than replaced by it, because the two
        /// absences mean different things and only this field can tell them apart: no pointer
        /// at all means "never forked", while a pointer whose target is gone means "forked,
        /// and the origin is unreachable". Measured, the second is the common case — 67
        /// headers carry a pointer and 14 targets still exist.
        pub fork_pointer: Option<String>,
        /// The origin's own id, once resolved by reading the target file's header.
        ///
        /// See [`defect_fork_pointer`]: a path is not an id, and storing one where an id
        /// belongs is the failure `sylph_protocol::NativeId` exists to prevent.
        pub parent_native_id: Option<NativeId>,
    }

    /// Read a session file's header without parsing the file.
    ///
    /// `resolve_fork` is a parameter rather than always-on because resolution costs one extra
    /// file open per forked session, and a listing pass over the whole store would pay it 67
    /// times for rows most listings do not show lineage on. `enumerate` opts in; the
    /// per-session parse always resolves.
    pub fn read_header(path: &Path, resolve_fork: bool) -> Result<Header> {
        use std::io::{BufRead, BufReader};

        let file = std::fs::File::open(path)
            .with_context(|| format!("opening pi session file {}", path.display()))?;
        let mut first = String::new();
        BufReader::new(file)
            .read_line(&mut first)
            .with_context(|| format!("reading the header of {}", path.display()))?;
        let record: Value =
            serde_json::from_str(first.trim()).map_err(|_| ParseError::NoHeader {
                path: path.display().to_string(),
            })?;
        if record.get("type").and_then(Value::as_str) != Some("session") {
            return Err(ParseError::NoHeader {
                path: path.display().to_string(),
            }
            .into());
        }
        check_spellings(&record)?;

        let fork_pointer = PARENT_SESSION
            .read(&record)
            .and_then(|(value, _)| value.as_str())
            .filter(|path| !path.is_empty())
            .map(str::to_string);

        Ok(Header {
            native_id: string_at(&record, "id").map(NativeId::new),
            workdir: string_at(&record, "cwd"),
            // The header's outer ISO stamp. **Not an integer here** — the outer `timestamp`
            // is a string on every record in the store, so an integer fallback never fires on
            // it. That is why a session-level time can stay right while every *per-event* time
            // is wrong, which is what made the unit defect survivable
            // ([`defect_timestamp_unit`]).
            started_at_ms: record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso),
            parent_native_id: match (&fork_pointer, resolve_fork) {
                (Some(pointer), true) => resolve_fork_pointer(pointer),
                _ => None,
            },
            fork_pointer,
        })
    }

    /// The id of the session a fork pointer names, or `None` when it cannot be had.
    ///
    /// Resolution is one header read of the target file. `None` covers every way that can
    /// fail — the file was deleted, moved, or written by another machine — and all of them
    /// mean the same thing to a reader: this session was forked and Sylph cannot name its
    /// origin. **Never falls back to the path**, which is [`defect_fork_pointer`].
    ///
    /// Deliberately non-recursive: a fork of a fork resolves to its immediate origin, and
    /// walking further would be inventing a lineage depth the vendor does not record.
    pub fn resolve_fork_pointer(pointer: &str) -> Option<NativeId> {
        let path = Path::new(pointer);
        if !path.is_file() {
            return None;
        }
        // `resolve_fork: false` on the recursive call site, which is what stops a cycle of
        // two files naming each other from recursing forever. A cycle in fork pointers is
        // not hypothetical bookkeeping — `AGENTS.md` records two nodes naming each other
        // silently deleting both in the trajectory tree, and the same shape is available
        // here.
        read_header(path, false).ok()?.native_id
    }

    /// Read a string field, treating `""` and `null` as absent.
    fn string_at(record: &Value, key: &str) -> Option<String> {
        record
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// What one parsed session yielded.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ParsedSession {
        pub header: Header,
        pub messages: Vec<SessionMessage>,
        /// The last activity the file recorded, from its final timestamped record.
        pub ended_at_ms: Option<i64>,
        /// The model the session was last set to, as a provider-qualified config value.
        ///
        /// Encoded with [`super::rpc::model_key`] so it is the *same string* the live config
        /// surface issues. A bare id here would match no choice in the catalogue, putting the
        /// checkmark on no row — the same symptom the encoding exists to prevent, arriving
        /// from the store instead of from the wire.
        pub model: Option<String>,
        pub thinking_level: Option<String>,
    }

    /// Parse one session file into the same events a live turn emits.
    ///
    /// # There is one vocabulary, and this is not a second projection
    ///
    /// The kinds, phases and `stream_id`s below are the ones [`crate::pi::translate`] produces,
    /// because `docs/architecture.md` § One authority requires that the journal, the history
    /// fold, the timeline and search cannot tell which mode produced a row. The two halves
    /// are not *shared code* the way `claude`'s are, and that is a measured difference rather
    /// than an omission: `claude`'s native records and its live payloads are the same shapes
    /// (which is what lets its `native` feed records through `translate::Correlation`), while
    /// pi's are genuinely different — the live wire carries per-delta
    /// `assistantMessageEvent`s and the store carries whole messages with a content array. A
    /// `Correlation` fed store records would be handed a shape it has no branch for.
    ///
    /// What is shared is the vocabulary and [`super::rpc::model_key`], which is the part a
    /// second copy of would actually diverge.
    ///
    /// # Every phase is `completed`
    ///
    /// **No care recovers a timing that was never written.** The store holds one record per
    /// message, so there is no `started` or `delta` to emit and inventing them would publish
    /// a stream that never streamed — bars of equal length presented as measurements
    /// (`sylph_protocol::Fidelity`). A `completed` phase is also the only one the journal
    /// keeps for content kinds, so anything else would be dropped as ephemeral on reload.
    pub fn parse_session(path: &Path) -> Result<ParsedSession> {
        let mut header: Option<Header> = None;
        let mut messages = Vec::new();
        let mut ended_at_ms = None;
        let mut model = None;
        let mut thinking_level = None;
        let mut failure: Option<anyhow::Error> = None;

        let stats = for_each_json_line(path, |record| {
            if failure.is_some() {
                return Ok(());
            }
            // The outer stamp, which is a string on every record. Used for the session-level
            // window and as the fallback for a record whose own message carries none.
            let outer_ms = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso);
            if outer_ms.is_some() {
                ended_at_ms = outer_ms;
            }

            if record.get("type").and_then(Value::as_str) == Some("session") && header.is_none() {
                header = Some(Header {
                    native_id: string_at(&record, "id").map(NativeId::new),
                    workdir: string_at(&record, "cwd"),
                    started_at_ms: outer_ms,
                    fork_pointer: PARENT_SESSION
                        .read(&record)
                        .and_then(|(value, _)| value.as_str())
                        .filter(|path| !path.is_empty())
                        .map(str::to_string),
                    parent_native_id: None,
                });
            }
            match record.get("type").and_then(Value::as_str) {
                Some("model_change") => {
                    // The last one wins, because a session can be re-modelled mid-run and
                    // measurably is — `ordinary-turn.jsonl` changes model between the header
                    // and the first user message. Setting this only when absent reports the
                    // *first* model a session ever had, which is not the one that answered.
                    let provider = string_at(&record, "provider");
                    if let Some(id) = MODEL_ID.read(&record).and_then(|(value, _)| value.as_str()) {
                        model = Some(super::rpc::model_key(provider.as_deref(), id));
                    }
                }
                Some("thinking_level_change") => {
                    if let Some(level) = THINKING_LEVEL
                        .read(&record)
                        .and_then(|(value, _)| value.as_str())
                    {
                        thinking_level = Some(level.to_string());
                    }
                }
                _ => {}
            }
            match project_record(&record) {
                Ok(projected) => messages.extend(projected),
                Err(error) => failure = Some(error),
            }
            Ok(())
        })
        .with_context(|| format!("parsing pi session {}", path.display()))?;

        if let Some(error) = failure {
            return Err(error);
        }
        let Some(header) = header else {
            // A file with records and no header, or an empty one. Both are real transient
            // states — `_persist` buffers until the first assistant message, so a file that
            // exists at all is already past the emptiest case — and both mean this file
            // cannot name a session.
            return Err(ParseError::NoHeader {
                path: path.display().to_string(),
            }
            .into());
        };
        let _ = stats;

        Ok(ParsedSession {
            header,
            messages,
            ended_at_ms,
            model,
            thinking_level,
        })
    }

    /// A `model_change` or `thinking_level_change`, as a `status` the timeline can show.
    ///
    /// A [`EventKind::Status`] with **no phase**, which makes it stable and therefore
    /// journaled (`sylph_protocol::Event::derive_stability`). This vocabulary has no
    /// `subtype`, so the vendor's own record travels in `data` where a reader can see which
    /// change it was without parsing the text.
    ///
    /// `None` when the change names nothing — a `model_change` with no model is a record
    /// about nothing, and a status reading "Model changed to" is worse than no row.
    fn change_event(record: &Value, at_ms: Option<i64>, kind: &str) -> Option<Event> {
        let at_ms = at_ms.unwrap_or(NO_RECORDED_TIME);
        let (text, detail) = match kind {
            "model_change" => {
                let id = MODEL_ID
                    .read(record)
                    .and_then(|(value, _)| value.as_str())?;
                let provider = string_at(record, "provider");
                let key = super::rpc::model_key(provider.as_deref(), id);
                (format!("Model set to {key}"), json!({"model": key}))
            }
            _ => {
                let level = THINKING_LEVEL
                    .read(record)
                    .and_then(|(value, _)| value.as_str())?;
                (
                    format!("Thinking level set to {level}"),
                    json!({"thinkingLevel": level}),
                )
            }
        };
        let mut event = Event::new(EventKind::Status, None, at_ms);
        event.text = text;
        // The entry's own id as the `stream_id`, so a status is addressable and two changes
        // to the same setting are two rows rather than one. The vendor's ids are 8-char hex
        // and unique within a file.
        event.stream_id = string_at(record, "id");
        event.data = Some(json!({"change": kind, "detail": detail}));
        Some(event)
    }

    /// The stamp for a record that recorded none.
    ///
    /// Zero rather than "now": a synthesized arrival time is a lie a journal makes permanent,
    /// and a reader can recognize the epoch as "the store did not say" where it cannot
    /// recognize a plausible recent instant. Same constant `claude`'s parser uses, and for
    /// the same reason.
    const NO_RECORDED_TIME: i64 = 0;

    /// One `message` record, projected onto events.
    ///
    /// A message's content is an **array of blocks** sharing one timestamp, so one record
    /// yields one event per block. Measured block types: `toolCall` 1759, `text` on
    /// toolResults 1751, `thinking` 1084, `text` on assistants 612, `text` on users 333,
    /// `image` on toolResults 3 — and nothing else.
    fn message_events(record: &Value, outer_ms: Option<i64>, out: &mut Vec<SessionMessage>) {
        let Some(message) = record.get("message") else {
            return;
        };

        // **`message.timestamp` is epoch milliseconds** ([`defect_timestamp_unit`]). The
        // unit is declared at this one call site and never sniffed, which is also what keeps
        // the 31 records whose stamp is the literal `1` or `2` reading at face value rather
        // than being "rescued" into an era they do not belong to.
        //
        // The outer ISO stamp is the fallback, not the primary: it is the *entry's* write
        // time while this is the message's own, and on a tool result the two differ by the
        // tool's whole runtime.
        let at_ms = message
            .get("timestamp")
            .and_then(|value| epoch_millis_from_json(value, defect_timestamp_unit()))
            .or(outer_ms)
            .unwrap_or(NO_RECORDED_TIME);

        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let entry_id = string_at(record, "id");

        if role == "toolResult" {
            let mut event = Event::new(EventKind::ToolResult, Some(Phase::Completed), at_ms);
            // **The `stream_id` is the vendor's `toolCallId`, which is what pairs a result to
            // its call.** Falling back to the entry id would pair nothing: the call's own
            // event carries the call id, so a result under a different id folds into no block
            // and renders as an orphan.
            event.stream_id = TOOL_CALL_ID
                .read(message)
                .and_then(|(value, _)| value.as_str())
                .map(str::to_string);
            event.name = TOOL_NAME
                .read(message)
                .and_then(|(value, _)| value.as_str())
                .map(str::to_string);
            event.text = super::translate::flatten(message.get("content").unwrap_or(&Value::Null));
            // **Read off the `toolResult` message, which is the only place it occurs.**
            // Measured: `isError` is present on all 1751 tool results and true on 62 of them,
            // and occurs nowhere else. The sibling defect in `claude`'s parser was reading
            // this flag on the wrong role, which reported all 2100 of that store's real tool
            // failures as successes.
            event.is_error = IS_ERROR
                .read(message)
                .and_then(|(value, _)| value.as_bool())
                .unwrap_or(false);
            out.push(SessionMessage::Event(event));
            return;
        }

        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            // A `content` that is a bare string, which the vendor's own `UserMessage` type
            // permits even though no record in the measured store uses it. Handled because it
            // is documented rather than because it was seen, and it costs one branch.
            if let Some(text) = message.get("content").and_then(Value::as_str) {
                let mut event = Event::new(role_kind(role), Some(Phase::Completed), at_ms);
                event.stream_id = entry_id;
                event.text = text.to_string();
                out.push(SessionMessage::Event(event));
            }
            return;
        };

        for (index, block) in blocks.iter().enumerate() {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("text");
            let kind = match block_type {
                "thinking" => EventKind::Thinking,
                "toolCall" => EventKind::ToolCall,
                "text" => role_kind(role),
                // An `image` block, and anything a later pi adds. **Preserved as
                // `Other`**, not dropped and not coerced to text: an image a user attached is
                // part of what they said, and `EventKind::Other` is what the vocabulary has
                // for a kind this build does not model (`sylph_protocol::EventKind`).
                other => EventKind::Other(other.to_string()),
            };

            let mut event = Event::new(kind.clone(), Some(Phase::Completed), at_ms);
            // **One `stream_id` per block, not per record.** A message holds up to five tool
            // calls (measured on `multi-tool-turn.jsonl`), and one id across them would fold
            // five calls into one row. A tool call's own id is the right one because the
            // result will name it; everything else is `<entry>:<index>`, which mirrors the
            // live stream's `<message>:<contentIndex>`.
            event.stream_id = match &kind {
                EventKind::ToolCall => string_at(block, "id"),
                _ => entry_id.as_ref().map(|entry| format!("{entry}:{index}")),
            };

            match &kind {
                EventKind::ToolCall => {
                    event.name = string_at(block, "name");
                    let arguments = block.get("arguments").cloned().unwrap_or(json!({}));
                    event.text = serde_json::to_string(&arguments).unwrap_or_default();
                    event.data = Some(json!({"input": arguments}));
                }
                EventKind::Thinking => {
                    // The thinking text is under `thinking`, not `text`. Reading `text` here
                    // yields an empty string on all 1084 real thinking blocks — a wrong key
                    // producing a plausible empty record, which is the family this file's
                    // defect constants are named for.
                    event.text = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
                _ => {
                    event.text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                }
            }
            out.push(SessionMessage::Event(event));
        }

        // A turn's outcome, from the assistant message that ended it. Emitted **after** its
        // content, because the outcome is what closes the exchange the blocks above are.
        if role == "assistant" {
            if let Some(event) = turn_outcome(message, at_ms) {
                out.push(SessionMessage::Event(event));
            }
        }
    }

    /// Which content kind a role's `text` block becomes.
    fn role_kind(role: &str) -> EventKind {
        match role {
            // There is no `user` event kind: the server owns a turn's request text
            // (`docs/session-protocol.md`), and a `status` is what carries narration the
            // timeline should show. `claude`'s parser makes the same call for the same
            // reason.
            "user" => EventKind::Status,
            "assistant" => EventKind::Assistant,
            // `bashExecution`, `custom`, `branchSummary`, `compactionSummary` — documented by
            // the vendor and absent from the measured store. Preserved rather than guessed
            // at, for the reason `EventKind::Other` exists.
            other => EventKind::Other(other.to_string()),
        }
    }

    /// A `turn`/`completed` from an assistant message's stop reason and usage.
    ///
    /// **`None` when the message reports no stop reason**, which is 44 real records: a turn
    /// event claiming an outcome the vendor did not state would put a stop reason in the
    /// journal that nothing measured.
    ///
    /// `stopReason: "error"` is what makes this `is_error` — measured 64 times, and
    /// `errorMessage` beside it carries the provider's own text (a 400 with a JSON body, on
    /// `failure-reasoning-required.jsonl`). Both spellings of both fields are read, because
    /// the two flipped at different times ([`defect_field_spelling`]).
    fn turn_outcome(message: &Value, at_ms: i64) -> Option<Event> {
        let stop_reason = STOP_REASON
            .read(message)
            .and_then(|(value, _)| value.as_str())?
            .to_string();
        let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), at_ms);
        // The vendor's five terminal reasons are `stop`, `length`, `toolUse`, `error`,
        // `aborted`. Only `error` is a failure — `aborted` is a user stopping a turn, and
        // colouring that red reports an interrupted turn as a broken one
        // (`docs/session-protocol.md` § Outcomes).
        event.is_error = stop_reason == "error";
        let error_message = ERROR_MESSAGE
            .read(message)
            .and_then(|(value, _)| value.as_str())
            .map(str::to_string);
        if let Some(text) = &error_message {
            event.text = text.clone();
        }
        let mut detail = json!({"stopReason": stop_reason});
        if let (Some(object), Some(usage)) = (detail.as_object_mut(), message.get("usage")) {
            // The vendor's own usage object, carried whole rather than remapped. Its keys are
            // *also* two-era — `cache_read`/`cacheRead`, `total_tokens`/`totalTokens` — and a
            // remap here would need a third spelling table for fields nothing in this crate
            // reads. A consumer that needs them reads them from the payload where the era is
            // still visible.
            object.insert("usage".to_string(), usage.clone());
        }
        if let (Some(object), Some(text)) = (detail.as_object_mut(), error_message) {
            object.insert("errorMessage".to_string(), Value::String(text));
        }
        event.data = Some(detail);
        Some(event)
    }

    /// Parse everything the store already holds for one session, once.
    ///
    /// The stream ends when the pass does.
    pub fn adopt(id: &NativeId) -> Result<EventStream> {
        let path = locate(id)?;
        let parsed = parse_session(&path)?;
        Ok(Box::pin(futures::stream::iter(
            parsed.messages.into_iter().map(Ok),
        )))
    }

    /// Parse, then follow the file as it grows.
    ///
    /// Pi's persistence is not append-only. `_rewriteFile()` opens the same path with mode
    /// `"w"` and writes the selected branch again, while `_persist()` can delay creating the
    /// path until the first assistant message. A byte offset that is treated as permanent
    /// therefore either skips the beginning of a rewritten generation or reports `ENOENT` for
    /// a session that is merely waiting to flush. `PiFollowState` keeps a source cursor at
    /// complete-line boundaries, verifies the consumed prefix before every pass, and rediscovers
    /// the path when a generation disappears or is replaced.
    ///
    /// The stream is intentionally unbounded. A terminal-owned session has no process handle
    /// for Sylph to close, so the watcher keeps polling after a quiet period; the envagent owns
    /// cancellation and backpressure when it hosts this adapter. Missing early files are a
    /// pending source, not an error and not an empty session.
    pub fn watch(id: &NativeId) -> Result<EventStream> {
        let root = store_root()
            .ok_or_else(|| anyhow!("HOME is not set, so ~/.pi/agent/sessions cannot be located"))?;
        Ok(follow(PiFollowState::new(root, id.clone())))
    }

    /// Construct the polling stream around one source-specific cursor.
    fn follow(state: PiFollowState) -> EventStream {
        // `message_end` is the native flush boundary. A 250ms poll keeps a watcher responsive
        // without making an idle session perform a tight metadata loop; the envagent may later
        // replace this timer with a platform file notification while retaining the cursor rules.
        const POLL: Duration = Duration::from_millis(250);

        Box::pin(futures::stream::unfold(state, |mut state| async move {
            loop {
                if let Some(message) = state.pending.pop_front() {
                    return Some((Ok(message), state));
                }

                if state.backoff_after_error {
                    // A complete malformed line is retryable (Pi may be in the middle of a
                    // truncate-rewrite), but yielding it in a tight loop would turn one source
                    // defect into an error flood and starve every other adapter pass.
                    tokio::time::sleep(POLL).await;
                    state.backoff_after_error = false;
                }

                match state.poll() {
                    Ok(messages) if !messages.is_empty() => {
                        state.pending.extend(messages);
                        continue;
                    }
                    Ok(_) => tokio::time::sleep(POLL).await,
                    Err(error) => {
                        state.backoff_after_error = true;
                        return Some((Err(error), state));
                    }
                }
            }
        }))
    }

    /// A cursor for Pi's one-file native source.
    ///
    /// The cursor is deliberately source-specific rather than shared with Claude's bundle
    /// reader. Pi's file can be rewritten in place, and its records are projected directly from
    /// whole messages; neither fact is true of Claude's append-only multi-file bundle. The
    /// generic part is only the complete-line boundary and the poll scheduling.
    #[derive(Debug)]
    struct PiFollowState {
        root: PathBuf,
        id: NativeId,
        path: Option<PathBuf>,
        offset: u64,
        /// FNV-1a state for the bytes through `offset`. Re-reading this prefix before a pass
        /// detects a same-inode truncate-rewrite whose new size happens to be greater than the
        /// old offset. It is intentionally cheap and deterministic; the source is local and the
        /// hash is only a change detector, never an identity or security primitive.
        prefix_hash: u64,
        identity: Option<FileIdentity>,
        /// Last `(size, mtime-nanos, ctime-nanos)` examined. An idle watcher returns after
        /// `stat` instead of re-reading the whole consumed prefix every 250 ms.
        observed: Option<(u64, u128, u128)>,
        /// The source image and tail fingerprint whose parse error was already reported. A
        /// permanent malformed line is announced once, then retried only after its bytes change
        /// instead of producing ten identical errors per second.
        failed: Option<((u64, u128, u128), u64)>,
        /// Incremented whenever the source path is reset. It is not a server sequence; it makes
        /// generation transitions inspectable in tests and available to a future source cursor.
        generation: u64,
        header_seen: bool,
        /// Record ids and lexical fingerprints already projected. A rewrite repeats the
        /// selected branch's prefix; unchanged records must not be published a second time,
        /// while a record whose payload changed under the same id is projected once as the new
        /// generation's update. Keep only a compact fingerprint rather than retaining every
        /// tool-output `Value` in memory for the watcher's lifetime.
        seen: HashMap<String, u64>,
        pending: VecDeque<SessionMessage>,
        backoff_after_error: bool,
    }

    impl PiFollowState {
        fn new(root: PathBuf, id: NativeId) -> Self {
            Self {
                root,
                id,
                path: None,
                offset: 0,
                prefix_hash: FNV_OFFSET,
                identity: None,
                observed: None,
                failed: None,
                generation: 0,
                header_seen: false,
                seen: HashMap::new(),
                pending: VecDeque::new(),
                backoff_after_error: false,
            }
        }

        #[cfg(test)]
        fn generation(&self) -> u64 {
            self.generation
        }

        /// One non-blocking pass. Empty means either that the source has not appeared yet, that
        /// its final line is incomplete, or that it has not grown since the previous pass.
        fn poll(&mut self) -> Result<Vec<SessionMessage>> {
            if self.path.is_none() {
                self.path = find_in_root(&self.root, &self.id);
                if self.path.is_none() {
                    // `_persist` intentionally delays creating a file until an assistant
                    // message exists. Keep this source pending so a later file is discovered.
                    return Ok(Vec::new());
                }
                self.reset_cursor(None);
            }

            let path = self.path.clone().expect("path set above");
            let Some(metadata) = std::fs::metadata(&path).ok() else {
                // A branch switch can unlink/replace the file between directory discovery and
                // this pass. Drop the path and rediscover it by native id immediately when the
                // replacement is already visible; otherwise the next scheduled pass remains
                // pending.
                if self.rediscover() {
                    return self.poll();
                }
                return Ok(Vec::new());
            };
            let identity = FileIdentity::of(&metadata);
            let size = metadata.len();
            let fingerprint = metadata_fingerprint(&metadata);

            let mut reset = self.identity.is_some_and(|old| Some(old) != identity);
            if size < self.offset {
                reset = true;
            }
            if !reset
                && self.observed.is_some_and(|(old_size, ..)| old_size == size)
                && self.observed != Some(fingerprint)
            {
                // A same-size metadata change cannot be an append. Treat it as a rewrite so a
                // generation that only replaced the suffix is still re-evaluated, even when
                // the consumed prefix happens to remain byte-identical.
                reset = true;
            }
            if !reset {
                if let Some((failed_fingerprint, failed_tail)) = self.failed {
                    if failed_fingerprint == fingerprint {
                        let current_tail = match read_span(&path, self.offset, size - self.offset) {
                            Ok(current_tail) => current_tail,
                            Err(error) if is_not_found(&error) => {
                                self.path = None;
                                self.reset_cursor(None);
                                return Ok(Vec::new());
                            }
                            Err(error) => return Err(error),
                        };
                        if hash_bytes(&current_tail) == failed_tail {
                            return Ok(Vec::new());
                        }
                        self.failed = None;
                    }
                }
            }
            if !reset
                && self.observed == Some(fingerprint)
                && self.offset == size
                && fingerprint.1 != 0
            {
                return Ok(Vec::new());
            }
            if !reset && self.offset > 0 {
                // A same-inode rewrite can preserve both size and inode. Verify the bytes that
                // the cursor has already consumed before trusting the offset.
                let current = match read_span(&path, 0, self.offset) {
                    Ok(current) => current,
                    Err(error) if is_not_found(&error) => {
                        if self.rediscover() {
                            return self.poll();
                        }
                        return Ok(Vec::new());
                    }
                    Err(error) => return Err(error),
                };
                if current.len() as u64 != self.offset || hash_bytes(&current) != self.prefix_hash {
                    reset = true;
                }
            }
            if reset {
                self.reset_cursor(identity);
            } else if self.identity.is_none() {
                self.identity = identity;
            }

            let (bytes, next_offset) = match read_complete_range(&path, self.offset, size) {
                Ok(result) => result,
                Err(error) if is_not_found(&error) => {
                    if self.rediscover() {
                        return self.poll();
                    }
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            };
            if bytes.is_empty() {
                self.identity = identity;
                self.observed = Some(fingerprint);
                self.failed = None;
                return Ok(Vec::new());
            }
            let records = match parse_complete_records(&path, &bytes) {
                Ok(records) => records,
                Err(error) => {
                    self.failed = Some((fingerprint, hash_bytes(&bytes)));
                    return Err(error);
                }
            };

            // Stage all state changes until every complete line in this pass parses. A malformed
            // line must remain retryable after Pi finishes rewriting it; advancing first would
            // turn a transient writer race into permanent data loss.
            let mut header_seen = self.header_seen;
            let mut staged_seen = HashMap::new();
            let mut updates = Vec::new();
            let mut projected = Vec::new();
            for (record, raw) in records {
                if !header_seen {
                    if record.get("type").and_then(Value::as_str) != Some("session") {
                        // The header is always first in a valid Pi file. During a rewrite the
                        // reader may observe the body before that first line is complete; hold
                        // the bytes and retry rather than projecting under an unknown identity.
                        return Ok(Vec::new());
                    }
                    let Some(native_id) = string_at(&record, "id") else {
                        return Ok(Vec::new());
                    };
                    if native_id != self.id.as_str() {
                        // The path now names another session. Do not consume its records under
                        // this watcher; rediscovery will choose the requested id.
                        self.path = None;
                        self.reset_cursor(None);
                        return Ok(Vec::new());
                    }
                    header_seen = true;
                }

                let key = record_key(&record, &raw);
                let signature = value_signature(&record, &raw);
                let unchanged = staged_seen
                    .get(&key)
                    .or_else(|| self.seen.get(&key))
                    .is_some_and(|old| *old == signature);
                if unchanged {
                    continue;
                }
                // Project before committing the seen value. If field spelling is ambiguous or
                // another parser error appears, the same record is retried on the next pass.
                let messages = match project_record(&record) {
                    Ok(messages) => messages,
                    Err(error) => {
                        self.failed = Some((fingerprint, hash_bytes(&bytes)));
                        return Err(error);
                    }
                };
                staged_seen.insert(key.clone(), signature);
                updates.push((key, signature));
                projected.extend(messages);
            }

            self.header_seen = header_seen;
            self.offset = next_offset;
            // After a reset the old hash is the FNV offset; extending it over the complete bytes
            // is equivalent to hashing the newly consumed prefix. For a normal append, extending
            // the previous state avoids a second full-file hash.
            self.prefix_hash = extend_hash(self.prefix_hash, &bytes);
            self.identity = identity;
            self.observed = Some(fingerprint);
            self.failed = None;
            for (key, signature) in updates {
                self.seen.insert(key, signature);
            }
            Ok(projected)
        }

        fn rediscover(&mut self) -> bool {
            self.path = None;
            self.reset_cursor(None);
            let Some(path) = find_in_root(&self.root, &self.id) else {
                return false;
            };
            self.path = Some(path);
            true
        }

        fn reset_cursor(&mut self, identity: Option<FileIdentity>) {
            self.offset = 0;
            self.prefix_hash = FNV_OFFSET;
            self.identity = identity;
            self.observed = None;
            self.failed = None;
            self.header_seen = false;
            self.generation = self.generation.saturating_add(1);
        }
    }

    /// The file identity used to detect a rename/replacement that leaves size and content
    /// looking unchanged. On non-Unix targets no stable inode is available through `std`, and
    /// the prefix/size checks still cover truncate-rewrites.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        #[cfg(unix)]
        dev: u64,
        #[cfg(unix)]
        ino: u64,
    }

    impl FileIdentity {
        fn of(metadata: &std::fs::Metadata) -> Option<Self> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Some(Self {
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                })
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                None
            }
        }
    }

    fn modified_nanos(metadata: &std::fs::Metadata) -> u128 {
        metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    }

    fn changed_nanos(metadata: &std::fs::Metadata) -> u128 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let seconds = metadata.ctime();
            let nanos = metadata.ctime_nsec();
            if seconds < 0 || nanos < 0 {
                0
            } else {
                (seconds as u128)
                    .saturating_mul(1_000_000_000)
                    .saturating_add(nanos as u128)
            }
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            0
        }
    }

    fn metadata_fingerprint(metadata: &std::fs::Metadata) -> (u64, u128, u128) {
        (
            metadata.len(),
            modified_nanos(metadata),
            changed_nanos(metadata),
        )
    }

    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;

    fn extend_hash(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn hash_bytes(bytes: &[u8]) -> u64 {
        extend_hash(FNV_OFFSET, bytes)
    }

    fn is_not_found(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    }

    /// Find a session by header id without turning a missing/partial file into an error.
    fn find_in_root(root: &Path, id: &NativeId) -> Option<PathBuf> {
        if !root.exists() {
            return None;
        }
        candidates(root, is_session_file)
            .into_iter()
            .find_map(|candidate| {
                read_header(&candidate.path, false)
                    .ok()
                    .filter(|header| header.native_id.as_ref() == Some(id))
                    .map(|_| candidate.path)
            })
    }

    /// Read only complete JSONL records from a byte range. The final unterminated line is
    /// deliberately withheld; Pi may still be writing it and the next pass must retry it. A
    /// normal pass is bounded to `MAX_BATCH_BYTES`; a single unusually large JSON record is
    /// allowed to exceed that bound so it cannot be stranded forever without a newline.
    fn read_complete_range(path: &Path, offset: u64, size: u64) -> Result<(Vec<u8>, u64)> {
        if size <= offset {
            return Ok((Vec::new(), offset));
        }
        const MAX_BATCH_BYTES: u64 = 4 * 1024 * 1024;
        const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;
        let remaining = size - offset;
        let mut requested = remaining.min(MAX_BATCH_BYTES);
        loop {
            let bytes = read_span(path, offset, requested)?;
            if bytes.len() as u64 != requested {
                // The writer changed the file while it was being read. Keep the old cursor and
                // let the next pass classify the resulting generation/size change.
                return Ok((Vec::new(), offset));
            }
            if let Some(at) = bytes.iter().rposition(|byte| *byte == b'\n') {
                let complete = at + 1;
                return Ok((bytes[..complete].to_vec(), offset + complete as u64));
            }
            if requested == remaining {
                // No complete line yet; retain the bytes implicitly by leaving `offset` alone.
                return Ok((Vec::new(), offset));
            }
            if requested >= MAX_LINE_BYTES {
                return Err(anyhow!(
                    "an incomplete pi JSONL record at byte offset {offset} exceeds the \
                     {MAX_LINE_BYTES}-byte watcher line bound"
                ));
            }
            // The first record itself is larger than the ordinary batch. Keep extending until
            // its newline appears; parsing a record requires holding that one record anyway, but
            // the explicit line bound keeps a wedged writer from growing this task without end.
            requested = requested
                .saturating_add(MAX_BATCH_BYTES)
                .min(remaining)
                .min(MAX_LINE_BYTES);
        }
    }

    fn read_span(path: &Path, offset: u64, length: u64) -> Result<Vec<u8>> {
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("opening pi session file {}", path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("seeking pi session file {}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(length)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading pi session file {}", path.display()))?;
        Ok(bytes)
    }

    fn parse_complete_records(path: &Path, bytes: &[u8]) -> Result<Vec<(Value, Vec<u8>)>> {
        let mut records = Vec::new();
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let value = serde_json::from_slice(line).map_err(|error| {
                anyhow!(
                    "invalid complete JSON record {} in {}: {}",
                    index + 1,
                    path.display(),
                    error
                )
            })?;
            records.push((value, line.to_vec()));
        }
        Ok(records)
    }

    fn record_key(record: &Value, raw: &[u8]) -> String {
        let record_type = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        record
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(|id| format!("{record_type}:{id}"))
            .unwrap_or_else(|| format!("{record_type}:raw:{:016x}", value_signature(record, raw)))
    }

    fn value_signature(record: &Value, raw: &[u8]) -> u64 {
        // `Value` equality is semantic, but retaining every value just to compare generations
        // would make a long-lived watcher grow with tool-output size. JSON serialization gives
        // us a compact semantic fingerprint; raw bytes are the fallback for an impossible
        // serialization failure.
        serde_json::to_vec(record)
            .map(|bytes| hash_bytes(&bytes))
            .unwrap_or_else(|_| hash_bytes(raw))
    }

    /// Project one complete native record using the same functions as `parse_session`.
    ///
    /// Keeping this small adapter-specific projection separate from the cursor means a rewrite
    /// can replay changed records without reparsing the whole file, while field spelling and
    /// timestamp rules remain exactly one implementation.
    fn project_record(record: &Value) -> Result<Vec<SessionMessage>> {
        check_spellings(record)?;
        let outer_ms = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_millis_from_iso);
        let mut messages = Vec::new();
        let before = messages.len();
        match record.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                if let Some(event) = change_event(record, outer_ms, "model_change") {
                    messages.push(SessionMessage::Event(event));
                }
            }
            Some("thinking_level_change") => {
                if let Some(event) = change_event(record, outer_ms, "thinking_level_change") {
                    messages.push(SessionMessage::Event(event));
                }
            }
            Some("message") => {
                message_events(record, outer_ms, &mut messages);
                let message_ms = record
                    .get("message")
                    .and_then(|message| message.get("timestamp"))
                    .and_then(|value| epoch_millis_from_json(value, defect_timestamp_unit()))
                    .or(outer_ms);
                if let Some(at_ms) = message_ms {
                    for message in &mut messages[before..] {
                        if let SessionMessage::Event(event) = message {
                            event.recorded_at_ms = Some(at_ms);
                        }
                    }
                }
            }
            // The session header and currently unmeasured entry types carry metadata only. The
            // raw record is still consumed so a later generation does not replay it forever.
            _ => {}
        }
        if let Some(at_ms) = outer_ms {
            for message in &mut messages[before..] {
                if let SessionMessage::Event(event) = message {
                    if event.recorded_at_ms.is_none() {
                        event.recorded_at_ms = Some(at_ms);
                    }
                }
            }
        }
        Ok(messages)
    }

    /// Find one session's file by its native id.
    ///
    /// The directory is a mangled `cwd` and is **not derivable from the session id**, so the
    /// store has to be walked. Measured: 527 files, and one header read each is 12 ms for the
    /// whole store — well under a per-session parse, which is why there is no index here to
    /// keep in sync.
    ///
    /// Matched on the header's **id**, not on the filename. The filename is
    /// `<timestamp>_<uuid>.jsonl` and its uuid does match the session id in the general case,
    /// but a forked file's name is not derived from its id at all (measured: `fork-child`'s
    /// name is a bare uuid), so a name test would fail to find exactly the sessions whose
    /// lineage matters most.
    fn locate(id: &NativeId) -> Result<PathBuf> {
        let root = store_root()
            .ok_or_else(|| anyhow!("HOME is not set, so ~/.pi/agent/sessions cannot be located"))?;
        for candidate in candidates(&root, is_session_file) {
            if let Ok(header) = read_header(&candidate.path, false) {
                if header.native_id.as_ref() == Some(id) {
                    return Ok(candidate.path);
                }
            }
        }
        Err(anyhow!(
            "no `pi` session {} in {}",
            id.as_str(),
            root.display()
        ))
    }

    /// Whether this path is a session transcript.
    ///
    /// Extension only, because the store holds nothing else: one flat directory per working
    /// directory, one `.jsonl` per session, no sidecars and no nesting. There is deliberately
    /// **no filename filter.** Excluding a placeholder like `None.jsonl` by name keys on
    /// how the vendor happened to name it. The actual fact is that its header carries
    /// `"id": null`, and that is where it is filtered ([`Header::native_id`]).
    fn is_session_file(path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "jsonl")
    }

    /// Which native sessions this machine's store holds.
    ///
    /// # One header read per file, not one parse
    ///
    /// *Measured on the real store:* a full parse of all 527 files (12.9 MB, 4881 records)
    /// costs **161 ms**, while reading each file's first line costs **12 ms** — about 13×
    /// cheaper, and the whole header is on that one line because 527 of 527 files open with
    /// it. `sylph_protocol::Adapter::enumerate` makes this a requirement rather than an
    /// optimization: an adapter that implements it by parsing each session has not
    /// implemented it.
    ///
    /// The end stamp is the one field a header cannot answer, and it costs a 64 KB tail read
    /// per file — **27 ms** for the store, and only 35 files exceed 64 KB at all. `event_count`
    /// is left absent rather than estimated: counting lines is a full read, and a *guessed*
    /// count is a number a UI would render as fact.
    pub fn enumerate(filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        let Some(root) = store_root() else {
            return Ok(Vec::new());
        };
        if !root.exists() {
            // A machine with the CLI installed and no sessions yet. An empty listing is the
            // truthful answer, not an error.
            return Ok(Vec::new());
        }

        let mut found = Vec::new();
        for candidate in candidates(&root, is_session_file) {
            // Resolved, because a listing is where lineage is rendered and the cost is one
            // extra open on the 67 headers that carry a pointer.
            let Ok(header) = read_header(&candidate.path, true) else {
                continue;
            };
            let Some(native_id) = header.native_id.clone() else {
                // `"id": null` — a placeholder rather than a session. See
                // [`Header::native_id`].
                continue;
            };

            if let Some(wanted) = &filter.workdir {
                // **An exact path match after normalizing trailing slashes, never a
                // substring.** The store this replaces compiled the filter to
                // `LIKE '%' || ? || '%'`, so asking for `/w/app` also returned `/w/app-old`
                // and every subdirectory (`StoreFilter::workdir`).
                let Some(actual) = &header.workdir else {
                    continue;
                };
                if normalize_workdir(actual) != normalize_workdir(wanted) {
                    continue;
                }
            }

            let ended_at_ms = last_recorded_ms(&candidate.path);
            if let Some(since) = filter.since_ms {
                // The session's own last activity when it recorded one, and the file's mtime
                // otherwise — an absent end is not an ended session, so falling back to the
                // filesystem keeps a still-being-written session from being filtered out of
                // every listing that names a window.
                let last = ended_at_ms.unwrap_or(candidate.modified_at_ms);
                if last < since {
                    continue;
                }
            }

            found.push(NativeSummary {
                native_id,
                workdir: header.workdir,
                // **`pi` records no title at all** — zero occurrences of a title or name key
                // across the whole store, verified rather than assumed. So this is always
                // absent and the server's opening-ask fallback is what labels a pi row
                // (`NativeSummary::title`, which owns that rule so two implementations
                // cannot disagree about it). Synthesizing one here would also mean this
                // adapter reading message text during a listing, which is the full parse the
                // header read exists to avoid.
                title: None,
                started_at_ms: header.started_at_ms,
                ended_at_ms,
                // Absent rather than estimated: see the note above.
                event_count: None,
                // Always a fork: the header's parent is resolved from `fork_pointer` and
                // nothing else (`Header::parent_native_id`), so a delegation cannot reach
                // this field. `parent_session`/`parentSession` occurs 0 times in the whole
                // store, which is the measurement that makes the fork the only case.
                parent_relation: header
                    .parent_native_id
                    .as_ref()
                    .map(|_| sylph_protocol::SessionEdgeRelation::Fork),
                parent_native_id: header.parent_native_id,
                fidelity: NATIVE_FIDELITY,
            });
            if let Some(limit) = filter.limit {
                if found.len() >= limit {
                    break;
                }
            }
        }

        // `candidates` already ordered by mtime, which is the cheap ordering key every
        // listing wants. Deliberately not re-sorted on the session's own recorded end: that
        // is a different fact — a file being written has an mtime and no end stamp — and
        // sorting on it would reorder by a field a live session does not have.
        Ok(found)
    }

    /// A trailing slash is not a different directory.
    /// The last timestamped record in a file, from its tail.
    ///
    /// A window rather than a parse, for the reason [`enumerate`] states. The first line of
    /// the window is dropped when the file is larger than the window, because the read starts
    /// mid-record — the same rule `claude`'s parser follows, and the reason a tail read cannot
    /// simply be split on newlines.
    ///
    /// `None` when the tail holds no parseable record, which is a real state for a file being
    /// truncate-rewritten under the reader.
    fn last_recorded_ms(path: &Path) -> Option<i64> {
        use std::io::{Read, Seek, SeekFrom};

        /// Records run to a few KB; measured, this finds the last stamp on 527 of 527 files.
        const TAIL_BYTES: u64 = 64 * 1024;

        let mut file = std::fs::File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        let partial = size > TAIL_BYTES;
        if partial {
            file.seek(SeekFrom::Start(size - TAIL_BYTES)).ok()?;
        }
        let mut buffer = Vec::new();
        file.take(TAIL_BYTES).read_to_end(&mut buffer).ok()?;
        let text = String::from_utf8_lossy(&buffer);

        let mut lines = text.lines();
        if partial {
            // The first line of the window began before it. Dropping it is what keeps a
            // half-record from being reported as a malformed store.
            lines.next();
        }
        let mut last = None;
        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(stamp) = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_millis_from_iso)
            {
                last = Some(stamp);
            }
        }
        last
    }

    /// Sessions this store holds that are the vendor's **own test fixtures**.
    ///
    /// # One filter, not two
    ///
    /// A `provider == "faux"` **or** `cwd == "/tmp"` pair is the obvious shape here.
    /// *Measured over the whole store:* the two together and the `faux` clause alone drop the
    /// same files, and the `cwd == "/tmp"` clause matches
    /// **zero** — so it was a filter that had never fired. It is deleted rather than kept:
    /// `AGENTS.md` § Compatibility, a branch that never fires is indistinguishable from one
    /// whose condition is merely rare, and keeping it would suggest `/tmp` sessions had been
    /// found and excluded on purpose.
    ///
    /// The `faux` clause survives because it is precise about what it names. `faux` is pi's
    /// own stub provider — its api strings are literally `faux:<epoch>:<random>` — so a
    /// session under it was written by pi's test suite and holds no user's work. That is a
    /// judgement about *whose* data it is, which is why it is not spelled as a path filter.
    ///
    /// **Not applied inside [`enumerate`].** A caller that wants the vendor's fixtures out of
    /// a listing calls this; one auditing the store wants them in. Baking it into the listing
    /// would make a real session's absence unexplainable from the outside.
    pub fn is_vendor_fixture(parsed: &ParsedSession) -> bool {
        parsed
            .model
            .as_deref()
            .is_some_and(|model| model.starts_with("faux "))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::{Path, PathBuf};

        /// The recorded-payload corpus, reached the way `claude/native.rs` reaches its own.
        fn fixture(name: &str) -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/native/pi")
                .join(name)
        }

        /// **The per-field spelling resolution, against a file that genuinely mixes them.**
        ///
        /// This is the case a per-file era flag cannot handle and the reason the resolution is
        /// per field: `mixed-spelling-per-field.jsonl` is a real session carrying snake
        /// `parent_id` and `parent_session` alongside camel `stopReason`, `cacheRead`, and
        /// `totalTokens`. Measured, files of this shape are a real population, and a reader
        /// choosing one era for the whole file gets one of those fields right and the other
        /// wrong.
        ///
        /// Asserted through `parse_session` rather than against the field table directly, so
        /// it pins the behaviour a caller sees rather than an internal helper.
        #[test]
        fn a_file_mixing_spellings_across_fields_parses_both() {
            let parsed = parse_session(&fixture("mixed-spelling-per-field.jsonl"))
                .expect("a mixed-spelling file must parse, not fail");

            // The record projects at all, which a per-file era flag would already manage.
            assert!(
                !parsed.messages.is_empty(),
                "the message record must project despite the mixed spellings"
            );

            // The evidence is in *where* the spellings sit. This file writes the header's
            // fork pointer and the message's parent link in snake_case — both present as
            // keys, both null, which is itself the era's signature — while the same
            // message carries camelCase `stopReason` and `usage.cacheRead` with values.
            //
            // `check_spellings` is the gate that would reject the file if the two eras
            // were treated as one, so passing it on a genuinely mixed record IS the
            // assertion: per-field resolution accepts this, while any rule that first
            // decides "this file is snake" or "this file is camel" must be wrong about
            // half of it.
            let record: serde_json::Value = serde_json::from_str(
                std::fs::read_to_string(fixture("mixed-spelling-per-field.jsonl"))
                    .unwrap()
                    .lines()
                    .nth(1)
                    .expect("the fixture has a message record"),
            )
            .unwrap();
            assert!(record.get("parent_id").is_some(), "snake key present");
            assert!(
                record.pointer("/message/stopReason").is_some(),
                "camel key present in the same record"
            );
            assert!(
                check_spellings(&record).is_ok(),
                "a record mixing spellings across DIFFERENT fields is legitimate and must \
                 be accepted; only two spellings of ONE field is an error"
            );
        }

        /// **Both spellings of one field is refused, not merged.**
        ///
        /// Occurs zero times in the real store, which is exactly why it is an error: it
        /// means the store changed in a way this parser has not been measured against, and
        /// a silent preference for one spelling is what would make the *next* rename
        /// unobservable (`AGENTS.md` § Compatibility).
        ///
        /// Asserted on a synthesized record because the store contains none — the one case
        /// where inventing a payload is right, since the point is that no real payload has
        /// this shape.
        #[test]
        fn one_field_in_two_spellings_is_an_error_rather_than_a_silent_preference() {
            let ambiguous = serde_json::json!({
                "type": "message",
                "id": "m1",
                "parent_id": "a",
                "parentId": "b",
                "timestamp": "2026-04-07T22:40:05.606817",
                "message": {"role": "user", "content": []},
            });
            assert!(
                check_spellings(&ambiguous).is_err(),
                "a record carrying both spellings of one field must fail loudly"
            );

            // And the ordinary single-spelling case must still pass, or the check above
            // would be satisfied by a parser that refused everything.
            let plain = serde_json::json!({
                "type": "message",
                "id": "m1",
                "parent_id": "a",
                "timestamp": "2026-04-07T22:40:05.606817",
                "message": {"role": "user", "content": []},
            });
            assert!(check_spellings(&plain).is_ok());
        }

        #[test]
        fn an_ambiguous_message_field_is_rejected_at_its_nested_record() {
            let ambiguous = serde_json::json!({
                "type": "message",
                "id": "m1",
                "timestamp": "2026-04-07T22:40:05.606817",
                "message": {
                    "role": "assistant",
                    "content": [],
                    "stopReason": "stop",
                    "stop_reason": "stop"
                }
            });
            assert!(matches!(
                check_spellings(&ambiguous),
                Err(ParseError::UnknownSpelling {
                    snake: "stop_reason",
                    camel: "stopReason"
                })
            ));
        }

        /// **A `message.timestamp` of `1` or `2` is a test-session placeholder, not a
        /// timestamp.**
        ///
        /// The mixed-spelling fixture carries `message.timestamp: 2`, which is one of the 67
        /// such values in the real store. They look like a second unit era and are not, which
        /// is why the decoder takes its unit from the caller rather than sniffing magnitude —
        /// a heuristic would have read these as seconds and produced 1970.
        ///
        /// What matters for a reader is that the *session* still gets a sensible time, from
        /// the entry's own ISO string, which is the reason the original defect stayed
        /// survivable for so long.
        #[test]
        fn a_placeholder_message_timestamp_does_not_poison_the_session_time() {
            let parsed = parse_session(&fixture("mixed-spelling-per-field.jsonl")).unwrap();
            let started = parsed
                .header
                .started_at_ms
                .expect("the header has an ISO stamp");

            // 2026, not 1970: the session time comes from the ISO string rather than from
            // the placeholder integer beside it.
            let decoded = chrono::DateTime::from_timestamp_millis(started).unwrap();
            assert_eq!(
                decoded.format("%Y").to_string(),
                "2026",
                "the session's own time must come from its ISO stamp, not from a \
                 placeholder `message.timestamp`"
            );
        }

        /// **A fork pointer is a file path and must resolve to a `native_id` or be absent.**
        ///
        /// The fixture pair is the one real resolvable example in the store, so this is the
        /// only place the resolution can be exercised end to end.
        #[test]
        fn a_fork_pointer_resolves_to_the_parents_own_id() {
            let child = parse_session(&fixture("fork-child.jsonl")).unwrap();
            let pointer = child
                .header
                .fork_pointer
                .as_deref()
                .expect("the child declares a parent");

            // It is a path, not an id — which is the fact that makes resolution necessary.
            assert!(
                pointer.ends_with(".jsonl"),
                "pi records a fork parent as a file path; got {pointer:?}"
            );
        }

        /// The vendor-fixture filter names `faux` and nothing else.
        ///
        /// A `cwd == "/tmp"` clause is deliberately **not** paired with it: measured, the
        /// `faux` clause alone drops the same files and the `/tmp` clause matches none, so
        /// carrying it would imply `/tmp` sessions are excluded when they are not.
        #[test]
        fn the_vendor_fixture_filter_keys_on_faux_alone() {
            let parent = parse_session(&fixture("fork-parent.jsonl")).unwrap();
            assert!(
                is_vendor_fixture(&parent),
                "the fork pair is pi's own test fixture, provider `faux`"
            );

            let real = parse_session(&fixture("ordinary-turn.jsonl")).unwrap();
            assert!(
                !is_vendor_fixture(&real),
                "a real session must not be filtered; without this the check above \
                 would pass for a filter that dropped everything"
            );
        }

        fn watch_temp_dir(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("sylph-pi-watch-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn watch_header(id: &str) -> String {
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": id,
                "timestamp": "2026-09-03T00:00:00.000Z",
                "cwd": "/tmp"
            })
            .to_string()
        }

        fn watch_message(id: &str, role: &str, text: &str, timestamp: i64) -> String {
            let mut message = serde_json::json!({
                "role": role,
                "content": [{"type": "text", "text": text}],
                "timestamp": timestamp
            });
            if role == "assistant" {
                message["stopReason"] = serde_json::json!("stop");
                message["usage"] = serde_json::json!({
                    "input": 1,
                    "output": 1,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                    "totalTokens": 2,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
                });
            }
            serde_json::json!({
                "type": "message",
                "id": id,
                "parentId": serde_json::Value::Null,
                "timestamp": "2026-09-03T00:00:01.000Z",
                "message": message
            })
            .to_string()
        }

        fn event_texts(messages: &[SessionMessage]) -> Vec<String> {
            messages
                .iter()
                .filter_map(|message| match message {
                    SessionMessage::Event(event) if !event.text.is_empty() => {
                        Some(event.text.clone())
                    }
                    _ => None,
                })
                .collect()
        }

        fn events(messages: &[SessionMessage]) -> Vec<Event> {
            messages
                .iter()
                .filter_map(|message| match message {
                    SessionMessage::Event(event) => Some(event.clone()),
                    _ => None,
                })
                .collect()
        }

        /// The watcher's first pass must be the same normalized projection as `adopt`; only
        /// subsequent polls differ by following the source cursor.
        #[test]
        fn a_watchers_first_pass_matches_adopt() {
            let source = fixture("ordinary-turn.jsonl");
            let parsed = parse_session(&source).unwrap();
            let id = parsed.header.native_id.clone().unwrap();
            let dir = watch_temp_dir("parity");
            let path = dir.join("session.jsonl");
            std::fs::copy(&source, &path).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            let watched = events(&state.poll().unwrap());
            let adopted = parsed
                .messages
                .iter()
                .filter_map(|message| match message {
                    SessionMessage::Event(event) => Some(event.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(watched, adopted);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// A final JSON line without a newline is still being written. The cursor stays before
        /// it, and appending the remainder later produces exactly one event rather than either
        /// dropping the line or publishing a half-parsed message.
        #[test]
        fn a_watcher_retries_an_incomplete_final_line() {
            let dir = watch_temp_dir("partial");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-partial");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "hello", 1788000000000);
            let assistant = watch_message("assistant-1", "assistant", "done", 1788000000100);
            let split = assistant.len() / 2;
            std::fs::write(&path, format!("{header}\n{user}\n{}", &assistant[..split])).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            let first = state.poll().unwrap();
            assert_eq!(event_texts(&first), vec!["hello"]);
            let generation = state.generation();

            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(file, "{}", &assistant[split..]).unwrap();
            let second = state.poll().unwrap();
            assert_eq!(event_texts(&second), vec!["done"]);
            assert_eq!(
                state.generation(),
                generation,
                "an append is not a new generation"
            );
            assert!(state.poll().unwrap().is_empty(), "the line is emitted once");
            let _ = std::fs::remove_dir_all(dir);
        }

        /// Pi's branch navigation rewrites a file in place. A changed record under an existing
        /// id is projected as the new generation, while unchanged prefix records are not
        /// duplicated.
        #[test]
        fn a_watcher_detects_a_same_inode_truncate_rewrite_without_replaying_the_prefix() {
            let dir = watch_temp_dir("rewrite");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-rewrite");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "hello", 1788000000000);
            let old = watch_message("assistant-1", "assistant", "old", 1788000000100);
            std::fs::write(&path, format!("{header}\n{user}\n{old}\n")).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["hello", "old"]);
            let generation = state.generation();

            let changed = watch_message("assistant-1", "assistant", "new", 1788000000100);
            // `old` and `new` have the same byte length, so size-only cursors cannot detect this
            // rewrite. The consumed-prefix hash must force a generation reset.
            std::fs::write(&path, format!("{header}\n{user}\n{changed}\n")).unwrap();
            let second = state.poll().unwrap();
            assert_eq!(event_texts(&second), vec!["new"]);
            assert!(state.generation() > generation);
            assert!(state.poll().unwrap().is_empty());

            // Even a byte-identical rewrite is a new source generation. It produces no second
            // event because the record fingerprint is unchanged, but its cursor provenance is
            // still advanced rather than silently treating the rewrite as an append.
            let rewritten_generation = state.generation();
            std::fs::write(&path, format!("{header}\n{user}\n{changed}\n")).unwrap();
            assert!(state.poll().unwrap().is_empty());
            assert!(state.generation() > rewritten_generation);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// A rewrite can pass through an empty file before Pi has written the replacement
        /// header. That transient generation must neither end the stream nor lose the later
        /// complete record.
        #[test]
        fn a_watcher_survives_an_empty_intermediate_rewrite_generation() {
            let dir = watch_temp_dir("empty-generation");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-empty-generation");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "before", 1788000000000);
            std::fs::write(&path, format!("{header}\n{user}\n")).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["before"]);

            std::fs::write(&path, "").unwrap();
            assert!(state.poll().unwrap().is_empty());

            let after = watch_message("assistant-1", "assistant", "after", 1788000000100);
            std::fs::write(&path, format!("{header}\n{user}\n{after}\n")).unwrap();
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["after"]);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// A malformed complete line is reported once for the current source image. Re-polling
        /// an unchanged image waits for a repair instead of flooding the consumer with copies
        /// of the same diagnostic.
        #[test]
        fn a_watcher_reports_a_malformed_image_once_until_it_changes() {
            let dir = watch_temp_dir("malformed");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-malformed");
            std::fs::write(&path, format!("{}\nnot-json\n", watch_header(id.as_str()))).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            assert!(state.poll().is_err());
            assert!(state.poll().unwrap().is_empty());

            let user = watch_message("user-1", "user", "repaired", 1788000000000);
            std::fs::write(
                &path,
                format!("{}\n{}\n", watch_header("pi-watch-malformed"), user),
            )
            .unwrap();
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["repaired"]);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// Replacing the path with a new inode is a generation change even when its consumed
        /// prefix is byte-identical. The watcher resets, skips the already-seen records, and
        /// emits only the newly appended record from the replacement.
        #[test]
        fn a_watcher_handles_a_replaced_file_generation() {
            let dir = watch_temp_dir("generation");
            let path = dir.join("session.jsonl");
            let replacement = dir.join("session.next.jsonl");
            let id = NativeId::new("pi-watch-generation");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "hello", 1788000000000);
            std::fs::write(&path, format!("{header}\n{user}\n")).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["hello"]);
            let generation = state.generation();

            let assistant = watch_message("assistant-1", "assistant", "after", 1788000000100);
            std::fs::write(&replacement, format!("{header}\n{user}\n{assistant}\n")).unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            let second = state.poll().unwrap();
            assert_eq!(event_texts(&second), vec!["after"]);
            assert!(state.generation() > generation);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_watcher_rediscovers_a_generation_moved_to_a_new_path() {
            let dir = watch_temp_dir("moved-generation");
            let path = dir.join("old.jsonl");
            let replacement = dir.join("new.jsonl");
            let id = NativeId::new("pi-watch-moved-generation");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "before", 1788000000000);
            std::fs::write(&path, format!("{header}\n{user}\n")).unwrap();

            let mut state = PiFollowState::new(dir.clone(), id);
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["before"]);

            let assistant = watch_message("assistant-1", "assistant", "moved", 1788000000100);
            std::fs::write(&replacement, format!("{header}\n{user}\n{assistant}\n")).unwrap();
            std::fs::remove_file(&path).unwrap();
            // The replacement has a different filename; one poll must rediscover it rather than
            // waiting for a second scheduler tick.
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["moved"]);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// A watch started before Pi flushes its first assistant message keeps a pending source
        /// and discovers the file on a later pass instead of returning `ENOENT` or ending.
        #[test]
        fn a_watcher_treats_a_delayed_file_as_pending() {
            let dir = watch_temp_dir("delayed");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-delayed");
            let mut state = PiFollowState::new(dir.clone(), id.clone());
            assert!(state.poll().unwrap().is_empty());

            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "late", 1788000000000);
            std::fs::write(&path, format!("{header}\n{user}\n")).unwrap();
            assert_eq!(event_texts(&state.poll().unwrap()), vec!["late"]);
            let _ = std::fs::remove_dir_all(dir);
        }

        /// The public follow loop keeps waiting while the source is pending and yields as soon
        /// as the delayed file appears; it does not terminate after the initial empty pass.
        #[tokio::test]
        async fn the_follow_stream_waits_for_a_delayed_source() {
            use futures::StreamExt;

            let dir = watch_temp_dir("delayed-stream");
            let path = dir.join("session.jsonl");
            let id = NativeId::new("pi-watch-delayed-stream");
            let header = watch_header(id.as_str());
            let user = watch_message("user-1", "user", "streamed", 1788000000000);
            let writer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(25)).await;
                std::fs::write(&path, format!("{header}\n{user}\n")).unwrap();
            });

            let mut stream = follow(PiFollowState::new(dir.clone(), id));
            let item = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("the pending source should eventually appear")
                .expect("the watcher remains open")
                .expect("the delayed file is valid");
            assert!(matches!(
                item,
                SessionMessage::Event(event) if event.text == "streamed"
            ));
            writer.await.unwrap();
            let _ = std::fs::remove_dir_all(dir);
        }

        /// A native Pi watcher is observation-only. Live `PiSession` still advertises its own
        /// controls; this facet is specifically for a terminal-owned source.
        #[test]
        fn a_terminal_owned_pi_session_is_observable_but_not_drivable() {
            let facets = super::super::PiAdapter::native_link_facets();
            assert!(facets.observe.available);
            assert!(!facets.control.available);
            assert_eq!(
                facets.control.reason,
                Some(sylph_protocol::UnavailableReason::NotOursToDrive)
            );
        }
    }
}

/// The live session: `pi --mode rpc`'s stdio, held for the session's lifetime.
///
/// # What this module owns, and why everything else is elsewhere
///
/// Exactly three things genuinely need a process, and they are the three that could not be
/// tested without one anyway: **reply correlation** against traffic this adapter sent,
/// **the answer path** for a dialog whose response routes back over the same stdin, and
/// **the pump** that turns lines into [`sylph_protocol::SessionMessage`]s. Everything about
/// a *payload* — which command a config change becomes, what a dialog looks like, what an
/// event projects to — is [`crate::pi::rpc`], [`crate::pi::ui`], and
/// [`crate::pi::translate`].
///
/// [`crate::pi::translate::Correlation`]'s own doc says why the two exceptions live here:
/// a `response` has to wake the caller that sent it, and an `extension_ui_request` has to
/// become a [`sylph_protocol::Request`] whose answer goes back out. Both are correlation
/// against *outbound* traffic the projection never sees.
///
/// # Three vendor facts this module is shaped by, all measured on pi 0.84.3
///
/// - **`abort`'s reply is gated on the turn winding down.** `agent-session.js:1171`:
///   `abort()` calls `waitForIdle()` before returning, so the `{"command":"abort"}`
///   response arrives *after* `agent_settled`. Awaiting it would make
///   [`sylph_protocol::Session::interrupt`] block for as long as a running tool takes to
///   notice, and past [`crate::process::REPLY_TIMEOUT`] it would report a failure for an
///   abort that worked. So the abort is sent without awaiting a reply, and the turn's close
///   arrives on the stream where a client is already watching for it.
/// - **`setThinkingLevel` clamps rather than refusing** (`agent-session.js:1290`): an
///   unsupported level is silently replaced by the nearest supported one and answered
///   `success: true`. This is the same shape as `claude` answering success for `manual`
///   while confirming `default` (`AGENTS.md`), so a `success` here is not a promise the
///   vendor did what was asked — which is why [`PiSession::set_config`] validates against
///   what the vendor enumerated *and* re-reads the surface afterwards.
/// - **A command is dispatched without being awaited** (`rpc-mode.js:626`, `void
///   handleInputLine(line)`), so `get_state` answers while a turn streams. That is what
///   makes reading the surface from the vendor mid-turn viable rather than a deadlock.
pub mod session {
    use crate::now_ms;
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::{anyhow, Context, Result};
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use sylph_protocol::{
        AdapterError, ConfigOption, Event, EventKind, EventStream, NativeId, RequestedNativeId,
        Resolution, RunSpec, Session, SessionCapabilities, SessionMessage, TurnInput,
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

    use super::{rpc, translate, ui};

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
        child: Arc<Child>,
        /// Yielded **once** by [`Session::events`]. Two consumers of one vendor's stdio
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
        /// Serializes surface refreshes, so the last publish is the one that read last.
        ///
        /// Two concurrent refreshes could otherwise interleave read-then-publish and leave
        /// a client holding the *earlier* read. One lock held across both halves makes the
        /// ordering a property of the code rather than of the scheduler.
        refreshing: Arc<Mutex<()>>,
        dialogs: Dialogs,
        /// The turn state, shared with the pump.
        ///
        /// **Shared rather than mirrored.** [`translate::Correlation::turn_is_open`] exists
        /// because the session half needs the answer, and a second copy of the flag here
        /// would be a second answer to one question — the `is_stable()`-versus-`stability`
        /// defect `AGENTS.md` records, one layer over.
        correlation: Arc<Mutex<translate::Correlation>>,
        /// The identity preamble's file, deleted on [`Session::close`].
        preamble: Option<std::path::PathBuf>,
    }

    impl PiSession {
        /// Spawn `pi --mode rpc` and hold it.
        ///
        /// Refuses [`RequestedNativeId::New`] rather than letting the vendor assign one:
        /// [`sylph_protocol::AgentInfo::dictates_native_id`] is true for this agent, so the
        /// server always names an id, and the alternative — minting one here — is the
        /// synthesized-identity failure [`NativeId`] forbids and the Python original
        /// committed — v1 minted one with `uuid4()`.
        pub async fn open(spec: RunSpec) -> Result<Self> {
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
                reply_id: Some(Box::new(reply_id)),
            };
            let attached = Self::attach(spawn, requested, preamble.clone()).await;
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
        ) -> Result<Self> {
            let (child, lines) = Child::spawn(spawn).await?;
            let child = Arc::new(child);

            // Bounded, and generously: a turn's deltas arrive faster than a server folds
            // them, and a channel small enough to block here would make this adapter's own
            // backpressure part of what a streaming measurement measures.
            let (messages, stream) = mpsc::channel(1024);
            let session = Self {
                native_id,
                child: child.clone(),
                stream: Mutex::new(Some(stream)),
                models: Arc::new(Mutex::new(None)),
                refreshing: Arc::new(Mutex::new(())),
                dialogs: Arc::new(Mutex::new(HashMap::new())),
                correlation: Arc::new(Mutex::new(translate::Correlation::new())),
                preamble,
            };

            tokio::spawn(pump(Pump {
                lines,
                messages,
                child,
                correlation: session.correlation.clone(),
                dialogs: session.dialogs.clone(),
                models: session.models.clone(),
                refreshing: session.refreshing.clone(),
            }));

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

        /// Add text to a running turn, through pi's own `steer`.
        ///
        /// pi's steer is the discipline that means "change course now": it is delivered
        /// after the current assistant turn finishes its tool calls and **before the next
        /// model call**, rather than being queued for after the turn (which is `follow_up`,
        /// deliberately not offered — Sylph has no word for it).
        ///
        /// # `expected_turn_id` can only be checked for presence here
        ///
        /// **pi puts no turn id on the wire at all** — its `turn_start`/`turn_end` carry
        /// none, and neither does any command's reply — so there is nothing to compare a
        /// caller's id against. What the precondition exists to prevent is a steer racing a
        /// turn boundary and landing in the *next* conversation, and the half of that this
        /// vendor can answer is whether a turn is open: a caller that named a turn is
        /// refused when none is running, because the turn it named has ended and pi would
        /// otherwise queue the text for whatever runs next.
        ///
        /// `None` means "whatever is running" and is forwarded as-is, which is a coherent ask
        /// from a client with no turn in view. Stated rather than silently ignored, because a
        /// weaker check presented as the real one is worse than a documented weaker check.
        async fn steer(&self, text: &str, expected_turn_id: Option<&str>) -> Result<()> {
            if expected_turn_id.is_some() && !self.correlation.lock().await.turn_is_open() {
                return Err(anyhow!(
                    "no turn is running, so this steer would be delivered into the next one"
                ));
            }
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

        fn events(&mut self) -> Option<EventStream> {
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

    /// Everything the pump task owns.
    ///
    /// A struct rather than eight arguments because they travel together and four of them
    /// are the same `Arc<Mutex<…>>` shape, where a swapped pair compiles.
    struct Pump {
        lines: mpsc::Receiver<process::Line>,
        messages: mpsc::Sender<Result<SessionMessage>>,
        child: Arc<Child>,
        correlation: Arc<Mutex<translate::Correlation>>,
        dialogs: Dialogs,
        models: Arc<Mutex<Option<Value>>>,
        refreshing: Arc<Mutex<()>>,
    }

    /// Turn pi's lines into session messages, for the session's whole life.
    ///
    /// Ends when the vendor closes stdout or the server drops the session. A **parse failure
    /// is forwarded and does not end the stream**: one malformed line must not fail a turn,
    /// and `sylph_protocol::EventStream` documents an `Err` item as a failure *reading* the
    /// session rather than a failure of it.
    async fn pump(mut state: Pump) {
        let mut last_model: Option<String> = None;
        while let Some(line) = state.lines.recv().await {
            let payload = match line {
                Ok(line) => line.value,
                Err(error) => {
                    if state.messages.send(Err(error)).await.is_err() {
                        break;
                    }
                    continue;
                }
            };
            let at_ms = now_ms();

            match payload.get("type").and_then(Value::as_str) {
                // An `extension_ui_request` is the session half's business, not the
                // projection's: its answer routes back over this process's own stdin.
                Some("extension_ui_request") => {
                    if !handle_ui_request(&state, &payload, at_ms).await {
                        break;
                    }
                    continue;
                }
                // A `response` reaching the pump is one [`reply_id`] declined to correlate,
                // which after that predicate means exactly two things: it carried no `id`, or
                // no waiter held the one it carried. Both are **this adapter's business rather
                // than a fact about the session**, so it is logged and never published —
                // publishing it would put a `set_model` acknowledgement in the transcript as
                // an event.
                //
                // The reachable case is `abort`, which is deliberately sent without an id
                // because its reply is gated on the turn winding down. pi omits `id` from a
                // response when the command carried none, so its acknowledgement lands here by
                // construction rather than by accident.
                Some("response") => {
                    debug!(?payload, "pi answered a request nothing was waiting for");
                    continue;
                }
                _ => {}
            }

            let emission = {
                // Scoped tightly: the lock must not be held across the send below, or
                // backpressure from a slow consumer would block `steer`'s read of
                // `turn_is_open` for as long as the channel stays full.
                let mut correlation = state.correlation.lock().await;
                correlation.translate(&payload, at_ms)
            };

            for message in emission.messages {
                if state.messages.send(Ok(message)).await.is_err() {
                    return;
                }
            }

            // **The vendor reporting a different model is a capability change, so it is
            // published rather than stored.** pi's own TUI can steer a session, and a
            // `/model` typed as prose changes it too, so a client that only ever learns the
            // model from its own `set_config` reply shows a stale one indefinitely — the
            // announce-arrival-without-departure shape, applied to a value rather than to a
            // set.
            if let Some(observed) = emission.observed_model {
                if last_model.as_deref() != Some(observed.as_str()) {
                    last_model = Some(observed);
                    refresh_config(&state);
                }
            }
        }
    }

    /// One `extension_ui_request`, published or answered. `false` means the consumer is gone.
    async fn handle_ui_request(state: &Pump, payload: &Value, at_ms: i64) -> bool {
        match ui::inversion(payload, at_ms) {
            ui::Inversion::Ask(request) => {
                // Remembered **before** the request is published, because the answer can
                // arrive on another task the instant a client sees it — and an offered list
                // recorded afterwards would be missing for exactly the fastest answers.
                //
                // `Some` only for a dialog that genuinely offered options; see [`Dialogs`].
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
            ui::Inversion::Notice(text) => {
                let mut event = Event::new(EventKind::Notice, None, at_ms);
                event.text = text;
                state
                    .messages
                    .send(Ok(SessionMessage::Event(event)))
                    .await
                    .is_ok()
            }
            ui::Inversion::Ignore => true,
            // **Cancelled, never left parked.** An unknown dialog method still blocks pi, so
            // silence stalls the turn until the vendor's own timeout lapses — which presents
            // as a session that stopped for no reason. Reported as a `status` rather than a
            // `notice`, because this is the adapter narrating its own limit rather than the
            // agent saying something, and not as an error, because nothing failed: a build
            // that has not been taught a method is a gap in Sylph, not in the session.
            ui::Inversion::Unsupported { method, vendor_id } => {
                let _ = state.child.send(ui::cancel_response(&vendor_id)).await;
                let mut event = Event::new(EventKind::Status, None, at_ms);
                event.text =
                    format!("pi asked something this build cannot show ({method}); dismissed");
                state
                    .messages
                    .send(Ok(SessionMessage::Event(event)))
                    .await
                    .is_ok()
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
    /// [`PiSession::refreshing`] prevents by holding one lock across read **and** publish.
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
    /// now *enforces* [`translate::Correlation::translate`]'s rule that a `response` and an
    /// `extension_ui_request` are the session half's business: the transport keeps replies off
    /// the projection's input, rather than the split being only documented.
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

        // The discriminant first. A refusal carries `error` and no `data`.
        if reply.get("success").and_then(Value::as_bool) != Some(true) {
            let reason = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("pi refused it and said nothing further");
            return Err(anyhow!("pi refused `{command}`: {reason}"));
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

    /// Epoch milliseconds now.
    ///
    /// **pi's RPC events carry no timestamp at all** — checked against `pi-coding-agent/docs/rpc.md`, where
    /// every event shape is listed and none holds one — so the adapter stamps arrival. That
    /// is why [`translate::Correlation::translate`] takes the clock as an argument: a test
    /// that cannot fix the stamps cannot assert on ordering.
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
        use sylph_protocol::{Phase, RefusalCode};

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
                    let Some(stale) = stale.to_str() else { continue };
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
                    .filter_map(|line| serde_json::from_str(line).ok())
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

        /// The next message off the stream, or `None` if none arrives in `ms`.
        ///
        /// A timeout rather than an unbounded await, so a test asserting that **nothing** is
        /// published can say so — which is what pins "a reply is not an event".
        async fn next_message(stream: &mut EventStream, ms: u64) -> Option<SessionMessage> {
            use futures::StreamExt;
            tokio::time::timeout(Duration::from_millis(ms), stream.next())
                .await
                .ok()
                .flatten()
                .map(|item| item.expect("the scripted peer emits no read failures here"))
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

        /// **A turn's outcome is read from the vendor, never asserted by the adapter.**
        ///
        /// *Measured 2026-08-28 on pi 0.84.3:* the configured model 404'd on every request, and
        /// the observable result was a turn that opened, closed `completed` with no error, and
        /// produced no assistant block — a session that looks like it worked and simply said
        /// nothing. One hardcoded `{"stopReason": "end_turn"}` asserted a clean finish over
        /// every failure, and the Rust port inherited it because a constant compiles and reads
        /// as a default.
        ///
        /// This drives the failure through the whole session half: the peer emits the real
        /// event sequence, and the close must carry the vendor's `error` reason, the provider's
        /// own sentence, and `is_error`.
        #[tokio::test]
        async fn a_vendor_failure_closes_the_turn_as_a_failure_with_its_own_sentence() {
            let sentence =
                "404: Thank you for participating in the Stealth Ox Alpha testing period";
            let events = [
                r#"{"type":"agent_start"}"#,
                r#"{"type":"message_start","message":{"role":"assistant"}}"#,
                &format!(
                    r#"{{"type":"message_end","message":{{"role":"assistant","stopReason":"error","errorMessage":"{sentence}"}}}}"#
                ),
                r#"{"type":"agent_settled"}"#,
            ];
            let (mut session, _log) = scripted("vendor-failure", &events).await;
            let mut stream = session.events().expect("the stream is yielded once");

            let mut closed = None;
            while let Some(message) = next_message(&mut stream, 2000).await {
                if let SessionMessage::Event(event) = message {
                    if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed) {
                        closed = Some(event);
                        break;
                    }
                }
            }
            let closed = closed.expect("the turn must close");
            assert!(
                closed.is_error,
                "`error` is a failure; a hardcoded reason reports it as a clean finish"
            );
            assert_eq!(
                closed
                    .data
                    .as_ref()
                    .and_then(|data| data.get("stopReason"))
                    .and_then(Value::as_str),
                Some("error"),
                "the reason must be the vendor's"
            );
            // Silence is the hardest outcome to debug, so the provider's sentence has to be
            // somewhere a reader will find it.
            assert!(
                closed.text.contains("Stealth Ox Alpha"),
                "the failure must be legible: {closed:?}"
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
                r#"{"type":"extension_error","error":"an extension threw"}"#,
            ];
            let (mut session, _log) = scripted("reply-not-event", &events).await;
            let mut stream = session.events().unwrap();

            // The *first* thing published must be the extension error, not the response above
            // it — which is only observable because the peer emits them in that order.
            let first = next_message(&mut stream, 2000)
                .await
                .expect("the extension error must be published");
            let SessionMessage::Event(event) = first else {
                panic!("expected an event, got {first:?}");
            };
            assert_eq!(event.kind, EventKind::Status);
            assert!(event.is_error);
            assert_eq!(event.text, "an extension threw");
        }

        /// **A malformed line is forwarded and does not end the stream.**
        ///
        /// One bad line must not fail a turn, and it must not be silently dropped either, or
        /// the record cannot explain a gap. pi's own store is truncate-rewritten with no lock,
        /// so a half-written payload is a real state rather than a hypothetical.
        #[tokio::test]
        async fn a_malformed_line_is_reported_and_the_stream_continues() {
            let events = [
                "not json at all",
                r#"{"type":"extension_error","error":"still here"}"#,
            ];
            let (mut session, _log) = scripted("malformed", &events).await;
            let mut stream = session.events().unwrap();

            use futures::StreamExt;
            let first = tokio::time::timeout(Duration::from_millis(2000), stream.next())
                .await
                .unwrap()
                .unwrap();
            assert!(first.is_err(), "the parse failure must be reported");

            let second = next_message(&mut stream, 2000)
                .await
                .expect("the stream must continue past a bad line");
            let SessionMessage::Event(event) = second else {
                panic!("expected an event");
            };
            assert_eq!(event.text, "still here");
        }

        /// **The stream is yielded once**, because two consumers of one vendor's stdio would
        /// each see half the turn. Fan-out is the server's job, which already does it.
        #[tokio::test]
        async fn the_event_stream_is_yielded_exactly_once() {
            let (mut session, _log) = scripted("yield-once", &[]).await;
            assert!(session.events().is_some());
            assert!(
                session.events().is_none(),
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
            let mut stream = session.events().unwrap();

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
        /// — which presents as a session that stopped for no reason. The dismissal is reported
        /// as a `status` and **not** as an error: nothing failed, this build simply has not been
        /// taught the method.
        #[tokio::test]
        async fn an_unknown_dialog_method_is_dismissed_and_narrated() {
            let events = [
                r#"{"type":"extension_ui_request","id":"uuid-9","method":"holograph","title":"?"}"#,
            ];
            let (mut session, log) = scripted("unknown-dialog", &events).await;
            let mut stream = session.events().unwrap();

            let message = next_message(&mut stream, 2000).await.expect("narrated");
            let SessionMessage::Event(event) = message else {
                panic!("an unknown method is narrated, not raised as a request: {message:?}");
            };
            assert_eq!(event.kind, EventKind::Status);
            assert!(!event.is_error, "a gap in Sylph is not a session failure");
            assert!(event.text.contains("holograph"), "{event:?}");

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
            let mut stream = session.events().unwrap();

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

        /// **A steer that names a turn is refused when no turn is running.**
        ///
        /// pi puts no turn id on the wire, so the id itself cannot be compared — what the
        /// precondition prevents is a steer racing a turn boundary and being delivered into the
        /// *next* conversation, and presence is the half this vendor can answer. `None` means
        /// "whatever is running" and is forwarded, which is a coherent ask from a client with no
        /// turn in view.
        #[tokio::test]
        async fn a_steer_naming_a_turn_is_refused_when_none_is_open() {
            let (session, log) = scripted("steer-precondition", &[]).await;

            session
                .steer("keep going", Some("turn-7"))
                .await
                .expect_err("naming a turn that is not running must not be delivered");
            // Forwarded without an expectation, because "whatever is running" is answerable.
            session
                .steer("keep going", None)
                .await
                .expect("an unconditional steer is pi's to accept or refuse");

            let lines = sent(&log, 1).await;
            let steers: Vec<&Value> = lines
                .iter()
                .filter(|line| line.get("type").and_then(Value::as_str) == Some("steer"))
                .collect();
            assert_eq!(
                steers.len(),
                1,
                "exactly the unconditional one reached pi: {lines:?}"
            );
        }

        /// **A steer naming a turn is delivered once a turn is genuinely open.**
        ///
        /// The other half of the test above, and the one that stops the guard from being
        /// satisfied by an implementation that refuses every named steer. The turn state is read
        /// from the shared [`translate::Correlation`] rather than from a copy, which is what
        /// makes one answer to "is a turn open" impossible to disagree with.
        #[tokio::test]
        async fn a_named_steer_is_delivered_while_a_turn_is_open() {
            let (mut session, log) = scripted("steer-open", &[r#"{"type":"agent_start"}"#]).await;
            let mut stream = session.events().unwrap();

            // Drained so the pump has processed `agent_start` before the steer is attempted —
            // otherwise this would pass or fail on scheduling rather than on the guard.
            let opened = next_message(&mut stream, 2000)
                .await
                .expect("the turn must open");
            let SessionMessage::Event(event) = opened else {
                panic!("expected the turn open, got {opened:?}");
            };
            assert_eq!(event.kind, EventKind::Turn);
            assert_eq!(event.phase, Some(Phase::Started));

            session
                .steer("change course", Some("turn-7"))
                .await
                .expect("a turn is open, so the steer belongs to it");
            let lines = sent(&log, 1).await;
            assert_eq!(lines[0].get("type").and_then(Value::as_str), Some("steer"));
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
            let mut stream = session.events().unwrap();

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

        /// **No event this adapter emits carries a `span`.**
        ///
        /// pi reports no delegation on the RPC wire in any form, so a `span` here would be
        /// invented structure the timeline would then render as real
        /// ([`translate::SPAN_HAS_NO_VENDOR_SOURCE`]). Asserted over a whole turn rather than
        /// per event, because the failure would be one branch quietly synthesizing one.
        #[tokio::test]
        async fn nothing_this_adapter_publishes_carries_a_span() {
            let events = [
                r#"{"type":"agent_start"}"#,
                r#"{"type":"message_start","message":{"role":"assistant"}}"#,
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_start","contentIndex":0}}"#,
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"working"}}"#,
                r#"{"type":"tool_execution_start","toolCallId":"call-1","toolName":"bash","args":{"cmd":"ls"}}"#,
                r#"{"type":"tool_execution_end","toolCallId":"call-1","toolName":"bash","result":{"content":"a.txt"}}"#,
                r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop"}}"#,
                r#"{"type":"agent_settled"}"#,
            ];
            let (mut session, _log) = scripted("no-span", &events).await;
            let mut stream = session.events().unwrap();

            let mut seen = 0;
            while let Some(message) = next_message(&mut stream, 2000).await {
                if let SessionMessage::Event(event) = message {
                    seen += 1;
                    assert!(
                        event.span.is_none(),
                        "pi exposes no delegation; this span is invented: {event:?}"
                    );
                    if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed) {
                        break;
                    }
                }
            }
            assert!(
                seen >= 6,
                "the whole turn must have been observed, saw {seen}"
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
            let mut stream = session.events().expect("the stream is yielded once");

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
        /// `dictates_native_id` is true for this agent, so the server always names an id. The
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
            let error = PiSession::open(spec)
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
