//! The integration suite's agent, as an in-process [`Adapter`].
//!
//! # Why this replaced a shell script, and what that changed
//!
//! It used to be a real Python `adapter.sh` installed into a temp directory, and the
//! harness's comment said why: it kept the supervisor on its production path — spawning a
//! process, reading NDJSON from its stdout, parsing envelopes — without paying for a model
//! call. That path no longer exists. The server holds a [`Session`] obtained from an
//! [`Adapter`], so a subprocess speaking NDJSON is no longer the production path but a
//! *simulation* of a deleted one, and a fixture that exercises deleted code is worse than
//! no fixture at all.
//!
//! What survives is every observable **shape**: the same agent names, the same flags, the
//! same config option with the same refusable value, and one behaviour per real failure
//! mode. Those are what the specs assert on.
//!
//! # Why the source is injected rather than registered by name
//!
//! Tests run concurrently in one process and several of them want an agent called `mock`
//! or `agent` with *different* behaviour. A name-keyed global registry cannot answer that —
//! `sylph_adapters::for_agent` has no way to know which test is asking — so each
//! [`super::TestBackend`] owns its own [`MockAgents`] and hands it to
//! `AppState::build_with_adapters`. That is also what makes "the agent was uninstalled"
//! expressible ([`MockAgents::uninstall`]) now that there is no directory to delete.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use sylph_protocol::{
    Adapter, AdapterError, AgentInfo, Capability, CapabilityFacets, CommandKind, CommandSpec,
    ConfigChoice, ConfigKind, ConfigOption, Event, EventKind, EventStream, Fidelity, FlagSchema,
    FlagSpec, NativeId, NativeSummary, Phase, RequestedNativeId, Resolution, RunSpec, Session,
    SessionCapabilities, SessionMessage, StoreFilter, SubagentSpec, TurnInput,
};
use sylph_web_backend::adapters::AdapterSource;
use tokio::sync::{mpsc, Mutex};

/// The value `set_config` refuses, so the classification path is reachable.
///
/// **Enumerated on purpose.** The server validates a value against what the agent offered
/// before forwarding it, so an unlisted value never reaches an adapter and could not
/// exercise `INVALID_VALUE` at all.
const REFUSED_VALUE: &str = "refused";

/// What an agent does when a turn arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behavior {
    /// Stream a full turn, then report it completed.
    Success,
    /// Report the turn failed.
    Failure,
    /// Complete the turn with no assistant output at all.
    ///
    /// Under v1 this was "emit no final envelope", a protocol violation the supervisor had
    /// to detect. There is no envelope to omit now: the `turn` event *is* the outcome. The
    /// nearest real failure is an agent that ends a turn having produced nothing.
    NoOutput,
    /// Dispatch a delegate, end the turn, then report the delegate's completion *after*
    /// the turn has settled — and open a turn of its own to say so.
    ///
    /// What `claude` 2.x actually does with backgrounded work, measured: after a turn's
    /// `result` it emits the delegate's completion and then an entire exchange reporting
    /// it, with stdout still open. Every one of those events used to be dropped for
    /// arriving outside a turn.
    Backgrounded,
    /// The same shape, with the delegate declaring `is_backgrounded` on every phase.
    ///
    /// What a *cooperating* adapter sends: `claude` reports the flag only on
    /// `task_started`, so its adapter re-attaches it to each later phase. The turn-settle
    /// sweep skips such a trajectory, which is the branch [`Behavior::Backgrounded`]
    /// cannot reach — that one exercises every adapter that reports nothing, which is all
    /// of them except `claude`.
    BackgroundedFlagged,
    /// Accept the turn and never finish it, so a stop or interrupt can arrive.
    Slow,
    /// Fail `probe`, so the agent is present and unusable.
    ///
    /// The v3 equivalent was failing the `hello` handshake. `probe` is where that lands
    /// now, and it is a genuinely different failure from an absent CLI: the binary is
    /// there and will not say what it is.
    BrokenProbe,
}

/// One agent the harness offers, named and with a behaviour.
///
/// The constructors keep their v3 names because ~55 call sites use them; what changed is
/// that `install` is gone — there is no directory to write a script into.
pub struct MockRunner {
    name: String,
    behavior: Behavior,
}

impl MockRunner {
    pub fn success(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Success }
    }

    pub fn failure(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Failure }
    }

    pub fn no_envelope(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::NoOutput }
    }

    pub fn slow(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Slow }
    }

    /// An adapter that reports a delegate finishing after its turn ended.
    pub fn backgrounded(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Backgrounded }
    }

    pub fn backgrounded_flagged(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::BackgroundedFlagged }
    }

    pub fn broken_describe(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::BrokenProbe }
    }
}

/// The agents one [`super::TestBackend`] can run.
#[derive(Clone, Default)]
pub struct MockAgents {
    agents: Arc<RwLock<HashMap<String, Behavior>>>,
}

impl MockAgents {
    pub fn new(runners: &[MockRunner]) -> Self {
        let agents = runners
            .iter()
            .map(|runner| (runner.name.clone(), runner.behavior))
            .collect();
        Self { agents: Arc::new(RwLock::new(agents)) }
    }

    /// Make an agent absent, as uninstalling its CLI would.
    ///
    /// This is what `std::fs::remove_dir_all(agents_dir().join(name))` used to express.
    /// Asking the harness rather than the filesystem is what keeps a test about the
    /// *detached* path from silently stopping to produce it — a literal directory name was
    /// a second place the layout was written down, and it removed nothing the moment that
    /// layout changed.
    pub fn uninstall(&self, name: &str) {
        self.agents
            .write()
            .expect("the mock agent map is not poisoned")
            .remove(name);
    }
}

impl AdapterSource for MockAgents {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        let behavior = *self
            .agents
            .read()
            .expect("the mock agent map is not poisoned")
            .get(name)?;
        Some(Box::new(MockAdapter { name: name.to_string(), behavior }))
    }

    fn available(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .agents
            .read()
            .expect("the mock agent map is not poisoned")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn is_known(&self, name: &str) -> bool {
        self.agents
            .read()
            .expect("the mock agent map is not poisoned")
            .contains_key(name)
    }
}

struct MockAdapter {
    name: String,
    behavior: Behavior,
}

#[async_trait]
impl Adapter for MockAdapter {
    async fn probe(&self) -> Result<AgentInfo> {
        if self.behavior == Behavior::BrokenProbe {
            return Err(anyhow!("handshake is broken"));
        }
        let mut flags = FlagSchema::new();
        flags.insert(
            "model".to_string(),
            FlagSpec {
                choices: vec!["fast".to_string(), "slow".to_string()],
                description: "Mock model".to_string(),
            },
        );
        flags.insert(
            "effort".to_string(),
            FlagSpec {
                choices: vec!["low".to_string(), "high".to_string()],
                description: "Mock effort".to_string(),
            },
        );
        Ok(AgentInfo {
            name: self.name.clone(),
            version: Some("0.0.0".to_string()),
            flags,
            // The three the vocabulary has. The v3 fixture also declared `approval` and
            // `slash`, which `sylph_protocol::Capability` deliberately does not model: an
            // approval is a `Request` the agent raises and slash commands are the
            // `commands` facet, so both are visible from the surface itself rather than
            // from a claim beside it.
            supports: vec![
                Capability::Interrupt,
                Capability::Steer,
                Capability::SetConfig,
            ],
            // **False on purpose, and it splits the coverage.** `codex` assigns its own id
            // and reports it, `claude` and `pi` accept a dictated one, and the server takes
            // a different branch per agent — so both need exercising. The E2E fixture
            // declares `true`, which leaves this one to cover the branch where the id the
            // *adapter* reports becomes the session's primary key. That is also the branch
            // where a fixture answering one constant id would make the second session in
            // any test a `409`, which is why `run` mints a unique one.
            dictates_native_id: false,
            native_fidelity: Fidelity::Blocks,
        })
    }

    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
        // The id the server named, echoed back — which is what a real adapter must do, and
        // what `claude`'s got wrong on the resume branch, making every session unlinkable
        // once its first link died.
        //
        // A **unique** id when the server names none, because the id an adapter reports is
        // the session's primary key (`docs/architecture.md` § Identity): a fixture
        // answering one constant makes the second session in any test a `409`, the
        // constraint correctly refusing two rows for one conversation.
        let native_id = match spec.native_id {
            RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => id,
            RequestedNativeId::New => {
                NativeId::new(format!("mock-session-{}", uuid::Uuid::new_v4()))
            }
        };
        Ok(Box::new(MockSession::new(native_id, self.behavior)))
    }

    /// Refused: this agent writes no native store, so there is nothing to parse.
    ///
    /// Synthesizing one would make a test assert against records the harness itself wrote.
    async fn adopt(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!("the mock agent has no native store"))
    }

    async fn watch(&self, _id: &NativeId) -> Result<EventStream> {
        Err(anyhow!("the mock agent has no native store"))
    }

    async fn enumerate(&self, _filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(Vec::new())
    }
}

/// The config surface, which is one option with one refusable value.
fn config_options(current: &str) -> Vec<ConfigOption> {
    let choice = |value: &str, name: &str| ConfigChoice {
        value: value.to_string(),
        name: name.to_string(),
        description: None,
    };
    vec![ConfigOption {
        id: "model".to_string(),
        name: "Model".to_string(),
        description: None,
        category: Some("model".to_string()),
        kind: ConfigKind::Select,
        current_value: Some(json!(current)),
        // The same three every time. A narrower list after a successful set would retract
        // `refused`, so a test that set a value and then tried the refusable one would be
        // refused by the *server* for offering an unknown value — a refusal from the wrong
        // layer, and one that reads as this classification working when it was never
        // reached.
        options: vec![
            choice("fast", "Fast"),
            choice("slow", "Slow"),
            choice(REFUSED_VALUE, "Refused"),
        ],
    }]
}

fn initial_surface() -> SessionCapabilities {
    SessionCapabilities {
        commands: vec![
            CommandSpec {
                name: "noop".to_string(),
                description: "Does nothing".to_string(),
                arg_hint: None,
                source: None,
                kind: CommandKind::Prompt,
                config_id: None,
                aliases: Vec::new(),
            },
            CommandSpec {
                name: "model".to_string(),
                description: "Pick a model".to_string(),
                arg_hint: None,
                source: None,
                kind: CommandKind::Control,
                config_id: Some("model".to_string()),
                aliases: Vec::new(),
            },
        ],
        subagents: vec![SubagentSpec {
            name: "Explore".to_string(),
            description: "Search".to_string(),
        }],
        config: config_options("fast"),
        permission_mode: Some("default".to_string()),
        context_window: None,
    }
}

/// One live mock session.
struct MockSession {
    native_id: NativeId,
    behavior: Behavior,
    stream: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    emit: mpsc::Sender<Result<SessionMessage>>,
    surface: Arc<Mutex<SessionCapabilities>>,
}

impl MockSession {
    fn new(native_id: NativeId, behavior: Behavior) -> Self {
        // Generously bounded: `Behavior::Success` emits its whole turn as fast as it can,
        // and a channel small enough to block there would make the fixture's own
        // backpressure part of what a test measures.
        let (emit, stream) = mpsc::channel(1024);
        Self {
            native_id,
            behavior,
            stream: Mutex::new(Some(stream)),
            emit,
            surface: Arc::new(Mutex::new(initial_surface())),
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Build one event, naming only the fields a behaviour needs.
///
/// Every optional field defaults to **absent**, so a name this fixture does not know stays
/// absent rather than becoming a placeholder — a journal makes every guess permanent.
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

    fn failed(mut self) -> Self {
        self.0.is_error = true;
        self
    }
}

/// One send, so a closed receiver — the server dropped the session mid-turn — is not an
/// error worth reporting. The scenario simply stops being observed, which is what a
/// detached link means.
async fn emit(tx: &mpsc::Sender<Result<SessionMessage>>, event: Ev) {
    let _ = tx.send(Ok(SessionMessage::Event(event.0))).await;
}

#[async_trait]
impl Session for MockSession {
    fn native_id(&self) -> &NativeId {
        &self.native_id
    }

    async fn capabilities(&self) -> Result<SessionCapabilities> {
        Ok(self.surface.lock().await.clone())
    }

    /// Accept the turn and run it on its own task.
    ///
    /// Returning before the turn finishes is the contract, and it is also what makes
    /// interrupt testable: a turn that blocked here could not be interrupted, because the
    /// session would be unreachable until it finished.
    async fn send(&self, _turn: TurnInput) -> Result<()> {
        let tx = self.emit.clone();
        let behavior = self.behavior;
        tokio::spawn(async move { run_turn(tx, behavior).await });
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        emit(
            &self.emit,
            Ev::new(EventKind::Turn, Some(Phase::Completed))
                .text("interrupted")
                .data(json!({"stopReason": "interrupted"})),
        )
        .await;
        Ok(())
    }

    async fn steer(&self, _text: &str, _expected_turn_id: Option<&str>) -> Result<()> {
        Ok(())
    }

    /// Two refusals with two codes, which is the whole point of this method here.
    ///
    /// An option that does not exist is `UNSUPPORTED_FIELD` and justifies disabling a
    /// control; one real value refused right now is `INVALID_VALUE` and must leave it
    /// usable. The server classifies on the code, so a fixture that can only produce one
    /// of them cannot observe the distinction being erased.
    async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
        if id != "model" {
            return Err(anyhow::Error::new(AdapterError::unsupported_field(
                "no such option",
            )));
        }
        if value.as_str() == Some(REFUSED_VALUE) {
            return Err(anyhow::Error::new(AdapterError::invalid_value(
                "that model is not available on this account",
            )));
        }
        let refreshed = config_options(value.as_str().unwrap_or("fast"));
        let mut surface = self.surface.lock().await;
        CapabilityFacets {
            config: Some(refreshed.clone()),
            ..Default::default()
        }
        .apply_to(&mut surface);
        Ok(refreshed)
    }

    async fn resolve(&self, _resolution: Resolution) -> Result<()> {
        Ok(())
    }

    fn events(&mut self) -> Option<EventStream> {
        let stream = self.stream.try_lock().ok()?.take()?;
        Some(Box::pin(futures::stream::unfold(
            stream,
            |mut stream| async move { stream.recv().await.map(|item| (item, stream)) },
        )))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

/// One turn's events, per behaviour.
async fn run_turn(tx: mpsc::Sender<Result<SessionMessage>>, behavior: Behavior) {
    // A per-turn capability update naming **one facet**, which is what `claude` does: its
    // `system/init` arrives once per *turn* and carries only the permission mode. This is
    // the fixture's whole reason for having it — without it no test can observe a partial
    // update erasing the facets it never mentioned, and that bug shipped once already.
    let _ = tx
        .send(Ok(SessionMessage::Capabilities(CapabilityFacets {
            permission_mode: Some("default".to_string()),
            ..Default::default()
        })))
        .await;

    match behavior {
        Behavior::Success | Behavior::BrokenProbe => success(&tx).await,
        Behavior::Failure => {
            emit(
                &tx,
                Ev::new(EventKind::Turn, Some(Phase::Completed))
                    .text("mock runner failed on purpose")
                    .failed(),
            )
            .await;
        }
        Behavior::NoOutput => {
            emit(
                &tx,
                Ev::new(EventKind::Turn, Some(Phase::Completed)).failed(),
            )
            .await;
        }
        Behavior::Backgrounded => backgrounded(&tx, false).await,
        Behavior::BackgroundedFlagged => backgrounded(&tx, true).await,
        // Never completes: the turn stays in flight until interrupted or the link is torn
        // down, which is what a stop test needs.
        Behavior::Slow => {
            emit(
                &tx,
                Ev::new(EventKind::Status, Some(Phase::Started)).text("working"),
            )
            .await;
        }
    }
}

async fn success(tx: &mpsc::Sender<Result<SessionMessage>>) {
    emit(
        tx,
        Ev::new(EventKind::Thinking, Some(Phase::Started)).stream_id("t1"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Thinking, Some(Phase::Delta))
            .stream_id("t1")
            .text("think"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Thinking, Some(Phase::Completed))
            .stream_id("t1")
            .text("thinking done"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::ToolCall, Some(Phase::Completed))
            .stream_id("c1")
            .name("Read")
            .data(json!({"path": "/tmp/x"})),
    )
    .await;
    // Deltas must concatenate to the completed text, exactly as a real agent's do — the
    // fold builds the completed event by accumulating them, and a fixture whose deltas
    // disagree makes it refuse. That is correct behaviour on bad input and useless as a
    // test of the good path.
    emit(
        tx,
        Ev::new(EventKind::Assistant, Some(Phase::Delta))
            .stream_id("a1")
            .text("mock "),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Assistant, Some(Phase::Delta))
            .stream_id("a1")
            .text("reply"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("a1")
            .text("mock reply"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Usage, Some(Phase::Completed))
            .data(json!({"inputTokens": 10, "outputTokens": 3})),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Turn, Some(Phase::Completed)).text("mock reply"),
    )
    .await;
}

/// A host that dispatches and returns, whose delegate outlives the turn.
///
/// `flagged` is whether the delegate declares `is_backgrounded` on every phase, which is
/// what a *cooperating* adapter sends. The vendor reports it on the open only, so an
/// adapter re-attaches it — and the turn-settle sweep has to find it from whichever phase
/// carried it rather than from the phase it happens to be looking at.
async fn backgrounded(tx: &mpsc::Sender<Result<SessionMessage>>, flagged: bool) {
    emit(
        tx,
        Ev::new(EventKind::ToolCall, Some(Phase::Completed))
            .stream_id("bg-host")
            .name("Workflow")
            .text(r#"{"script": "background(worker)"}"#),
    )
    .await;

    let mut open = Ev::new(EventKind::Subagent, Some(Phase::Started))
        .stream_id("bg-task")
        .span("bg-host")
        .name("Background: worker");
    if flagged {
        open = open.data(json!({"is_backgrounded": true}));
    }
    emit(tx, open).await;

    emit(
        tx,
        Ev::new(EventKind::ToolResult, Some(Phase::Completed))
            .stream_id("bg-host")
            .name("Workflow")
            .text("dispatched"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("bg-a1")
            .text("dispatched"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Turn, Some(Phase::Completed)).text("dispatched"),
    )
    .await;

    // Everything below arrives with the turn **already settled**, which is the point.
    // Before agent-initiated turns existed these were dropped, so the completion never
    // reached a client and the delegate stayed abandoned forever.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // The real outcome, with the flag *absent* even in the flagged case — exactly as the
    // vendor sends a close.
    emit(
        tx,
        Ev::new(EventKind::Subagent, Some(Phase::Completed))
            .stream_id("bg-task")
            .span("bg-host")
            .text("worker finished"),
    )
    .await;
    emit(tx, Ev::new(EventKind::Turn, Some(Phase::Started))).await;
    emit(
        tx,
        Ev::new(EventKind::Assistant, Some(Phase::Completed))
            .stream_id("bg-a2")
            .text("the background worker finished"),
    )
    .await;
    emit(
        tx,
        Ev::new(EventKind::Turn, Some(Phase::Completed))
            .text("the background worker finished"),
    )
    .await;
}
