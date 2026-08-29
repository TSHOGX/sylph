//! The deterministic E2E agent, ported from
//! `frontend/e2e/fixtures/adapters/e2e/adapter.py` (780 lines).
//!
//! Real agents are non-deterministic and need credentials, so the suite runs against this
//! instead and still exercises the production path end to end: a held-open session, a
//! surface that reports capabilities, streamed events, and requests the UI has to answer.
//!
//! # Every size and every sleep in this file is load-bearing
//!
//! That is the one thing a reader has to know before editing anything here. The Python
//! original's comments were mostly *reasons*, each one a defect this repo has already
//! paid for, and a fixture's shape is part of the test rather than a detail of it. Four
//! rules generalize out of them, and each has a measured instance below:
//!
//! - **A fixture too small cannot observe the defect.** A two-item catalogue is below
//!   every threshold that makes a list scrollable or searchable, so an unscrollable
//!   catalogue shipped against a green suite. One delegate per host cannot exhibit a
//!   multiplexing collapse, because with N=1 the collapse is indistinguishable from
//!   correct.
//! - **A fixture too fast cannot observe streaming.** Every scenario but two finishes
//!   inside one render, which is what let a fully non-streaming client pass. And the
//!   server stamps `created_at_ms` **on arrival**, so the only way to make one delegate
//!   measurably slower than another is to actually take longer.
//! - **A fixture that only reports success cannot observe a failure path.** Hence a
//!   catalogue entry that refuses to be set, a delegate that fails, a tool that fails,
//!   and a delegation left open for the server to close.
//! - **A fixture that reports its whole surface once cannot observe a partial update.**
//!   A per-turn capability update naming one facet is what makes the erasure regression
//!   visible at all.
//!
//! # What the port changed, and what it deliberately did not
//!
//! The protocol boundary is gone: the Python spoke NDJSON on stdio and this implements
//! [`sylph_protocol::Adapter`] directly, so the wire framing, the `hello`/`open`
//! handshake, the reply correlation ids, and the `out_lock` around stdout have no port
//! and are simply absent. What survives unchanged is every observable *shape* — the
//! scenarios, their sizes, their orderings, and their sleeps — because those are what the
//! specs assert on.
//!
//! Two consequences of being in-process rather than a subprocess, both improvements:
//! a refusal is a typed [`sylph_protocol::AdapterError`] rather than a string code, so
//! the `INVALID_VALUE`-versus-`UNSUPPORTED_FIELD` distinction cannot be lost in framing;
//! and a turn runs on a `tokio` task rather than a thread, which is the same property the
//! original needed — the session stays reachable while a turn emits, or a turn could not
//! be interrupted.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use sylph_protocol::{
    Adapter, AdapterError, AgentInfo, Capability, CapabilityFacets, Choice, ConfigChoice,
    ConfigKind, ConfigOption, Event, EventKind, EventStream, Field, FieldKind, Fidelity, FlagSpec,
    Form, NativeId, NativeSummary, Phase, Request, RequestKind, RequestOption, RequestedNativeId,
    Resolution, RunSpec, SelectMode, Session, SessionCapabilities, SessionMessage, StoreFilter,
    SubagentSpec, TurnInput,
};
use tokio::sync::{mpsc, Mutex};

/// The name this adapter answers to.
///
/// Not a `PATH` binary, unlike every other module here: there is no CLI to find, so
/// availability is whether this build has the `e2e` feature — see
/// [`E2eAdapter::is_installed`].
pub const AGENT_NAME: &str = "e2e";

/// The native session id this agent reports when the server does not dictate one.
///
/// Stable rather than minted, so a resumed session in a spec is recognisably the same
/// conversation rather than a new one that happens to look alike.
const DEFAULT_NATIVE_ID: &str = "e2e-native-session";

/// The context window this agent declares, in tokens.
const CONTEXT_WINDOW: i64 = 200_000;

/// The value [`E2eSession::set_config`] refuses, and the reason it exists.
///
/// A refusable value makes the client's *failure* path observable rather than only
/// reasoned about. It is not hypothetical: a real adapter sending the wrong field name to
/// its vendor gets a failed RPC back, and with no error path in the client the rejection
/// was invisible — the value simply did not change, which is indistinguishable from a
/// broken control.
const REFUSED_VALUE: &str = "refused";

/// A long model catalogue: `(value, display name, provider)`.
///
/// **Seventeen entries, and the count is the test.** The threshold between "a menu" and
/// "a searchable list" is a real behaviour and nothing could observe it while every
/// fixture option had two choices — every symptom of a catalogue being unscrollable and
/// unsearchable was invisible to a green suite. Seventeen is well above the client's
/// search threshold of 8 and in the range real agents report: `pi` declares 21 across
/// providers and `claude` around a dozen. The ids are deliberately long enough to be the
/// kind that once widened a `<select>` past the right edge of a phone.
///
/// Descriptions carry the provider, so a spec can assert the filter matches the hint and
/// not only the label.
const CATALOGUE: &[(&str, &str, &str)] = &[
    ("opus-5-1m", "Opus 5 (1M context)", "Anthropic"),
    ("opus-5", "Opus 5", "Anthropic"),
    ("sonnet-5", "Sonnet 5", "Anthropic"),
    ("haiku-4-5", "Haiku 4.5", "Anthropic"),
    ("fable-5", "Fable 5", "Anthropic"),
    ("gpt-5-codex-preview", "GPT-5 Codex Preview", "OpenAI"),
    ("gpt-5-mini", "GPT-5 Mini", "OpenAI"),
    ("o5-reasoning", "o5 Reasoning", "OpenAI"),
    ("kimi-k2-thinking", "Kimi K2 Thinking", "Moonshot"),
    ("kimi-k2", "Kimi K2", "Moonshot"),
    ("llama-4-405b-instruct", "Llama 4 405B Instruct", "Meta"),
    ("mistral-large-3", "Mistral Large 3", "Mistral"),
    ("qwen-3-max", "Qwen 3 Max", "Alibaba"),
    ("deepseek-v4", "DeepSeek V4", "DeepSeek"),
    // Two providers offering one model under one display name, which is a **measured**
    // shape rather than an invented one: pi 0.84.3 offers two "Claude Opus 5" (anthropic
    // direct and via openrouter) and two each of "DeepSeek V4 Pro" and "GLM-5.2". The
    // reported bug was both rows showing a checkmark, because a producer's
    // `checked: value == current_value` is a *filter* and the bare id was not unique —
    // and the bare id also made one of each pair unreachable, since a by-id lookup can
    // only answer with one provider.
    //
    // Values are provider-qualified for the same reason `pi`'s now are, and delimited
    // with a **space** rather than a `/`: 350 of pi's 372 ids already contain a slash, so
    // splitting on one mis-parses the majority of the catalogue. The pair exists so a
    // spec can prove exactly one row reads as current and that each provider is
    // separately reachable.
    ("ikun glm-5.2", "GLM-5.2", "ikun-glm"),
    ("huaweicloud glm-5.2", "GLM-5.2", "huaweicloud"),
    // Refused by `set_config`, so the client's failure path is observable.
    (REFUSED_VALUE, "Unavailable Model", "Refuses to be set"),
];

/// The commands this agent reports.
///
/// **Deliberately a mix of prompt and control kinds.** The UI's routing between "send this
/// as a turn" and "open a picker" is a real behaviour, and a fixture reporting only
/// prompts could not exercise it.
fn commands() -> Vec<sylph_protocol::CommandSpec> {
    use sylph_protocol::{CommandKind, CommandSpec};
    let spec = |name: &str, description: &str, kind: CommandKind| CommandSpec {
        name: name.to_string(),
        description: description.to_string(),
        arg_hint: None,
        source: Some("builtin".to_string()),
        kind,
        config_id: None,
        aliases: Vec::new(),
    };
    vec![
        spec("status", "Report runner status", CommandKind::Prompt),
        CommandSpec {
            arg_hint: Some("[target]".to_string()),
            source: Some("skill".to_string()),
            ..spec("verify", "Verify a change end to end", CommandKind::Prompt)
        },
        CommandSpec {
            aliases: vec!["cost".to_string(), "stats".to_string()],
            ..spec("usage", "Show session cost", CommandKind::Prompt)
        },
        CommandSpec {
            config_id: Some("model".to_string()),
            ..spec("model", "Choose the model", CommandKind::Control)
        },
        // A control command onto the *long* catalogue. The dialog a control command opens
        // reaches the filterable list through a different surface than the composer's
        // popover does — an overlay owning its own dismissal — so without this the
        // dialog's search path had no coverage at all.
        CommandSpec {
            config_id: Some("provider".to_string()),
            ..spec("provider", "Choose the provider model", CommandKind::Control)
        },
    ]
}

/// The live config surface.
///
/// Three options covering both ARIA contracts on one screen, which is why the short and
/// long selects share a `category`: below the search threshold a config option renders as
/// a menu of radio items, and above it as a combobox over a listbox. That is a different
/// contract rather than merely a different width, so a fixture declaring only one size
/// leaves the other untested.
fn initial_config() -> Vec<ConfigOption> {
    let choice = |value: &str, name: &str, description: Option<&str>| ConfigChoice {
        value: value.to_string(),
        name: name.to_string(),
        description: description.map(str::to_string),
    };
    vec![
        // Two choices, so the menu pattern stays covered.
        ConfigOption {
            id: "model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: ConfigKind::Select,
            current_value: Some(json!("fast")),
            options: vec![
                choice("fast", "Fast", None),
                choice("thorough", "Thorough", None),
            ],
        },
        // Above the threshold, and in the same category so it lands on the composer's
        // control row beside the short one — which is what lets one screen assert both.
        ConfigOption {
            id: "provider".to_string(),
            name: "Provider model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: ConfigKind::Select,
            current_value: Some(json!("sonnet-5")),
            options: CATALOGUE
                .iter()
                .map(|(value, name, provider)| choice(value, name, Some(provider)))
                .collect(),
        },
        ConfigOption {
            id: "verbose".to_string(),
            name: "Verbose".to_string(),
            description: None,
            category: Some("output".to_string()),
            kind: ConfigKind::Boolean,
            current_value: Some(json!(false)),
            options: Vec::new(),
        },
    ]
}

/// The whole surface this agent reports on open.
fn initial_surface() -> SessionCapabilities {
    SessionCapabilities {
        commands: commands(),
        subagents: vec![SubagentSpec {
            name: "Explore".to_string(),
            description: "Read-only search".to_string(),
        }],
        config: initial_config(),
        permission_mode: Some("default".to_string()),
        context_window: Some(CONTEXT_WINDOW),
    }
}

/// The deterministic E2E agent.
///
/// Behind the `e2e` feature because it is a test fixture that must never be reachable
/// from a production build: `registry()` offering it would make "which agents exist"
/// depend on how the server was compiled, and a fixture that can be discovered by
/// accident is the failure `docs/session-protocol.md` § What changed records twice.
#[derive(Debug, Clone, Default)]
pub struct E2eAdapter;

impl E2eAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Always true, and that is not a stub.
    ///
    /// Every other module answers this by looking for a vendor binary on `PATH`. There is
    /// no binary here — the agent *is* this code — so the honest answer is that it is
    /// installed exactly when it is compiled in, which the `e2e` feature already decides.
    /// Returning `false` for some further condition would give the suite a second way to
    /// find zero agents, which is the total, silent failure this whole design removes.
    pub fn is_installed() -> bool {
        true
    }

    /// The spawn-time options this agent declares.
    ///
    /// Long for the same reason [`CATALOGUE`] is: the settings panel's per-agent default is
    /// a *second* surface with the same scroll-and-filter problem, and a two-item list
    /// could not observe it.
    fn flags() -> sylph_protocol::FlagSchema {
        let mut flags = sylph_protocol::FlagSchema::new();
        let mut choices = vec!["fast".to_string(), "thorough".to_string()];
        choices.extend(CATALOGUE.iter().map(|(value, _, _)| value.to_string()));
        flags.insert(
            "model".to_string(),
            FlagSpec {
                choices,
                description: "Model tier".to_string(),
            },
        );
        flags
    }
}

#[async_trait]
impl Adapter for E2eAdapter {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(AgentInfo {
            name: AGENT_NAME.to_string(),
            // A fixed version rather than a crate version: a spec asserting on what the
            // agent reports must not change when this crate's version does.
            version: Some("3.0.0".to_string()),
            flags: Self::flags(),
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
                // Both, so a spec can observe a client that conflates them. This fixture is
                // the only agent offering `Steer` and `Queue` together besides `pi`, and it
                // is the one the suite actually drives.
                Capability::Queue,
                Capability::SetConfig,
            ],
            // The server names the id, so identity is known before the first event —
            // the same path `claude` and `pi` take, which is what keeps the suite on
            // the production code path rather than on the fallback one.
            dictates_native_id: true,
            // Irrelevant in practice, since this agent writes no native store, and
            // declared honestly rather than left at a default: `run` is always
            // `Stream`, and this field describes `adopt` and `watch`, which below
            // refuse rather than pretend.
            native_fidelity: Fidelity::Blocks,
        })
    }

    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
        let native_id = match &spec.native_id {
            RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => id.clone(),
            // Honoured rather than refused: `dictates_native_id` is true so the server
            // always names one, but a fixture that panics on a protocol value is a
            // fixture that fails the suite for a reason no spec is about.
            RequestedNativeId::New => NativeId::new(DEFAULT_NATIVE_ID),
        };
        Ok(Box::new(E2eSession::new(native_id)))
    }

    /// Refused, and the refusal is the honest answer.
    ///
    /// This agent has no native store: there is no vendor writing files for it, so there
    /// is nothing to parse. Synthesizing a store would make the suite assert against
    /// records Sylph itself wrote, which tests the fixture rather than the parser — see
    /// the module test `adopt_and_watch_refuse_because_there_is_no_native_store` for why
    /// that is stated as a property rather than left implicit.
    async fn adopt(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!(
            "the e2e agent has no native store to adopt: it is an in-process fixture, so \
             its only fidelity is the live stream"
        ))
    }

    async fn watch(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!(
            "the e2e agent has no native store to watch: it is an in-process fixture, so \
             its only fidelity is the live stream"
        ))
    }

    /// Empty, which is a real state rather than a failure.
    ///
    /// A machine with this agent and no native sessions genuinely has none to list, and
    /// that is exactly what a store-less agent always reports.
    async fn enumerate(&self, _filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(Vec::new())
    }
}

/// One live E2E session.
///
/// # Why the turn runs on its own task
///
/// The Python ran each turn on a thread, and the reason was in its comment: a turn that
/// blocked the read loop could not be interrupted, and interrupt is one of the things the
/// suite tests. The same property is needed here — [`Session::send`] returns when the turn
/// is *accepted*, not when it finishes, exactly as `POST /turns` answers `202` — so a turn
/// is spawned and its events reach the stream through a channel.
struct E2eSession {
    native_id: NativeId,
    /// The events this session has produced, as the server reads them.
    ///
    /// An `Option` because [`Session::events`] yields the stream **once**: a second call is
    /// a bug rather than a second subscription, since two consumers of one session would
    /// each see half the turn.
    stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    emit: Emitter,
    /// The surface, mutated by `set_config` and read back by `capabilities`.
    ///
    /// **Read from here rather than from what was last requested.** `pi`'s adapter
    /// reported a field only one code path populated, so any surface built by another path
    /// reported nothing while the session plainly had a model, and the chip fell back to
    /// "agent default" with the checkmark on no row.
    surface: Arc<Mutex<SessionCapabilities>>,
    /// How many turns have run, which is what makes a request id unique per turn.
    turns: Arc<Mutex<u64>>,
}

/// The one way an event reaches the server, so no scenario can bypass the ordering.
///
/// Cloneable and held by each turn task. A closed receiver — the server dropped the
/// session mid-turn — is not an error worth reporting: the scenario simply stops being
/// observed, which is what a detached link means.
#[derive(Clone)]
struct Emitter(mpsc::Sender<Result<SessionMessage>>);

impl Emitter {
    /// Emit one event, with `created_at_ms` stamped now.
    ///
    /// The server re-stamps arrival time, so this value is not what a duration is computed
    /// from — which is precisely why [`Scenario::Fanout`] has to *sleep* rather than
    /// declare timestamps. Stamped anyway because an adapter that leaves it zero is an
    /// adapter reporting 1970.
    async fn event(&self, event: Event) {
        let _ = self.0.send(Ok(SessionMessage::Event(event))).await;
    }

    async fn request(&self, request: Request) {
        let _ = self.0.send(Ok(SessionMessage::Request(request))).await;
    }

    async fn capabilities(&self, facets: CapabilityFacets) {
        let _ = self.0.send(Ok(SessionMessage::Capabilities(facets))).await;
    }
}

/// Build one event, with only the fields a scenario names.
///
/// A builder rather than struct literals at ~90 call sites, and the shape mirrors the
/// Python `event()` helper for the same reason: every optional field defaults to absent,
/// so **a name this fixture does not know stays absent rather than becoming a
/// placeholder**. A journal makes every guess permanent, and `None` is the only value that
/// stays honest after the fact.
struct Ev(Event);

impl Ev {
    fn new(kind: EventKind, phase: Option<Phase>) -> Self {
        Self(Event::new(kind, phase, now_ms()))
    }

    fn stream_id(mut self, id: &str) -> Self {
        self.0.stream_id = Some(id.to_string());
        self
    }

    fn span(mut self, span: &str) -> Self {
        self.0.span = Some(span.to_string());
        self
    }

    fn name(mut self, name: &str) -> Self {
        self.0.name = Some(name.to_string());
        self
    }

    fn text(mut self, text: impl Into<String>) -> Self {
        self.0.text = text.into();
        self
    }

    fn data(mut self, data: Value) -> Self {
        self.0.data = Some(data);
        self
    }

    /// Mark this event a failure.
    ///
    /// Named `failed` rather than `is_error` because it *sets* rather than asks — an
    /// `is_*` method that takes `self` by value and returns a builder reads as a
    /// predicate at every call site, which is what clippy's `wrong_self_convention`
    /// is about.
    fn failed(mut self) -> Self {
        self.0.is_error = true;
        self
    }

    fn done(self) -> Event {
        self.0
    }
}

/// Epoch milliseconds now.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

impl E2eSession {
    fn new(native_id: NativeId) -> Self {
        // Bounded, and generously: `bulk` emits 1000 events as fast as it can, and a
        // channel small enough to block there would make the fixture's own backpressure
        // part of what a render-cost measurement measures.
        let (tx, rx) = mpsc::channel(4096);
        Self {
            native_id,
            stream: Mutex::new(Some(rx)),
            emit: Emitter(tx),
            surface: Arc::new(Mutex::new(initial_surface())),
            turns: Arc::new(Mutex::new(0)),
        }
    }
}

#[async_trait]
impl Session for E2eSession {
    fn native_id(&self) -> &NativeId {
        &self.native_id
    }

    async fn capabilities(&self) -> Result<SessionCapabilities> {
        Ok(self.surface.lock().await.clone())
    }

    /// Accept a turn and run it on its own task.
    ///
    /// Returns once accepted, which is the contract: a turn outlives the call that started
    /// it and its completion arrives on [`Session::events`].
    async fn send(&self, turn: TurnInput) -> Result<()> {
        // Per-turn config overrides are applied before the turn runs, which is the reason
        // they travel *with* the turn rather than as preceding calls — those were extra
        // round trips with a window in between where a turn could run under a mix of old
        // and new settings.
        {
            let mut surface = self.surface.lock().await;
            for (id, value) in &turn.config {
                if let Some(option) = surface.config.iter_mut().find(|o| &o.id == id) {
                    option.current_value = Some(value.clone());
                }
            }
        }

        // A command becomes the prompt text, so scenario selection sees `/verify target`
        // exactly as the Python's `serve` loop composed it — which is what lets a spec
        // drive a scenario through the slash menu rather than only through the composer.
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

        let turn_number = {
            let mut turns = self.turns.lock().await;
            *turns += 1;
            *turns
        };

        let emit = self.emit.clone();
        let surface = self.surface.clone();
        tokio::spawn(async move {
            run_turn(emit, surface, text, turn_number).await;
        });
        Ok(())
    }

    /// End the turn, and say so on the stream.
    ///
    /// The terminal event is what a client is waiting for; an `Ok(())` alone would leave
    /// the composer showing Stop forever.
    async fn interrupt(&self) -> Result<()> {
        self.emit
            .event(
                Ev::new(EventKind::Turn, Some(Phase::Completed))
                    .data(json!({"stopReason": "interrupted"}))
                    .done(),
            )
            .await;
        Ok(())
    }

    async fn steer(&self, text: &str, _expected_turn_id: Option<&str>) -> Result<()> {
        self.emit
            .event(
                Ev::new(EventKind::Assistant, Some(Phase::Completed))
                    .stream_id("blk-steered")
                    .text(format!("Steered: {text}"))
                    .done(),
            )
            .await;
        Ok(())
    }

    /// Queue text, observably.
    ///
    /// Emits a distinct block rather than reusing the steered one, so a spec can tell the two
    /// apart — the whole reason `Queue` is a separate capability is that a client renders a
    /// correction and a follow-up differently, and a fixture that answered both the same way
    /// could not observe a client conflating them.
    async fn queue(&self, text: &str) -> Result<()> {
        self.emit
            .event(
                Ev::new(EventKind::Assistant, Some(Phase::Completed))
                    .stream_id("blk-queued")
                    .text(format!("Queued: {text}"))
                    .done(),
            )
            .await;
        Ok(())
    }

    /// Set one option, and answer with the **refreshed array**.
    ///
    /// Not an acknowledgement, because setting one option can change another's choices.
    ///
    /// # The two refusal codes are not interchangeable
    ///
    /// An unknown id is [`sylph_protocol::RefusalCode::UnsupportedField`] — no such option,
    /// which justifies disabling a control. The [`REFUSED_VALUE`] is
    /// [`sylph_protocol::RefusalCode::InvalidValue`] — this value, right now, which must
    /// leave the control usable.
    ///
    /// **The earlier fixture used the wrong one of these**, so the suite exercised "no such
    /// option" while claiming to cover a refused *value*. The server classifies on the
    /// code, so the branch that keeps a control usable had no end-to-end coverage at all.
    async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
        let mut surface = self.surface.lock().await;
        let Some(option) = surface.config.iter_mut().find(|o| o.id == id) else {
            return Err(anyhow::Error::new(AdapterError::unsupported_field(format!(
                "no such config option: {id}"
            ))));
        };
        if value.as_str() == Some(REFUSED_VALUE) {
            return Err(anyhow::Error::new(AdapterError::invalid_value(
                "this agent refuses that value",
            )));
        }
        option.current_value = Some(value);
        Ok(surface.config.clone())
    }

    /// Answer a pending request, resuming the turn it blocked.
    ///
    /// **The received input is echoed into the tool result verbatim**, so a spec can assert
    /// what the *agent* got rather than only that the panel closed. That distinction is the
    /// whole lesson of the bug this path exists for: the answer was being written into a
    /// vendor field that does not exist, so the panel closed, the turn resumed, and the
    /// answer had been dropped — with nothing erroring at any layer.
    async fn resolve(&self, resolution: Resolution) -> Result<()> {
        let declined = matches!(resolution.option.as_str(), "deny" | "cancel");
        let text = if declined {
            "Declined by the user.".to_string()
        } else {
            // Sorted keys, so an assertion on this string is not a bet on map ordering.
            let input = resolution.input.clone().unwrap_or(Value::Null);
            format!("Received: {}", canonical_json(&input))
        };

        let mut result = Ev::new(EventKind::ToolResult, Some(Phase::Completed))
            .stream_id("blk-approved")
            .name("Bash")
            .text(text);
        // A denial is still an answer, and it is an error only when it is a *refusal* —
        // `cancel` on an elicitation is a withdrawal, not a failure.
        if resolution.option == "deny" {
            result = result.failed();
        }
        self.emit.event(result.done()).await;
        reply(&self.emit, false).await;
        settle(&self.emit).await;
        Ok(())
    }

    fn events(&mut self) -> Option<EventStream> {
        // `try_lock` because this is the sync method on an otherwise async trait, and the
        // lock is uncontended by construction: `&mut self` means nothing else holds this
        // session. Taking the receiver is what makes a second call answer `None`.
        let receiver = self.stream.try_lock().ok()?.take()?;
        Some(Box::pin(
            tokio_stream_receiver(receiver),
        ))
    }

    /// Idempotent, and there is nothing to signal.
    ///
    /// No process, so no escalation: the session simply stops producing. Dropping the
    /// emitter is what ends the stream for whoever is reading it.
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// Serialize with sorted keys, so an echoed answer is byte-stable.
///
/// `serde_json` sorts object keys only under its `preserve_order` feature being *off*,
/// which is the default here — but a `Value` built by a client may already hold an
/// arbitrary order, so this rebuilds it rather than relying on that.
fn canonical_json(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: std::collections::BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
                json!(ordered)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sorted(value)).unwrap_or_else(|_| "null".to_string())
}

/// A receiver as a `Stream`, without pulling in `tokio-stream`.
///
/// One `unfold` rather than a dependency, because this is the only place in the crate that
/// needs the conversion.
fn tokio_stream_receiver(
    receiver: mpsc::Receiver<Result<SessionMessage>>,
) -> impl futures::Stream<Item = Result<SessionMessage>> + Send {
    futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

/// Which scenario a prompt selects.
///
/// **Selected by substring**, as the Python was, so one fixture serves the whole suite and
/// a spec's prompt reads as English. An enum rather than a chain of `if text.contains`
/// because the fallthrough is a real case with a name — a streamed reply — and a chain
/// leaves it as an anonymous `else`.
///
/// Order matters only in that the first match wins; the tokens are disjoint in practice
/// because each spec writes its own prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Bulk,
    Tools,
    Approval,
    Narrow,
    Question,
    Elicit,
    Delegate,
    Unprompted,
    Promote,
    Paced,
    Fanout,
    /// Anything else: a streamed reply, which is what most specs need from a turn.
    Reply,
}

impl Scenario {
    /// The token each scenario answers to, in match order.
    const TOKENS: &'static [(&'static str, Scenario)] = &[
        ("bulk", Scenario::Bulk),
        ("tools", Scenario::Tools),
        ("approval", Scenario::Approval),
        ("narrow", Scenario::Narrow),
        ("question", Scenario::Question),
        ("elicit", Scenario::Elicit),
        ("delegate", Scenario::Delegate),
        ("unprompted", Scenario::Unprompted),
        ("promote", Scenario::Promote),
        ("paced", Scenario::Paced),
        ("fanout", Scenario::Fanout),
    ];

    fn of(text: &str) -> Self {
        Self::TOKENS
            .iter()
            .find(|(token, _)| text.contains(token))
            .map(|(_, scenario)| *scenario)
            .unwrap_or(Scenario::Reply)
    }

    /// Whether this scenario leaves the turn **blocked on a human** rather than settling.
    ///
    /// The turn resumes from [`Session::resolve`], which is what lets a spec assert that
    /// answering is what unblocks it. Stated as a property of the scenario rather than as
    /// an early `return` in each arm so the settle path below has one condition to read.
    fn blocks_on_a_request(self) -> bool {
        matches!(
            self,
            Scenario::Approval | Scenario::Narrow | Scenario::Question | Scenario::Elicit
        )
    }
}

/// One turn's worth of events, then a terminal `turn` event.
async fn run_turn(emit: Emitter, surface: Arc<Mutex<SessionCapabilities>>, text: String, turn: u64) {
    let scenario = Scenario::of(&text);

    emit.event(
        Ev::new(EventKind::Status, Some(Phase::Started))
            .text("starting")
            .done(),
    )
    .await;

    // A per-turn capability update naming **one facet**, which is what `claude` actually
    // does: its `system/init` arrives once per *turn* and carries only the permission mode.
    //
    // This line is why the suite can observe the regression at all. A partial update
    // erasing the facets it never mentioned is what stopped the slash menu opening after
    // the first exchange — 46 project commands gone on the first exchange of every session
    // — and no spec could see it while the fixture only ever reported its surface once, at
    // open. An unmentioned facet must not be cleared.
    emit.capabilities(CapabilityFacets {
        permission_mode: Some("default".to_string()),
        ..Default::default()
    })
    .await;

    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("blk-think")
            .text("Considering the request.")
            .done(),
    )
    .await;

    match scenario {
        Scenario::Bulk => bulk(&emit).await,
        Scenario::Tools => tools(&emit).await,
        Scenario::Approval => approval(&emit, turn).await,
        Scenario::Narrow => narrow(&emit, turn).await,
        Scenario::Question => question(&emit, turn).await,
        Scenario::Elicit => elicit(&emit, turn).await,
        Scenario::Delegate => delegate(&emit).await,
        Scenario::Unprompted => unprompted(&emit).await,
        Scenario::Promote => promote(&emit).await,
        Scenario::Paced => paced(&emit).await,
        Scenario::Fanout => fanout(&emit).await,
        Scenario::Reply => reply(&emit, true).await,
    }

    // A blocked scenario has emitted a request and nothing else: the turn is live, and
    // `resolve` is what settles it.
    if scenario.blocks_on_a_request() {
        return;
    }

    settle(&emit).await;

    // Everything below happens with the turn **already settled**, which is the point.
    if scenario == Scenario::Unprompted {
        unprompted_after_settle(&emit).await;
    }

    // Unused today and held deliberately: `surface` is what a scenario would read to
    // report a config-dependent reply, and dropping the parameter would mean threading it
    // back through `send` when one does.
    drop(surface);
}

/// One tool call and the result that answers it.
///
/// A struct rather than eight positional arguments, which is what this was: `(emit,
/// stream_id, name, input, result, is_error, data, span)` has two `&str` in a row and two
/// `Option`s in a row, so swapping either pair compiles. `Default` is what keeps the common
/// case short — most calls name three fields and leave the rest absent.
#[derive(Default)]
struct ToolPair<'a> {
    stream_id: &'a str,
    name: &'a str,
    /// The tool's input, serialized as the call's text and parsed in its `data`.
    input: Value,
    /// What the tool answered.
    result: &'a str,
    is_error: bool,
    data: Option<Value>,
    /// The trajectory this call happened in, or `None` for the root turn.
    span: Option<&'a str>,
}

/// A tool call and its result, as one pair.
///
/// The exit code rides on the **result**, which is where the timeline reads it: "failed"
/// alone is not what anyone wants from a failed command.
///
/// The result carries the same `span` as its call, because a delegate's tool output belongs
/// to the delegate — rooting it would show a subagent's failure as the parent's own.
async fn tool(emit: &Emitter, call: ToolPair<'_>) {
    let ToolPair {
        stream_id,
        name,
        input,
        result,
        is_error,
        data,
        span,
    } = call;
    let mut call = Ev::new(EventKind::ToolCall, Some(Phase::Completed))
        .stream_id(stream_id)
        .name(name)
        // The serialized input as the call's text, matching what a vendor streams as
        // partial JSON, with the parsed form beside it in `data`.
        .text(serde_json::to_string(&input).unwrap_or_default())
        .data(json!({"input": input}));
    if let Some(span) = span {
        call = call.span(span);
    }
    emit.event(call.done()).await;

    let mut done = Ev::new(EventKind::ToolResult, Some(Phase::Completed))
        .stream_id(stream_id)
        .name(name)
        .text(result);
    if let Some(span) = span {
        done = done.span(span);
    }
    if is_error {
        done = done.failed();
    }
    if let Some(data) = data {
        done = done.data(data);
    }
    emit.event(done.done()).await;
}

/// The assistant's reply, streamed or not.
///
/// `stream` is what the `paced` and default scenarios use: `started`, two deltas, then the
/// `completed` phase carrying the whole text. A scenario that only emits `completed` is
/// asserting on journaled state rather than on streaming.
async fn reply(emit: &Emitter, stream: bool) {
    const WHOLE: &str = "Hello from the E2E runner.";
    if stream {
        emit.event(
            Ev::new(EventKind::Assistant, Some(Phase::Started))
                .stream_id("blk-reply")
                .done(),
        )
        .await;
        for part in ["Hello from ", "the E2E runner."] {
            emit.event(
                Ev::new(EventKind::Assistant, Some(Phase::Delta))
                    .stream_id("blk-reply")
                    .text(part)
                    .done(),
            )
            .await;
        }
    }
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("blk-reply")
            .text(WHOLE)
            .done(),
    )
    .await;
}

/// Usage, then the terminal `turn` event.
async fn settle(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Usage, Some(Phase::Completed))
            .stream_id("usage")
            .data(json!({"inputTokens": 12, "outputTokens": 7}))
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Turn, Some(Phase::Completed))
            .text("Hello from the E2E runner.")
            .data(json!({"stopReason": "end_turn"}))
            .done(),
    )
    .await;
}

/// A long transcript, for measuring render cost.
///
/// **Completed blocks only**, so every one is journalled and survives a reload — which is
/// what makes the measurement reproducible rather than dependent on stream timing. 250
/// iterations, each contributing an assistant block, a tool pair, and a thinking block.
async fn bulk(emit: &Emitter) {
    for i in 1..=250 {
        emit.event(
            Ev::new(EventKind::Assistant, Some(Phase::Completed))
                .stream_id(&format!("blk-a{i}"))
                .text(format!("Step {i}: inspecting the module and its callers."))
                .done(),
        )
        .await;
        tool(
            emit,
            ToolPair {
                stream_id: &format!("blk-t{i}"),
                name: "Read",
                input: json!({"file_path": format!("src/mod_{i}.rs")}),
                result: &format!("pub fn helper_{i}() -> usize {{ {i} }}"),
                ..Default::default()
            },
        )
        .await;
        emit.event(
            Ev::new(EventKind::Thinking, Some(Phase::Completed))
                .stream_id(&format!("blk-th{i}"))
                .text(format!("Module {i} looks consistent with the others."))
                .done(),
        )
        .await;
    }
    reply(emit, false).await;
}

/// A read, an edit carrying a real diff, and a failing command.
///
/// The three renderings whose expansion policy differs, which is why all three are here
/// rather than one standing in for the others.
async fn tools(emit: &Emitter) {
    tool(
        emit,
        ToolPair {
            stream_id: "blk-read",
            name: "Read",
            input: json!({"file_path": "src/lib.rs"}),
            result: "fn main() {\n    println!(\"before\");\n}",
            ..Default::default()
        },
    )
    .await;
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("blk-plan")
            .text("I will replace the placeholder with **real** logging.")
            .done(),
    )
    .await;
    tool(
        emit,
        ToolPair {
            stream_id: "blk-edit",
            name: "Edit",
            input: json!({
            "file_path": "src/lib.rs",
            "old_string": "fn main() {\n    println!(\"before\");\n}",
            "new_string": "fn main() {\n    tracing::info!(\"after\");\n}",
        }),
            result: "Applied 1 edit.",
            ..Default::default()
        },
    )
    .await;
    tool(
        emit,
        ToolPair {
            stream_id: "blk-bash",
            name: "Bash",
            input: json!({"command": "cargo test --lib"}),
            result: "error[E0425]: cannot find value `after` in this scope",
            is_error: true,
            data: Some(json!({"exitCode": 101})),
            ..Default::default()
        },
    )
    .await;
    reply(emit, false).await;
}

/// An approval that blocks the turn until answered.
///
/// The inversion v1 could not express, and the reason the fixture had to become a duplex
/// adapter at all: a request the agent is *waiting* on. Left pending on purpose — the turn
/// resumes from `resolve`, which is what lets a spec assert that answering is what unblocks
/// it.
async fn approval(emit: &Emitter, turn: u64) {
    emit.request(Request {
        id: format!("req-{turn}"),
        kind: RequestKind::Approval,
        tool_use_id: Some("toolu_e2e".to_string()),
        tool: Some("Bash".to_string()),
        title: "Run a shell command".to_string(),
        detail: Some(json!({"command": "rm -rf build"})),
        options: vec![
            option("allow", "Allow once", false),
            // The one persistent option in the suite, so a client can be asserted to mark
            // a lasting decision differently from a one-off.
            option("allow_always", "Always allow Bash", true),
            option("deny", "Deny", false),
        ],
        // No form: an approval's subject **is** its `detail`, which is the one kind that
        // legitimately declares none.
        form: None,
        default: Some("deny".to_string()),
        risk: Some(sylph_protocol::Risk::Destructive),
        allow_persistent: true,
        expires_at_ms: None,
    })
    .await;
}

/// An approval that forbids a lasting grant.
///
/// `allow_persistent: false` means a client must omit the "always allow" affordance
/// **entirely**, not merely de-emphasize it: `claude` sets it on asks where accepting
/// always would write a permission rule *broader than the ask itself*, so offering it turns
/// a narrow yes into a wide one the user never read.
async fn narrow(emit: &Emitter, turn: u64) {
    emit.request(Request {
        id: format!("req-{turn}"),
        kind: RequestKind::Approval,
        tool_use_id: None,
        tool: Some("Write".to_string()),
        title: "Write to a sensitive path".to_string(),
        detail: Some(json!({"file_path": "/etc/hosts"})),
        options: vec![
            option("allow", "Allow once", false),
            option("deny", "Deny", false),
        ],
        form: None,
        default: Some("deny".to_string()),
        risk: Some(sylph_protocol::Risk::Destructive),
        allow_persistent: false,
        expires_at_ms: None,
    })
    .await;
}

/// One request option: a **verb, not data**.
///
/// `Choice` previously overloaded `options` as both the answer and the submit button, which
/// is why picking had to submit immediately and why a note had nowhere to live. Data belongs
/// in the [`Form`].
fn option(id: &str, label: &str, persistent: bool) -> RequestOption {
    RequestOption {
        id: id.to_string(),
        label: label.to_string(),
        persistent,
        description: None,
    }
}

/// A declared form, which is the shape an agent's own question takes: the **fields carry
/// the answers and the options are verbs**.
///
/// **Two fields, because one could not observe that two radio groups stay independent** — a
/// single shared `name` would make answering the second clear the first.
///
/// `allow_other` and `notes` are both on the first field on purpose: they are the two
/// affordances whose absence was the reported bug, and neither has any coverage unless a
/// fixture declares them. A client that cannot send an "Other" answer drops what the user
/// typed, and `claude`'s question dialog always offers that row.
///
/// The second field is `mode: Many` **and** `required`, so a spec can observe that a
/// multi-select with nothing picked refuses to submit.
async fn question(emit: &Emitter, turn: u64) {
    emit.request(Request {
        id: format!("req-{turn}"),
        kind: RequestKind::Choice,
        tool_use_id: None,
        tool: Some("AskUserQuestion".to_string()),
        title: "Two questions".to_string(),
        detail: Some(json!({})),
        options: vec![
            option("submit", "Send answer", false),
            option("deny", "Don't answer", false),
        ],
        form: Some(Form {
            fields: vec![
                Field {
                    id: "q:0".to_string(),
                    kind: FieldKind::Select {
                        mode: SelectMode::One,
                        choices: vec![
                            choice("Tabs", "Indent with tabs."),
                            choice("Spaces", "Indent with spaces."),
                        ],
                        allow_other: true,
                        other_label: Some("Something else".to_string()),
                        notes: Some(sylph_protocol::NotesSpec {
                            label: Some("Notes (optional)".to_string()),
                            placeholder: None,
                        }),
                    },
                    label: "Indentation?".to_string(),
                    description: None,
                    required: false,
                },
                Field {
                    id: "q:1".to_string(),
                    kind: FieldKind::Select {
                        mode: SelectMode::Many,
                        choices: ["ESLint", "Prettier", "Clippy"]
                            .iter()
                            .map(|value| Choice {
                                value: value.to_string(),
                                label: None,
                                description: None,
                            })
                            .collect(),
                        allow_other: false,
                        other_label: None,
                        notes: None,
                    },
                    label: "Linters?".to_string(),
                    description: None,
                    required: true,
                },
            ],
        }),
        default: Some("deny".to_string()),
        risk: Some(sylph_protocol::Risk::Safe),
        allow_persistent: true,
        expires_at_ms: None,
    })
    .await;
}

/// A text form carrying a prefill.
///
/// The kind whose entire purpose is free text and which, before a form could be *declared*,
/// rendered as a read-only dump of its own placeholder — because the client was sniffing
/// `detail` for field names it recognized, and an elicitation carries none of them.
///
/// The prefill is not decoration: `pi` sends one for its `editor` dialog, where discarding
/// it would lose the draft being revised.
async fn elicit(emit: &Emitter, turn: u64) {
    emit.request(Request {
        id: format!("req-{turn}"),
        kind: RequestKind::Elicitation,
        tool_use_id: None,
        tool: None,
        title: "Describe the change".to_string(),
        detail: Some(json!({})),
        options: vec![
            option("submit", "Submit", false),
            option("cancel", "Cancel", false),
        ],
        form: Some(Form {
            fields: vec![Field {
                id: "text".to_string(),
                kind: FieldKind::Text {
                    multiline: true,
                    prefill: Some("Refactor the ".to_string()),
                    placeholder: Some("One sentence".to_string()),
                },
                label: "Summary".to_string(),
                description: None,
                required: true,
            }],
        }),
        default: Some("cancel".to_string()),
        risk: None,
        allow_persistent: true,
        expires_at_ms: None,
    })
    .await;
}

/// One offered answer, with the description that makes it readable.
///
/// The description is load-bearing rather than decorative on a question the agent authored:
/// "Tabs" and "Spaces" say far less than the sentences beside them, and that difference is
/// most of what makes a terminal's picker readable.
fn choice(value: &str, description: &str) -> Choice {
    Choice {
        value: value.to_string(),
        label: None,
        description: Some(description.to_string()),
    }
}

/// A real delegation tree, because a shallow one cannot exhibit the defects this shape
/// exists to prevent.
///
/// Concretely it needs all five of these, and each is a shipped defect:
///
/// - **Two trajectories running concurrently**, interleaved on one wire, which is the only
///   way to catch grouping by arrival order instead of by `span`.
/// - **One nested three deep.** A flat subagent list passes at depth 1 and breaks at depth
///   2, so depth 3 is the smallest tree that cannot be faked.
/// - **A failure buried at the bottom.** A collapsed card showing green over it is the worst
///   outcome of collapsing, so a descendant failure has to escalate its ancestors.
/// - **One left open**, which only the server closes.
/// - The **`span` on the close as well as the open** — see [`fanout`], where that is the
///   measured trap.
async fn delegate(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("blk-dispatch")
            .text("Fanning out to two agents.")
            .done(),
    )
    .await;

    // Both open before either emits, so their events then interleave.
    open_delegate(emit, "sub-explore", Some("Explore"), None).await;
    open_delegate(emit, "sub-verify", Some("Verify"), None).await;

    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("blk-ex-think")
            .span("sub-explore")
            .text("Searching for the call sites.")
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("blk-vf-think")
            .span("sub-verify")
            .text("Planning the verification.")
            .done(),
    )
    .await;
    tool(
        emit,
        ToolPair {
            stream_id: "blk-ex-grep",
            name: "Grep",
            input: json!({"pattern": "fn handler"}),
            result: "src/api.rs:42: fn handler()",
            span: Some("sub-explore"),
            ..Default::default()
        },
    )
    .await;

    // Depth 3: a trajectory inside a trajectory inside the turn. This is the
    // dynamic-workflow shape — a phase that itself delegates.
    open_delegate(emit, "sub-worker", Some("Worker"), Some("sub-verify")).await;
    tool(
        emit,
        ToolPair {
            stream_id: "blk-wk-bash",
            name: "Bash",
            input: json!({"command": "cargo test --lib"}),
            result: "error[E0425]: cannot find value `after`",
            is_error: true,
            data: Some(json!({"exitCode": 101})),
            span: Some("sub-worker"),
        },
    )
    .await;
    close_delegate(emit, "sub-worker", Some("sub-verify"), "", false).await;

    // Closes clean, but its descendant failed: the card must still report a failure, or
    // collapsing hides it.
    close_delegate(emit, "sub-verify", None, "", false).await;
    close_delegate(emit, "sub-explore", None, "Found 3 call sites.", false).await;

    // Opened and never closed. The turn settling is what resolves it, so a spec can assert
    // it does not spin forever.
    open_delegate(emit, "sub-stranded", Some("Stranded"), None).await;
    reply(emit, false).await;
}

/// A host that dispatches work into the background and returns.
///
/// The turn ends with the delegate still open; its completion arrives **after** settling,
/// in [`unprompted_after_settle`].
async fn unprompted(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("up-intro")
            .text("Dispatching a background worker.")
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::ToolCall, Some(Phase::Completed))
            .stream_id("up-host")
            .name("Workflow")
            .text(json!({"script": "background(worker)"}).to_string())
            .done(),
    )
    .await;
    open_delegate(emit, "up-task", Some("Background: worker"), Some("up-host")).await;
    emit.event(
        Ev::new(EventKind::ToolResult, Some(Phase::Completed))
            .stream_id("up-host")
            .name("Workflow")
            .text("dispatched")
            .done(),
    )
    .await;
    reply(emit, false).await;
}

/// The agent speaking **after** its turn settled.
///
/// *Measured on `claude` 2.x:* a session with backgrounded work emits the delegate's
/// completion after its turn's `result`, then opens an entire exchange of its own to report
/// it, with stdout still open and the process still alive. Those events used to be dropped
/// for arriving outside a turn — so the completion a user was promised was discarded by the
/// transport, and no fixture could observe it because none emitted after settling.
///
/// Two orderings here are the contract rather than incidental:
///
/// - The delegate's close names a host from **the turn that has already ended**, which is
///   what makes this a report about earlier work rather than something new.
/// - `turn`/`started` arrives **after** the first out-of-turn event. That is what the vendor
///   does, and a server bracketing on it would drop the payload above and keep the
///   announcement — so the server must open an agent-initiated turn on the first event of
///   *any* kind.
async fn unprompted_after_settle(emit: &Emitter) {
    tokio::time::sleep(Duration::from_millis(500)).await;
    close_delegate(
        emit,
        "up-task",
        Some("up-host"),
        "the background worker finished",
        false,
    )
    .await;
    emit.event(Ev::new(EventKind::Turn, Some(Phase::Started)).done())
        .await;
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("up-late")
            .text("The background worker finished while you were away.")
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Usage, Some(Phase::Completed))
            .stream_id("up-usage")
            .data(json!({"inputTokens": 3, "outputTokens": 9}))
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Turn, Some(Phase::Completed))
            .text("The background worker finished while you were away.")
            .data(json!({"stopReason": "end_turn"}))
            .done(),
    )
    .await;
}

/// Open a delegated trajectory.
///
/// `name` is an `Option` and that is the whole point of the signature: a nameless delegate
/// is a real and common case — a delegation's brief arrives as streamed partial JSON, so at
/// `started` there is usually nothing to name it with — and **a placeholder written here
/// becomes permanent**. `claude`'s adapter once sent the literal `"Subagent"`, and rows
/// already journaled that way are indistinguishable from a genuine name.
async fn open_delegate(emit: &Emitter, stream_id: &str, name: Option<&str>, span: Option<&str>) {
    let mut event = Ev::new(EventKind::Subagent, Some(Phase::Started)).stream_id(stream_id);
    if let Some(name) = name {
        event = event.name(name);
    }
    if let Some(span) = span {
        event = event.span(span);
    }
    emit.event(event.done()).await;
}

/// Close a delegated trajectory.
///
/// **`span` is set on the close as well as the open, and it is load-bearing rather than
/// tidy.** `history::consolidate` folds a block's phases into one row keyed by the *closing*
/// event, and it carries `name` forward from an earlier phase but **not** `span`. So a
/// nested delegate that declares its parent only on `started` is correctly nested while the
/// turn is live and **re-roots itself the moment the turn settles**.
async fn close_delegate(emit: &Emitter, stream_id: &str, span: Option<&str>, text: &str, failed: bool) {
    let mut event = Ev::new(EventKind::Subagent, Some(Phase::Completed)).stream_id(stream_id);
    if let Some(span) = span {
        event = event.span(span);
    }
    if !text.is_empty() {
        event = event.text(text);
    }
    if failed {
        event = event.failed();
    }
    emit.event(event.done()).await;
}

/// Delegation that is never **declared** — the shape every delegating tool outside
/// `claude`'s built-in `Task` actually has, and the one no fixture covered.
///
/// A host is discovered to be a container only because its children name it
/// (`docs/session-protocol.md` § Promotion), so **nothing here emits a `subagent` event for
/// the hosts themselves**. That is what makes this scenario a test of promotion rather than
/// of declaration.
///
/// Four hosts, each a distinct shape a real session produces, plus one thing that looks like
/// a delegation and is not.
async fn promote(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("pr-intro")
            .text("Running a workflow.")
            .done(),
    )
    .await;

    // Host 1: multiplexes **two** delegates, and forwards no interior of its own.
    //
    // Two rather than one, and that is the measured part: keyed by the host, N delegates
    // become one row where the last name wins, and with N=1 that collapse is
    // indistinguishable from correct. Measured on a real session as a two-agent workflow
    // rendering as a single card reading `Demo: agent-2`, with `agent-1` absent from the
    // timeline entirely. The per-delegate identity is the delegate's own id, with the host
    // as its `span`.
    //
    // Its own close is its `tool_result`, **not** its call's `completed` phase — a call
    // completes when its arguments finish streaming, which says nothing about the delegated
    // work.
    host_call(emit, "pr-host-a", "Workflow", json!({"script": "parallel([agentOne, agentTwo])"})).await;
    open_delegate(emit, "pr-task-1", Some("Demo: agent-1"), Some("pr-host-a")).await;
    open_delegate(emit, "pr-task-2", Some("Demo: agent-2"), Some("pr-host-a")).await;

    // Progress as a **snapshot**: repeated, and identical each time. A vendor's progress
    // notification re-sends its whole current summary rather than a suffix, so appending
    // each one as delta text is what stored one sentence six times.
    for _ in 0..3 {
        emit.event(
            Ev::new(EventKind::Subagent, Some(Phase::Delta))
                .stream_id("pr-task-1")
                .span("pr-host-a")
                .data(json!({"progress": "Working on the demo"}))
                .done(),
        )
        .await;
    }
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("pr-t1-think")
            .span("pr-task-1")
            .text("Reading the brief.")
            .done(),
    )
    .await;
    close_delegate(emit, "pr-task-1", Some("pr-host-a"), "agent-1 done", false).await;

    // A failure inside the second delegate, so a spec can assert the promoted host reports
    // it *while collapsed*.
    tool(
        emit,
        ToolPair {
            stream_id: "pr-t2-bash",
            name: "Bash",
            input: json!({"command": "cargo test"}),
            result: "error: test failed",
            is_error: true,
            data: Some(json!({"exitCode": 101})),
            span: Some("pr-task-2"),
        },
    )
    .await;
    close_delegate(emit, "pr-task-2", Some("pr-host-a"), "agent-2 failed", true).await;
    host_result(emit, "pr-host-a", "Workflow", "workflow finished").await;

    // Host 2: reports that delegated work ran and forwards none of it.
    //
    // A closed trajectory with **no children is a terminal state, not a loading one**, and
    // must not present as an expandable card that never fills. *Measured:*
    // `--forward-subagent-text` only forwards for the built-in `Task`, so for every other
    // delegating tool the interior cannot be attributed at all — saying so only after the
    // click is the lossy collapse the tree exists to avoid.
    host_call(emit, "pr-host-b", "Agent", json!({"task": "summarize"})).await;
    close_delegate(emit, "pr-host-b", None, "reported nothing", false).await;

    // Host 3: promoted, and left **open**. Only the server closes this, so a spec can assert
    // a promoted host does not spin forever either — the path that reading the call's
    // `phase` used to miss entirely, because a `tool_call`'s `completed` phase reports every
    // delegation as finished the instant its input parsed.
    host_call(emit, "pr-host-c", "Workflow", json!({"script": "phase('Stage A')"})).await;
    open_delegate(emit, "pr-task-3", Some("Stage A: A:item-1"), Some("pr-host-c")).await;

    // Host 4: the shape a real backgrounded workflow has, and the one that produced two
    // wrong readings at once.
    //
    // Its own call **succeeded** — the tool dispatched and returned — while its delegate was
    // still running when the turn ended, so only the server closes that. The host must
    // therefore read as done, **not failed**: the server marks its closing event as an error
    // so the row reads as unresolved, and counting that as a descendant failure badged a
    // workflow red for finishing exactly as designed. And the delegate must not claim "no
    // interior", because nobody looked — which is a different fact from a host that finished
    // and forwarded nothing.
    host_call(emit, "pr-host-d", "Workflow", json!({"script": "parallel([sleep20, sleep15])"})).await;
    open_delegate(emit, "pr-task-4", Some("Barrier: worker-1"), Some("pr-host-d")).await;
    host_result(emit, "pr-host-d", "Workflow", "dispatched; running in background").await;

    // A backgrounded **Bash** job, which is not a delegation at all.
    //
    // `claude` reports it through the same `task_*` notifications a subagent uses and only
    // `task_type` tells them apart, so an adapter classifying by the notification alone
    // publishes a shell command as a subagent — a card named after the command, whose error
    // flag then escalates the Bash call hosting it, so a turn that did nothing wrong reads
    // `failed` from its first moment. Measured on a real session.
    //
    // A correct adapter emits **no trajectory** for this, so the fixture emits the tool call
    // alone: what a client must render is one failed Bash row and no delegation card
    // anywhere near it.
    tool(
        emit,
        ToolPair {
            stream_id: "pr-bg-bash",
            name: "Bash",
            input: json!({"command": "find / -ipath \"*workflows*\"", "run_in_background": true}),
            result: "find: permission denied",
            is_error: true,
            data: Some(json!({"exitCode": 1})),
            ..Default::default()
        },
    )
    .await;
    reply(emit, false).await;
}

/// A promoted host's own call: a `tool_call` nothing declares as a trajectory.
async fn host_call(emit: &Emitter, stream_id: &str, name: &str, input: Value) {
    emit.event(
        Ev::new(EventKind::ToolCall, Some(Phase::Completed))
            .stream_id(stream_id)
            .name(name)
            .text(input.to_string())
            .done(),
    )
    .await;
}

/// A promoted host's close, which is its `tool_result` rather than its call's phase.
async fn host_result(emit: &Emitter, stream_id: &str, name: &str, text: &str) {
    emit.event(
        Ev::new(EventKind::ToolResult, Some(Phase::Completed))
            .stream_id(stream_id)
            .name(name)
            .text(text)
            .done(),
    )
    .await;
}

/// How long `paced` waits between stages.
///
/// **A second, and it must stay far longer than a frame.** `e2e/streaming.spec.ts` asserts
/// an earlier stage is on screen *while a later one has not been emitted yet*, and without
/// real gaps that spec passes against a client that streams nothing. Every other scenario
/// finishes faster than a single render, which is the exact blind spot that let a fully
/// non-streaming client ship once already.
const PACED_STAGE: Duration = Duration::from_secs(1);

/// Deliberately slow, so "did the UI paint mid-turn" is observable rather than inferred.
async fn paced(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Started))
            .stream_id("blk-slow-think")
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Delta))
            .stream_id("blk-slow-think")
            .text("Reasoning step one. ")
            .done(),
    )
    .await;
    tokio::time::sleep(PACED_STAGE).await;
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Delta))
            .stream_id("blk-slow-think")
            .text("Reasoning step two.")
            .done(),
    )
    .await;
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("blk-slow-think")
            .text("Reasoning step one. Reasoning step two.")
            .done(),
    )
    .await;
    tokio::time::sleep(PACED_STAGE).await;
    tool(
        emit,
        ToolPair {
            stream_id: "blk-slow-tool",
            name: "Read",
            input: json!({"file_path": "src/paced.rs"}),
            result: "pub fn paced() {}",
            ..Default::default()
        },
    )
    .await;
    tokio::time::sleep(PACED_STAGE).await;
    // A second thinking block: multi-round reasoning has to stream, not only the first.
    emit.event(
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("blk-slow-think2")
            .text("Second round of reasoning.")
            .done(),
    )
    .await;
    tokio::time::sleep(PACED_STAGE).await;
    reply(emit, true).await;
}

/// The six delegates `fanout` runs, in the order they open.
///
/// **Six, because scanning two is not work.** The question the delegation overview answers is
/// "which of these was slow", and with two delegates the answer is visible without any
/// overview at all. Six is the smallest number where scanning is real work and where a
/// concurrent band is distinguishable from a staircase.
const FANOUT_DELEGATES: &[&str] = &["Survey", "Compile", "Lint", "Typecheck", "Format", "Audit"];

/// How long `fanout` holds the turn open **after** its reply.
///
/// **Eight seconds, and the number is empirical rather than generous.**
///
/// A settled turn's transcript comes from *consolidated* history, and consolidation folds a
/// declared trajectory's phases into one row keyed by its close — so its `started` phase, and
/// with it every exact duration, is gone by the time the turn stops being live. Any assertion
/// comparing real durations therefore has a window, and without a hold that window is the
/// microseconds between the reply and settling. A spec racing that is a spec that fails on a
/// slow machine and reads as flakiness.
///
/// The window is consumed from **both** ends: it opens when this sleep begins, and a spec
/// cannot assert until the browser has painted the reply — then it has to open the strip and
/// read several rows, each with its own polling deadline. So the whole assertion sequence has
/// to fit, not merely start.
///
/// **Three seconds fit in isolation and did not under full-suite load**: the slowest-row badge
/// vanished mid-spec, because once the turn settles every duration degrades to a lower bound
/// and the badge is deliberately withheld. That failure reads as a broken overview rather than
/// as a closed window, which is what makes an under-sized hold worse than a slow one.
const FANOUT_HOLD: Duration = Duration::from_secs(8);

/// A **wide** delegation with genuinely different durations.
///
/// Two things make it its own scenario rather than an edit to [`delegate`]:
///
/// **Width**, per [`FANOUT_DELEGATES`].
///
/// **Duration.** Every other scenario emits its whole tree inside one millisecond, so every
/// bar has the same length and the slowest is undefined — a client that computed durations
/// from the wrong field, or laid every bar out from the axis origin, passed the entire suite.
/// **The server stamps arrival time**, so the only way to make one delegate measurably slower
/// than another is to actually take longer.
///
/// Budget: ~2.6s of sleep, comparable to [`paced`], and deliberately spent on **one clear
/// outlier plus a short tail** rather than spread evenly, because "one of these is much worse
/// than the rest" is the real shape of a slow fan-out.
async fn fanout(emit: &Emitter) {
    emit.event(
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("fo-intro")
            .text("Fanning out to six agents.")
            .done(),
    )
    .await;

    // All six open before any closes: their bars must then overlap in one band. Laid out
    // sequentially — each starting where the last ended — this reads as a chain, which is a
    // different run and the mistake a single-column rendering cannot show.
    for name in FANOUT_DELEGATES {
        let id = format!("fo-{}", name.to_lowercase());
        open_delegate(emit, &id, Some(name), None).await;
        // Each delegate says something early, and that is not decoration.
        //
        // A settled turn's transcript comes from consolidated history, where a trajectory's
        // `started` phase is folded away — so its own start is gone and the only remaining
        // lower bound on when it ran is the first event it **forwarded**. A delegate that
        // forwards nothing therefore cannot be timed at all once the turn settles. Five of
        // six forwarding nothing made the settled comparison almost entirely untimed, which
        // is not what a real fan-out looks like: a delegate that ran did work, and work is
        // events. `Audit` is the deliberate exception.
        if *name != "Audit" {
            emit.event(
                Ev::new(EventKind::Thinking, Some(Phase::Completed))
                    .stream_id(&format!("fo-{}-think", name.to_lowercase()))
                    .span(&id)
                    .text(format!("{name} is starting."))
                    .done(),
            )
            .await;
        }
    }

    // A delegate with **no name**, which nothing else in this suite emits.
    //
    // This is the case a real `claude` session produces constantly: a delegation's
    // `subagent_type`/`description` arrives as streamed partial JSON, so at `started` there is
    // usually nothing to name it with. The client falls back to "Subagent" at render time, and
    // until this row no fixture reached that path — every other `subagent` open here carries a
    // name, so the fallback existed and was covered only by unit tests.
    //
    // That mattered in a way a green suite hid: the label it produces is "Subagent
    // delegation", which **contains** "agent delegation", so a `{ name: "<tool> delegation" }`
    // locator is ambiguous with it. Five such locators were given `exact: true` to fix exactly
    // that — and no test could produce the string they were guarding against, so the guards
    // were correct and unexercised. This row makes them load-bearing.
    //
    // Closed quickly and forwarding nothing: it must not compete for "slowest".
    open_delegate(emit, "fo-nameless", None, None).await;

    // A nested delegate inside the slowest one, so the overview has to carry depth as well as
    // duration — and so a deep row's bar stays on the shared axis instead of being indented
    // off it.
    open_delegate(emit, "fo-probe", Some("Deep probe"), Some("fo-survey")).await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    close_delegate(emit, "fo-format", None, "formatted", false).await;
    // Closed with no name on this phase either, so the row is nameless for its whole life
    // rather than acquiring one at the end.
    close_delegate(emit, "fo-nameless", None, "", false).await;

    tokio::time::sleep(Duration::from_millis(150)).await;
    close_delegate(emit, "fo-lint", None, "clean", false).await;

    tokio::time::sleep(Duration::from_millis(150)).await;
    // A failure in a **fast** delegate, so "slowest" and "failed" are on different rows: a
    // view that conflated them would look right whenever they coincide.
    tool(
        emit,
        ToolPair {
            stream_id: "fo-tc-bash",
            name: "Bash",
            input: json!({"command": "tsc --noEmit"}),
            result: "error TS2345: argument of the wrong type",
            is_error: true,
            data: Some(json!({"exitCode": 2})),
            span: Some("fo-typecheck"),
        },
    )
    .await;
    close_delegate(emit, "fo-typecheck", None, "type errors", true).await;

    // Closed having forwarded nothing: the overview must say so rather than drawing an empty
    // bar, which is the same lie as offering an empty card.
    //
    // Closed **before** the outlier below on purpose — closing it last made its duration tie
    // with the outlier's, and a tie has no unique slowest row, so any spec asserting one would
    // be asserting a coin flip.
    tokio::time::sleep(Duration::from_millis(150)).await;
    close_delegate(emit, "fo-audit", None, "reported nothing", false).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    close_delegate(emit, "fo-compile", None, "built", false).await;

    // The outlier, and the row every assertion about "slowest" is about. Closed well after
    // every sibling, with its nested delegate closing just before it.
    tokio::time::sleep(Duration::from_millis(1400)).await;
    // `span` on the close as well — see `close_delegate`, where the consolidation trap this
    // guards against is recorded. Every other scenario happens to set it on both phases,
    // which is why nothing caught it.
    close_delegate(emit, "fo-probe", Some("fo-survey"), "probed", false).await;
    close_delegate(emit, "fo-survey", None, "surveyed 412 files", false).await;

    // Left open, so the turn-settle sweep closes it. Its end is when we stopped listening,
    // not when the work stopped, and the row has to say so.
    open_delegate(emit, "fo-cutoff", Some("Cutoff"), None).await;
    reply(emit, false).await;

    tokio::time::sleep(FANOUT_HOLD).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::collections::BTreeMap;

    /// Run one turn and collect every message it produced.
    ///
    /// Drives the real [`Session`], so a test cannot assert on a scenario function the
    /// production path does not call.
    async fn run(prompt: &str) -> Vec<SessionMessage> {
        let adapter = E2eAdapter::new();
        let mut session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::Dictate(NativeId::new("e2e-test")),
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .expect("the e2e agent always opens");
        let stream = session.events().expect("the stream is yielded once");
        session
            .send(TurnInput {
                text: prompt.to_string(),
                ..Default::default()
            })
            .await
            .expect("a turn is accepted");

        // The turn ends the moment it settles, so the collection is bounded by the
        // scenario rather than by a timeout — except for a scenario that deliberately
        // blocks, which has no terminal event and is read with `take` instead.
        let mut collected = Vec::new();
        let mut stream = stream;
        while let Some(message) = stream.next().await {
            let message = message.expect("the fixture never yields a read failure");
            let terminal = matches!(
                &message,
                SessionMessage::Event(event)
                    if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed)
            );
            collected.push(message);
            if terminal {
                break;
            }
        }
        collected
    }

    /// Every event of one kind, in order.
    fn events(messages: &[SessionMessage]) -> Vec<&Event> {
        messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    fn of_kind(messages: &[SessionMessage], kind: EventKind) -> Vec<&Event> {
        events(messages)
            .into_iter()
            .filter(|event| event.kind == kind)
            .collect()
    }

    fn requests(messages: &[SessionMessage]) -> Vec<&Request> {
        messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// One block's phases, in arrival order.
    fn phases<'a>(messages: &'a [SessionMessage], stream_id: &str) -> Vec<&'a Event> {
        events(messages)
            .into_iter()
            .filter(|event| event.stream_id.as_deref() == Some(stream_id))
            .collect()
    }

    /// **The catalogue's size is the test.** A client renders a filterable list above a
    /// threshold of 8 and a menu below it, and every symptom of an unscrollable,
    /// unsearchable catalogue was invisible while fixture options came two at a time.
    #[test]
    fn the_catalogue_is_long_enough_to_reach_the_clients_search_threshold() {
        const CLIENT_SEARCH_THRESHOLD: usize = 8;
        assert_eq!(CATALOGUE.len(), 17);
        assert!(
            CATALOGUE.len() > CLIENT_SEARCH_THRESHOLD + 1,
            "a catalogue at or below the threshold cannot observe the searchable list"
        );

        // And the short option has to stay short, or the menu pattern loses its coverage.
        let config = initial_config();
        let short = config.iter().find(|option| option.id == "model").unwrap();
        assert!(
            short.options.len() < CLIENT_SEARCH_THRESHOLD,
            "the two-choice option is what keeps the menu contract covered"
        );
        let long = config.iter().find(|option| option.id == "provider").unwrap();
        assert_eq!(long.options.len(), CATALOGUE.len());
    }

    /// **Two providers offer one display name, and the values still have to be unique.**
    ///
    /// `checked` is computed as a filter — every producer writes `value == current_value` —
    /// so a shared value puts a checkmark on two rows and makes one of them unreachable,
    /// since a by-id lookup can only answer with one provider. Measured on `pi` 0.84.3.
    #[test]
    fn a_duplicated_display_name_still_has_two_distinct_reachable_values() {
        let shared: Vec<_> = CATALOGUE
            .iter()
            .filter(|(_, name, _)| *name == "GLM-5.2")
            .collect();
        assert_eq!(shared.len(), 2, "the duplicate-name pair is the point");

        let values: std::collections::HashSet<_> = shared.iter().map(|(value, _, _)| value).collect();
        assert_eq!(values.len(), 2, "both rows would be checked at once");
        // Provider-qualified, and delimited with a space rather than a `/` — 350 of pi's
        // 372 ids already contain a slash, so splitting on one mis-parses the catalogue.
        for (value, _, _) in shared {
            assert!(value.contains(' '), "{value} must be provider-qualified");
            assert!(!value.contains('/'), "{value} must not use a slash delimiter");
        }
    }

    /// Every value in the whole catalogue is unique, not only the pair above.
    #[test]
    fn no_two_catalogue_entries_share_a_value() {
        let mut seen = std::collections::HashSet::new();
        for (value, _, _) in CATALOGUE {
            assert!(seen.insert(*value), "{value} appears twice");
        }
    }

    /// **`INVALID_VALUE`, not `UNSUPPORTED_FIELD`.** The option exists and this one value is
    /// rejected; the server classifies on the code, and the earlier fixture used the other
    /// one — so the branch that keeps a control *usable* had no end-to-end coverage while
    /// the suite claimed to cover a refused value.
    #[tokio::test]
    async fn a_refused_value_is_invalid_value_and_an_unknown_option_is_unsupported_field() {
        let adapter = E2eAdapter::new();
        let session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();

        let refused = session
            .set_config("provider", json!(REFUSED_VALUE))
            .await
            .expect_err("the refusable value must be refused");
        let code = refused
            .downcast_ref::<AdapterError>()
            .expect("a refusal travels as a typed AdapterError")
            .code;
        assert_eq!(
            code,
            sylph_protocol::RefusalCode::InvalidValue,
            "a refused value must leave the control usable"
        );

        let unknown = session
            .set_config("no-such-option", json!("x"))
            .await
            .expect_err("an unknown option must be refused");
        assert_eq!(
            unknown.downcast_ref::<AdapterError>().unwrap().code,
            sylph_protocol::RefusalCode::UnsupportedField
        );

        // And the refusal must not have changed anything, or a refused write would be
        // indistinguishable from an applied one.
        let surface = session.capabilities().await.unwrap();
        let provider = surface.config_option("provider").unwrap();
        assert_eq!(provider.current_value, Some(json!("sonnet-5")));
    }

    /// A successful set is read back **from the surface**, not from what was requested.
    #[tokio::test]
    async fn an_applied_set_is_reported_by_the_surface_itself() {
        let adapter = E2eAdapter::new();
        let session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();

        let refreshed = session
            .set_config("provider", json!("ikun glm-5.2"))
            .await
            .expect("an offered value is accepted");
        // The **whole refreshed array**, because setting one option can change another's
        // choices — not an acknowledgement.
        assert_eq!(refreshed.len(), initial_config().len());
        let provider = refreshed.iter().find(|o| o.id == "provider").unwrap();
        assert_eq!(provider.current_value, Some(json!("ikun glm-5.2")));
        assert_eq!(
            session
                .capabilities()
                .await
                .unwrap()
                .config_option("provider")
                .unwrap()
                .current_value,
            Some(json!("ikun glm-5.2")),
            "capabilities must read the vendor's state, not the last request"
        );
    }

    /// **A per-turn capability update naming exactly one facet.**
    ///
    /// `claude`'s `system/init` arrives once per *turn* and carries only the permission
    /// mode. A whole-surface payload answering it erased 46 project slash commands on the
    /// first exchange of every session, and the symptom surfaced two layers away as a
    /// composer menu that would not open. No spec could see it while the fixture reported
    /// its surface only at open — so this update existing, and mentioning **one** facet, is
    /// what makes the regression observable.
    #[tokio::test]
    async fn every_turn_reports_one_capability_facet_and_never_the_whole_surface() {
        let messages = run("hello").await;
        let updates: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                SessionMessage::Capabilities(facets) => Some(facets),
                _ => None,
            })
            .collect();
        assert_eq!(updates.len(), 1, "one per turn, as the vendor does");

        let facets = updates[0];
        assert_eq!(facets.permission_mode.as_deref(), Some("default"));
        // The other four facets must be **unmentioned**, because an unmentioned facet
        // cannot be cleared and a mentioned-but-empty one erases.
        assert!(facets.commands.is_none(), "naming commands would erase them");
        assert!(facets.subagents.is_none());
        assert!(facets.config.is_none());
        assert!(facets.context_window.is_none());
        assert!(!facets.is_empty(), "an update naming no facet is a no-op");
    }

    /// The default scenario streams: `started`, deltas, then the whole text on `completed`.
    ///
    /// Asserting the *phases* rather than the final text, because a client that dropped every
    /// mid-turn event still produces the right final text — which is exactly how a
    /// non-streaming client passed the whole suite once.
    #[tokio::test]
    async fn an_ordinary_turn_streams_its_reply_in_phases() {
        let messages = run("hello").await;
        let reply = phases(&messages, "blk-reply");
        let arrived: Vec<_> = reply.iter().map(|event| event.phase).collect();
        assert_eq!(
            arrived,
            vec![
                Some(Phase::Started),
                Some(Phase::Delta),
                Some(Phase::Delta),
                Some(Phase::Completed)
            ]
        );
        // Deltas are a suffix each; the close carries the whole text.
        assert_eq!(reply[1].text, "Hello from ");
        assert_eq!(reply[2].text, "the E2E runner.");
        assert_eq!(reply[3].text, "Hello from the E2E runner.");
    }

    /// `bulk` emits 250 of each, and the count is what a render-cost measurement rests on.
    #[tokio::test]
    async fn bulk_emits_two_hundred_and_fifty_completed_tool_pairs() {
        let messages = run("bulk render please").await;
        assert_eq!(of_kind(&messages, EventKind::ToolCall).len(), 250);
        assert_eq!(of_kind(&messages, EventKind::ToolResult).len(), 250);
        // Completed blocks only, so every one is journalled and survives a reload — which is
        // what makes the measurement reproducible rather than dependent on stream timing.
        assert!(
            of_kind(&messages, EventKind::Assistant)
                .iter()
                .all(|event| event.phase == Some(Phase::Completed)),
            "a delta here would make the measurement depend on stream timing"
        );
    }

    /// A failing tool reports its exit code **on the result**, which is where the timeline
    /// reads it: "failed" alone is not what anyone wants from a failed command.
    #[tokio::test]
    async fn a_failing_tool_carries_its_exit_code_on_the_result_not_the_call() {
        let messages = run("exercise tools please").await;
        let bash: Vec<_> = phases(&messages, "blk-bash");
        assert_eq!(bash.len(), 2, "a call and its result");

        let (call, result) = (bash[0], bash[1]);
        assert_eq!(call.kind, EventKind::ToolCall);
        assert!(!call.is_error, "the call itself did not fail; the command did");
        assert_eq!(result.kind, EventKind::ToolResult);
        assert!(result.is_error);
        assert_eq!(result.data.as_ref().unwrap()["exitCode"], 101);
        // The call carries the parsed input, which is what an expanded row renders.
        assert_eq!(call.data.as_ref().unwrap()["input"]["command"], "cargo test --lib");
    }

    /// An approval **blocks**: it raises a request and emits no terminal turn event, so the
    /// turn is still live and `resolve` is what settles it.
    #[tokio::test]
    async fn an_approval_blocks_the_turn_until_it_is_answered() {
        let adapter = E2eAdapter::new();
        let mut session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();
        let mut stream = session.events().unwrap();
        session
            .send(TurnInput {
                text: "approval please".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        // Read until the request arrives. A terminal turn event before it would mean the
        // scenario settled without asking, which is the failure this pins.
        let mut collected = Vec::new();
        let mut request = None;
        while let Some(message) = stream.next().await {
            let message = message.unwrap();
            if let SessionMessage::Request(raised) = &message {
                request = Some(raised.clone());
                collected.push(message);
                break;
            }
            if let SessionMessage::Event(event) = &message {
                assert!(
                    !(event.kind == EventKind::Turn && event.phase == Some(Phase::Completed)),
                    "the turn settled without raising the approval it exists to raise"
                );
            }
            collected.push(message);
        }

        let request = request.expect("the approval must be raised");
        assert_eq!(request.kind, RequestKind::Approval);
        // An approval's subject **is** its detail, which is the one kind that legitimately
        // declares no form.
        assert!(request.form.is_none());
        assert_eq!(request.detail.as_ref().unwrap()["command"], "rm -rf build");
        assert_eq!(request.tool.as_deref(), Some("Bash"));
        assert!(
            request.options.iter().any(|option| option.persistent),
            "the lasting-grant option is what a client marks differently"
        );

        // Answering is what unblocks it, and the answer reaches the tool result verbatim.
        session
            .resolve(Resolution {
                request_id: request.id.clone(),
                option: "allow".into(),
                input: Some(json!({"command": "rm -rf build"})),
            })
            .await
            .unwrap();

        let mut settled = false;
        let mut echoed = None;
        while let Some(message) = stream.next().await {
            if let SessionMessage::Event(event) = message.unwrap() {
                if event.stream_id.as_deref() == Some("blk-approved") {
                    echoed = Some(event.text.clone());
                }
                if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed) {
                    settled = true;
                    break;
                }
            }
        }
        assert!(settled, "answering must settle the turn");
        // **The value the agent received**, not merely that the panel closed. Writing an
        // answer into a vendor field that does not exist was silent at every layer: the
        // panel closed, the turn resumed, and the CLI told the model the user had not
        // answered.
        assert_eq!(
            echoed.as_deref(),
            Some(r#"Received: {"command":"rm -rf build"}"#),
            "the answer must reach the tool result the model reads"
        );
    }

    /// A narrow approval forbids a lasting grant **entirely**.
    ///
    /// `claude` sets this on asks where accepting always would write a permission rule
    /// broader than the ask itself, so offering it turns a narrow yes into a wide one the
    /// user never read.
    #[tokio::test]
    async fn a_narrow_approval_offers_no_lasting_grant_at_all() {
        let messages = run_until_request("narrow please").await;
        let request = requests(&messages)[0];
        assert!(!request.allow_persistent);
        assert!(
            !request.options.iter().any(|option| option.persistent),
            "no option may be persistent when the ask forbids it"
        );
    }

    /// A question declares a **form**, and its two fields stay independent.
    ///
    /// One field could not observe that two radio groups do not share state — a single
    /// shared name would make answering the second clear the first. And `allow_other` plus
    /// `notes` are the two affordances whose absence was the reported bug.
    #[tokio::test]
    async fn a_question_declares_two_independent_fields_with_other_and_notes() {
        let messages = run_until_request("question please").await;
        let request = requests(&messages)[0];
        assert_eq!(request.kind, RequestKind::Choice);

        let form = request.form.as_ref().expect("a question declares its form");
        assert_eq!(form.fields.len(), 2, "one field cannot observe independence");

        let first = &form.fields[0];
        match &first.kind {
            FieldKind::Select {
                mode,
                choices,
                allow_other,
                other_label,
                notes,
            } => {
                assert_eq!(*mode, SelectMode::One);
                assert_eq!(choices.len(), 2);
                // The descriptions are load-bearing: "Tabs" and "Spaces" say far less than
                // the sentences beside them.
                assert_eq!(choices[0].description.as_deref(), Some("Indent with tabs."));
                assert!(*allow_other, "a client must be able to send a typed answer");
                assert_eq!(other_label.as_deref(), Some("Something else"));
                assert!(notes.is_some(), "a note has nowhere to live without this");
            }
            other => panic!("expected a select, found {other:?}"),
        }

        let second = &form.fields[1];
        assert!(second.required, "a required multi-select is what refuses an empty submit");
        match &second.kind {
            FieldKind::Select { mode, choices, .. } => {
                assert_eq!(*mode, SelectMode::Many);
                assert_eq!(choices.len(), 3);
            }
            other => panic!("expected a select, found {other:?}"),
        }
    }

    /// An elicitation is free text carrying a **prefill**, which is the draft being revised.
    #[tokio::test]
    async fn an_elicitation_declares_a_text_field_with_its_prefill() {
        let messages = run_until_request("elicit please").await;
        let request = requests(&messages)[0];
        assert_eq!(request.kind, RequestKind::Elicitation);
        let field = &request.form.as_ref().unwrap().fields[0];
        match &field.kind {
            FieldKind::Text {
                multiline,
                prefill,
                placeholder,
            } => {
                assert!(*multiline);
                // Discarding this would lose the draft `pi` sends for its editor dialog.
                assert_eq!(prefill.as_deref(), Some("Refactor the "));
                assert_eq!(placeholder.as_deref(), Some("One sentence"));
            }
            other => panic!("an elicitation's whole purpose is free text, found {other:?}"),
        }
    }

    /// Drive a blocking scenario until its request arrives.
    ///
    /// Separate from [`run`] because a blocking scenario has no terminal event, so the
    /// collector has to stop on the request instead — a shared helper waiting for a turn to
    /// settle would hang forever on exactly the four scenarios that matter here.
    async fn run_until_request(prompt: &str) -> Vec<SessionMessage> {
        let adapter = E2eAdapter::new();
        let mut session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();
        let mut stream = session.events().unwrap();
        session
            .send(TurnInput {
                text: prompt.to_string(),
                ..Default::default()
            })
            .await
            .unwrap();

        let mut collected = Vec::new();
        while let Some(message) = stream.next().await {
            let message = message.unwrap();
            let is_request = matches!(message, SessionMessage::Request(_));
            collected.push(message);
            if is_request {
                break;
            }
        }
        collected
    }

    /// `delegate` runs two trajectories **concurrently**, interleaved on one wire.
    ///
    /// Interleaving is the only way to catch grouping by arrival order instead of by `span`:
    /// both open before either emits, so a consumer that grouped by adjacency would put
    /// `Verify`'s thinking under `Explore`.
    #[tokio::test]
    async fn two_delegates_open_before_either_emits_so_their_events_interleave() {
        let messages = run("delegate this please").await;
        let all = events(&messages);

        let position = |stream_id: &str, phase: Phase| {
            all.iter()
                .position(|event| {
                    event.stream_id.as_deref() == Some(stream_id) && event.phase == Some(phase)
                })
                .unwrap_or_else(|| panic!("{stream_id} at {phase:?} is missing"))
        };

        let explore_open = position("sub-explore", Phase::Started);
        let verify_open = position("sub-verify", Phase::Started);
        let explore_work = position("blk-ex-think", Phase::Completed);
        assert!(
            explore_open < verify_open && verify_open < explore_work,
            "both must open before either emits, or arrival order and span agree by luck"
        );

        // And each child names its parent, which is what a tree is built from.
        let child = all
            .iter()
            .find(|event| event.stream_id.as_deref() == Some("blk-vf-think"))
            .unwrap();
        assert_eq!(child.span.as_deref(), Some("sub-verify"));
    }

    /// **Depth 3.** A flat subagent list passes at depth 1 and breaks at depth 2, so the
    /// smallest tree that cannot be faked is a trajectory inside a trajectory inside a turn.
    #[tokio::test]
    async fn a_delegation_nests_three_deep() {
        let messages = run("delegate this please").await;
        let all = events(&messages);

        let worker_open = all
            .iter()
            .find(|event| {
                event.stream_id.as_deref() == Some("sub-worker")
                    && event.phase == Some(Phase::Started)
            })
            .expect("the depth-3 trajectory must exist");
        // Turn -> sub-verify -> sub-worker -> the failing tool.
        assert_eq!(worker_open.span.as_deref(), Some("sub-verify"));
        let buried = all
            .iter()
            .find(|event| event.stream_id.as_deref() == Some("blk-wk-bash") && event.is_error)
            .expect("the failure buried at the bottom must exist");
        assert_eq!(buried.span.as_deref(), Some("sub-worker"));
    }

    /// The buried failure's ancestors close **clean**, which is what makes escalation the
    /// consumer's job rather than the adapter's.
    ///
    /// A collapsed card showing green over a failure three levels down is the worst outcome
    /// of collapsing, and the fixture must not paper over it by pre-marking the ancestors —
    /// then the client's escalation would never be exercised.
    #[tokio::test]
    async fn an_ancestor_of_a_buried_failure_closes_clean_so_escalation_is_the_clients_job() {
        let messages = run("delegate this please").await;
        let closed = |stream_id: &str| {
            *events(&messages)
                .iter()
                .find(|event| {
                    event.kind == EventKind::Subagent
                        && event.stream_id.as_deref() == Some(stream_id)
                        && event.phase == Some(Phase::Completed)
                })
                .unwrap_or_else(|| panic!("{stream_id} never closed"))
        };
        assert!(!closed("sub-worker").is_error);
        assert!(!closed("sub-verify").is_error, "the ancestor reports its own outcome");
        assert_eq!(closed("sub-explore").text, "Found 3 call sites.");
    }

    /// One delegation is **left open**, so the server's turn-settle sweep is what closes it.
    #[tokio::test]
    async fn a_stranded_delegation_is_left_for_the_server_to_close() {
        let messages = run("delegate this please").await;
        let stranded = phases(&messages, "sub-stranded");
        assert_eq!(stranded.len(), 1, "it must never close itself");
        assert_eq!(stranded[0].phase, Some(Phase::Started));
    }

    /// **A trajectory is stable at every phase**, including `started`.
    ///
    /// Its `started` phase is where a trajectory's id, name, and parent live. Dropped as an
    /// ephemeral projection, the journal keeps every child pointing at a parent it never
    /// recorded, and a reload flattens the whole subtree onto the root.
    #[tokio::test]
    async fn every_trajectory_phase_is_journaled() {
        let messages = run("delegate this please").await;
        for event in of_kind(&messages, EventKind::Subagent) {
            assert!(
                event.is_stable(),
                "{:?}/{:?} must reach the journal",
                event.stream_id,
                event.phase
            );
        }
    }

    /// **One host multiplexes two delegates**, and each keeps its own identity.
    ///
    /// Keyed by the host, N delegates collapse into one row where the last name wins — and
    /// with N=1 that is indistinguishable from correct. Measured on a real session as a
    /// two-agent workflow rendering as a single card reading `Demo: agent-2`, with `agent-1`
    /// absent from the timeline entirely.
    #[tokio::test]
    async fn one_promoted_host_carries_two_separately_identified_delegates() {
        let messages = run("promote this please").await;
        let under_host: Vec<_> = of_kind(&messages, EventKind::Subagent)
            .into_iter()
            .filter(|event| {
                event.span.as_deref() == Some("pr-host-a") && event.phase == Some(Phase::Started)
            })
            .collect();
        assert_eq!(under_host.len(), 2, "N=1 cannot observe the collapse");

        let names: Vec<_> = under_host
            .iter()
            .map(|event| event.name.as_deref().unwrap())
            .collect();
        assert_eq!(names, vec!["Demo: agent-1", "Demo: agent-2"]);
        let ids: std::collections::HashSet<_> =
            under_host.iter().map(|event| event.stream_id.as_deref()).collect();
        assert_eq!(ids.len(), 2, "a trajectory id must be the delegate's, not the host's");
    }

    /// **Nothing declares a promoted host.** It is discovered to be a container only because
    /// its children name it, which is the shape every delegating tool outside `claude`'s
    /// built-in `Task` actually has.
    #[tokio::test]
    async fn a_promoted_host_is_never_declared_as_a_trajectory() {
        let messages = run("promote this please").await;
        for host in ["pr-host-a", "pr-host-b", "pr-host-c", "pr-host-d"] {
            let declared = of_kind(&messages, EventKind::Subagent)
                .into_iter()
                .any(|event| {
                    event.stream_id.as_deref() == Some(host)
                        && event.phase == Some(Phase::Started)
                });
            assert!(!declared, "{host} must be promoted by reference, not declared");
        }

        // Host A's own close is its `tool_result`, not its call's `completed` phase: a call
        // completes when its *arguments* finish streaming, which says nothing about the
        // delegated work.
        let host = phases(&messages, "pr-host-a");
        assert_eq!(host[0].kind, EventKind::ToolCall);
        assert_eq!(host.last().unwrap().kind, EventKind::ToolResult);
        assert_eq!(host.last().unwrap().text, "workflow finished");
    }

    /// A delegation's progress is a **snapshot**, repeated identically — never appended text.
    ///
    /// A vendor's progress notification re-sends its whole current summary rather than a
    /// suffix, so appending each one is what stored one sentence six times.
    #[tokio::test]
    async fn delegation_progress_repeats_one_identical_snapshot() {
        let messages = run("promote this please").await;
        let deltas: Vec<_> = phases(&messages, "pr-task-1")
            .into_iter()
            .filter(|event| event.phase == Some(Phase::Delta))
            .collect();
        assert_eq!(deltas.len(), 3);
        for delta in &deltas {
            assert_eq!(delta.data.as_ref().unwrap()["progress"], "Working on the demo");
            assert!(
                delta.text.is_empty(),
                "progress travels as data; as text it would be appended and repeated"
            );
        }
    }

    /// **A backgrounded Bash job is not a delegation**, and the fixture emits no trajectory
    /// for it.
    ///
    /// `claude` reports it through the same notifications a subagent uses and only its type
    /// field tells them apart, so an adapter classifying by the notification alone publishes
    /// a shell command as a subagent — a card named after the command, whose error flag then
    /// escalates the Bash call hosting it, so a turn that did nothing wrong reads as failed
    /// from its first moment.
    #[tokio::test]
    async fn a_backgrounded_shell_job_emits_a_failed_tool_and_no_trajectory() {
        let messages = run("promote this please").await;
        let bash = phases(&messages, "pr-bg-bash");
        assert_eq!(bash.len(), 2, "a call and a result, and nothing else");
        assert!(bash.iter().all(|event| event.kind != EventKind::Subagent));
        assert!(bash[1].is_error);
        assert_eq!(bash[1].data.as_ref().unwrap()["exitCode"], 1);
        // And nothing anywhere names it as a parent, which is what a promoted card needs.
        assert!(
            !events(&messages)
                .iter()
                .any(|event| event.span.as_deref() == Some("pr-bg-bash")),
            "naming it as a span would promote a shell command to a subagent"
        );
    }

    /// `fanout` runs **six** delegates, because scanning two is not work.
    #[tokio::test]
    async fn fanout_runs_six_named_delegates_plus_a_nameless_one() {
        let messages = run("fanout across agents please").await;
        let opened: Vec<_> = of_kind(&messages, EventKind::Subagent)
            .into_iter()
            .filter(|event| event.phase == Some(Phase::Started))
            .collect();

        for name in FANOUT_DELEGATES {
            assert!(
                opened.iter().any(|event| event.name.as_deref() == Some(name)),
                "{name} must be one of the six"
            );
        }
        assert_eq!(FANOUT_DELEGATES.len(), 6, "six is the smallest scannable width");
    }

    /// **A nameless delegate**, whose name is absent at every phase.
    ///
    /// This is the case a real `claude` session produces constantly — a delegation's brief
    /// arrives as streamed partial JSON, so at `started` there is usually nothing to name it
    /// with — and no fixture reached the client's fallback before this row. A placeholder
    /// here would be permanent: rows already journaled with the literal `"Subagent"` are
    /// afterwards indistinguishable from a genuine name.
    #[tokio::test]
    async fn a_nameless_delegate_stays_nameless_at_every_phase() {
        let messages = run("fanout across agents please").await;
        let nameless = phases(&messages, "fo-nameless");
        assert_eq!(nameless.len(), 2, "it opens and closes");
        for event in nameless {
            assert!(
                event.name.is_none(),
                "a guessed name becomes this trajectory's durable name"
            );
        }
    }

    /// **`span` on the close as well as the open**, and it is load-bearing rather than tidy.
    ///
    /// `history::consolidate` folds a block's phases into one row keyed by the *closing*
    /// event, and it carries `name` forward from an earlier phase but **not** `span`. So a
    /// nested delegate declaring its parent only on `started` is correctly nested while the
    /// turn is live and re-roots itself the moment the turn settles.
    #[tokio::test]
    async fn a_nested_delegate_declares_its_parent_on_its_close_too() {
        let messages = run("fanout across agents please").await;
        let probe = phases(&messages, "fo-probe");
        assert_eq!(probe.len(), 2);
        for event in &probe {
            assert_eq!(
                event.span.as_deref(),
                Some("fo-survey"),
                "{:?} without a span re-roots once the turn settles",
                event.phase
            );
        }

        // Same rule in `delegate`, where the depth-3 node closes under its parent.
        let messages = run("delegate this please").await;
        for event in phases(&messages, "sub-worker") {
            assert_eq!(event.span.as_deref(), Some("sub-verify"));
        }
    }

    /// The outlier closes **last**, and the failure is on a **different** row.
    ///
    /// A view that conflated "slowest" with "failed" would look right whenever they coincide,
    /// so they must not. And the audit delegate closes *before* the outlier: closing it last
    /// made its duration tie, and a tie has no unique slowest row — any spec asserting one
    /// would be asserting a coin flip.
    #[tokio::test]
    async fn the_slowest_delegate_is_not_the_failed_one_and_closes_last() {
        let messages = run("fanout across agents please").await;
        let all = events(&messages);
        let closed_at = |stream_id: &str| {
            all.iter()
                .position(|event| {
                    event.kind == EventKind::Subagent
                        && event.stream_id.as_deref() == Some(stream_id)
                        && event.phase == Some(Phase::Completed)
                })
                .unwrap_or_else(|| panic!("{stream_id} never closed"))
        };

        let survey = closed_at("fo-survey");
        for sibling in ["fo-format", "fo-lint", "fo-typecheck", "fo-audit", "fo-compile"] {
            assert!(
                closed_at(sibling) < survey,
                "{sibling} must close before the outlier, or the slowest row is a tie"
            );
        }

        // The failure is on a fast row.
        let typecheck = all
            .iter()
            .find(|event| {
                event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("fo-typecheck")
                    && event.phase == Some(Phase::Completed)
            })
            .unwrap();
        assert!(typecheck.is_error);
        let survey_close = all
            .iter()
            .find(|event| {
                event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("fo-survey")
                    && event.phase == Some(Phase::Completed)
            })
            .unwrap();
        assert!(!survey_close.is_error, "slowest and failed must be different rows");
    }

    /// **Five of six delegates forward an event, and one deliberately does not.**
    ///
    /// A settled turn's transcript comes from consolidated history, where a trajectory's
    /// `started` phase is folded away — so the only remaining lower bound on when it ran is
    /// the first event it *forwarded*. A delegate that forwards nothing cannot be timed at
    /// all once the turn settles, and five of six forwarding nothing made the settled
    /// comparison almost entirely untimed. `Audit` is the exception on purpose, so "closed
    /// having forwarded nothing" stays a covered state.
    #[tokio::test]
    async fn every_fanout_delegate_forwards_work_except_the_one_that_deliberately_does_not() {
        let messages = run("fanout across agents please").await;
        let forwarded = |span: &str| {
            events(&messages)
                .iter()
                .any(|event| event.span.as_deref() == Some(span) && event.kind != EventKind::Subagent)
        };
        for name in FANOUT_DELEGATES {
            let id = format!("fo-{}", name.to_lowercase());
            if *name == "Audit" {
                assert!(!forwarded(&id), "Audit is the deliberate no-interior case");
            } else {
                assert!(forwarded(&id), "{name} must forward work, or it cannot be timed");
            }
        }
    }

    /// **`unprompted` speaks after the turn settled**, and the ordering is the contract.
    ///
    /// *Measured on `claude` 2.x:* a session with backgrounded work emits the delegate's
    /// completion after its turn's `result`, then opens an entire exchange of its own to
    /// report it. Those events used to be dropped for arriving outside a turn, so the
    /// completion a user was promised was discarded by the transport.
    ///
    /// The `turn`/`started` bracket arrives **after** the first out-of-turn event, which is
    /// what the vendor does — a server bracketing on it would drop the payload and keep the
    /// announcement.
    #[tokio::test]
    async fn work_reported_after_a_turn_settles_arrives_before_its_own_turn_bracket() {
        let adapter = E2eAdapter::new();
        let mut session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();
        let mut stream = session.events().unwrap();
        session
            .send(TurnInput {
                text: "unprompted please".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        // Collect through the first settle and on to the second, which is what the late
        // report produces.
        let mut collected = Vec::new();
        let mut settles = 0;
        while let Some(message) = stream.next().await {
            let message = message.unwrap();
            if let SessionMessage::Event(event) = &message {
                if event.kind == EventKind::Turn && event.phase == Some(Phase::Completed) {
                    settles += 1;
                }
            }
            collected.push(message);
            if settles == 2 {
                break;
            }
        }
        assert_eq!(settles, 2, "the agent opens a second turn of its own");

        let all = events(&collected);
        let first_settle = all
            .iter()
            .position(|event| event.kind == EventKind::Turn && event.phase == Some(Phase::Completed))
            .unwrap();
        let late_close = all
            .iter()
            .position(|event| {
                event.kind == EventKind::Subagent
                    && event.stream_id.as_deref() == Some("up-task")
                    && event.phase == Some(Phase::Completed)
            })
            .expect("the delegate's completion must arrive");
        let bracket = all
            .iter()
            .position(|event| event.kind == EventKind::Turn && event.phase == Some(Phase::Started))
            .expect("the agent's own turn must be bracketed");

        assert!(
            first_settle < late_close,
            "the completion must arrive after the turn settled, or nothing tests the drop"
        );
        assert!(
            late_close < bracket,
            "the payload precedes its bracket, which is what the vendor does"
        );
        // Its span names a host from the turn that already ended, which is what makes this a
        // report about earlier work rather than something new.
        assert_eq!(all[late_close].span.as_deref(), Some("up-host"));
    }

    /// A second `events()` call answers `None`.
    ///
    /// Two consumers of one session would each see half the turn, so this is a bug rather
    /// than a second subscription.
    #[tokio::test]
    async fn a_session_yields_its_stream_exactly_once() {
        let adapter = E2eAdapter::new();
        let mut session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::New,
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();
        assert!(session.events().is_some());
        assert!(session.events().is_none(), "a second call must not resubscribe");
    }

    /// The server's dictated id is honoured, so identity is known before the first event.
    #[tokio::test]
    async fn a_dictated_native_id_is_the_one_the_session_reports() {
        let adapter = E2eAdapter::new();
        let session = adapter
            .run(RunSpec {
                workdir: "/tmp".into(),
                native_id: RequestedNativeId::Dictate(NativeId::new("chosen-by-the-server")),
                inject_identity: false,
                identity_preamble: None,
                flags: BTreeMap::new(),
                permission_mode: None,
            })
            .await
            .unwrap();
        assert_eq!(session.native_id().as_str(), "chosen-by-the-server");
        assert!(adapter.probe().await.unwrap().dictates_native_id);
    }

    /// **This agent has no native store**, so `adopt` and `watch` refuse rather than
    /// synthesizing one.
    ///
    /// Stated as a test because the alternative is tempting and wrong: a fixture writing its
    /// own store would have the suite assert against records Sylph itself wrote, which tests
    /// the fixture rather than the parser.
    #[tokio::test]
    async fn adopt_and_watch_refuse_because_there_is_no_native_store() {
        let adapter = E2eAdapter::new();
        let id = NativeId::new("e2e-native-session");
        assert!(adapter.adopt(&id).await.is_err());
        assert!(adapter.watch(&id).await.is_err());
        // An empty listing is the truthful answer for a store-less agent, not an error.
        assert!(adapter
            .enumerate(&StoreFilter::default())
            .await
            .unwrap()
            .is_empty());
    }

    /// Scenario selection is by substring, and each token reaches its own scenario.
    ///
    /// Cheap, and it pins the routing every spec's prompt depends on: a token that stopped
    /// matching would fail a dozen specs with no hint that the fixture chose the fallthrough.
    #[test]
    fn every_scenario_token_selects_its_own_scenario() {
        for (token, expected) in Scenario::TOKENS {
            assert_eq!(
                Scenario::of(&format!("please {token} now")),
                *expected,
                "{token} must select its own scenario"
            );
        }
        // The fallthrough is a real case with a name.
        assert_eq!(Scenario::of("hello"), Scenario::Reply);
        assert_eq!(Scenario::of(""), Scenario::Reply);
    }

    /// The four blocking scenarios are exactly the ones that raise a request.
    ///
    /// If this drifted, a turn would either settle without asking or hang forever — and both
    /// read as a product bug rather than as a fixture one.
    #[test]
    fn exactly_the_request_raising_scenarios_block() {
        for blocking in [
            Scenario::Approval,
            Scenario::Narrow,
            Scenario::Question,
            Scenario::Elicit,
        ] {
            assert!(blocking.blocks_on_a_request(), "{blocking:?} raises a request");
        }
        for settling in [
            Scenario::Bulk,
            Scenario::Tools,
            Scenario::Delegate,
            Scenario::Unprompted,
            Scenario::Promote,
            Scenario::Paced,
            Scenario::Fanout,
            Scenario::Reply,
        ] {
            assert!(!settling.blocks_on_a_request(), "{settling:?} settles its own turn");
        }
    }
}
