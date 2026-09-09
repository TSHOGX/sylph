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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use sylph_protocol::{
    Adapter, AdapterError, AgentInfo, Capability, CapabilityFacets, CommandKind, CommandSpec,
    ConfigChoice, ConfigKind, ConfigOption, Event, EventKind, EventStream, Fidelity, FlagSchema,
    FlagSpec, NativeId, NativeRecord, NativeSummary, Phase, RequestedNativeId, Resolution, RunSpec,
    Session, SessionCapabilities, SessionMessage, StoreFilter, SubagentSpec, TurnInput,
};
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
    /// End the turn with many delegations open, so the settle sweep is measurably wide.
    ///
    /// **The width is the instrument.** The sweep closes what a turn left open, and the
    /// property under test is that a turn does not read terminal until it has. That
    /// property is about an *interleaving*, so a test can only observe it while the sweep
    /// is still running — and one open trajectory makes the sweep a single query and a
    /// single emit, narrow enough that a poll almost never lands inside it. A test written
    /// against that passes whether the ordering is right or wrong, which is the shape
    /// `AGENTS.md` names: a property observable only through timing needs the window opened
    /// on purpose.
    AbandonsMany,
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
    /// Declares no native store: `adopt` and `watch` refuse, `enumerate` answers empty.
    ///
    /// A real vendor state rather than a stub — the E2E fixture is exactly this agent, and
    /// `pi` is this agent early in a turn, before persistence has written anything. It is
    /// what the two "an agent with no native store" tests name, and it must stay
    /// distinguishable from an agent whose store is merely *empty*: the first cannot ever
    /// answer, the second may answer tomorrow.
    NoStore,
    /// Fail `probe`, so the agent is present and unusable.
    ///
    /// The v3 equivalent was failing the `hello` handshake. `probe` is where that lands
    /// now, and it is a genuinely different failure from an absent CLI: the binary is
    /// there and will not say what it is.
    BrokenProbe,
    /// Emit two different native payloads at one source revision, then a normalized event.
    /// The history store rejects the second record; the live drain must stop there.
    RawRetentionFailure,
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
        Self {
            name: name.to_string(),
            behavior: Behavior::Success,
        }
    }

    pub fn failure(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::Failure,
        }
    }

    pub fn no_envelope(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::NoOutput,
        }
    }

    pub fn slow(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::Slow,
        }
    }

    /// An adapter that reports a delegate finishing after its turn ended.
    pub fn backgrounded(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::Backgrounded,
        }
    }

    /// An adapter that ends its turn with [`OPEN_TRAJECTORIES`] delegations still open.
    pub fn abandons_many(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::AbandonsMany,
        }
    }

    pub fn backgrounded_flagged(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::BackgroundedFlagged,
        }
    }

    pub fn broken_describe(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::BrokenProbe,
        }
    }

    /// An agent with no native store to read.
    pub fn no_store(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::NoStore,
        }
    }

    pub fn raw_retention_failure(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: Behavior::RawRetentionFailure,
        }
    }
}

/// The agents one [`super::TestBackend`] can run.
#[derive(Clone, Default)]
pub struct MockAgents {
    agents: Arc<RwLock<HashMap<String, Behavior>>>,
    /// Names compiled into this fixture, including agents currently unavailable.
    /// Keeping this separate from `agents` preserves the protocol distinction between
    /// `AGENT_UNKNOWN` and a known vendor whose CLI was removed after pairing.
    known: Arc<RwLock<HashSet<String>>>,
    /// The workdir every stored session reports.
    ///
    /// Set by the harness to a directory inside its own tree, because adopting runs the
    /// recorded path through the same containment bound `POST /sessions` uses — so a
    /// fixture reporting a path outside the roots would be testing the refusal rather than
    /// the adopt. `None` reports a session the store recorded no directory for, which is
    /// the `NATIVE_SESSION_NOT_RESUMABLE` case.
    store_workdir: Arc<RwLock<Option<String>>>,
}

impl MockAgents {
    pub fn new(runners: &[MockRunner]) -> Self {
        let agents = runners
            .iter()
            .map(|runner| (runner.name.clone(), runner.behavior))
            .collect();
        Self {
            agents: Arc::new(RwLock::new(agents)),
            known: Arc::new(RwLock::new(
                runners.iter().map(|runner| runner.name.clone()).collect(),
            )),
            store_workdir: Arc::new(RwLock::new(None)),
        }
    }

    /// Point every stored session at `workdir`.
    pub fn set_store_workdir(&self, workdir: Option<String>) {
        *self
            .store_workdir
            .write()
            .expect("the mock store workdir is not poisoned") = workdir;
    }

    /// The native id the mock's store holds, for a test that adopts it.
    pub fn stored_native_id() -> &'static str {
        STORED_NATIVE_ID
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

    /// Make an agent available again, as installing or upgrading its CLI would.
    pub fn install(&self, name: &str) {
        self.agents
            .write()
            .expect("the mock agent map is not poisoned")
            .insert(name.to_string(), Behavior::Success);
    }

    /// Snapshot this fixture's adapter inventory in the shape an envagent hello carries.
    pub async fn inventory(&self) -> Vec<sylph_protocol::AgentInventory> {
        let mut names = self.available();
        names.sort();
        let mut inventory = Vec::with_capacity(names.len());
        for name in names {
            if let Some(adapter) = self.adapter_for(&name) {
                match adapter.probe().await {
                    Ok(info) => inventory.push(sylph_protocol::AgentInventory::available(info)),
                    Err(error) => inventory.push(sylph_protocol::AgentInventory::unavailable(
                        name,
                        None,
                        error.to_string(),
                    )),
                }
            }
        }
        inventory
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

    fn adapter_for(&self, name: &str) -> Option<Box<dyn Adapter>> {
        let behavior = *self
            .agents
            .read()
            .expect("the mock agent map is not poisoned")
            .get(name)?;
        Some(Box::new(MockAdapter {
            name: name.to_string(),
            behavior,
            store_workdir: self
                .store_workdir
                .read()
                .expect("the mock store workdir is not poisoned")
                .clone(),
        }))
    }

    pub async fn infos(&self) -> Vec<AgentInfo> {
        self.inventory()
            .await
            .into_iter()
            .filter_map(|entry| entry.info)
            .collect()
    }
}

impl sylph_envagent::adapters::AdapterSource for MockAgents {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        self.adapter_for(name)
    }

    /// The same set as [`Self::available`], because a mock agent has no CLI to be missing.
    ///
    /// The two differ in production — `codex` is known and may not be installed — and that
    /// distinction is what lets a message name a fix. Here every agent this source knows is
    /// one it can run, so collapsing them is honest rather than lossy.
    fn known_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .known
            .read()
            .expect("the mock known-agent map is not poisoned")
            .iter()
            .cloned()
            .collect();
        names.sort();
        names
    }
}

struct MockAdapter {
    name: String,
    behavior: Behavior,
    /// Snapshotted at resolve time, so a test that sets it before creating a session and a
    /// listing taken afterwards agree.
    store_workdir: Option<String>,
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

    /// One session's worth of events, for the agents that declare a store.
    ///
    /// # A store-*less* agent is a separate behaviour, and both are needed
    ///
    /// This used to refuse unconditionally, on the reasoning that synthesizing a store
    /// would make a test assert against records the harness itself wrote. That is right
    /// about a *parser* and wrong about this endpoint: `POST /sessions/adopt` does not
    /// parse a vendor file, it ingests whatever an adapter yields — so what a test needs
    /// here is an adapter that yields something, and the parsers have their own corpus
    /// tests in `crates/adapters`.
    ///
    /// The gap that hid: **all 13 adopt tests asserted a refusal**, because no mock could
    /// answer. A `201` path existed in the handler and nothing exercised it, so ingestion,
    /// the fidelity check, and the workdir containment on an adopted row were all
    /// unobserved.
    ///
    /// [`Behavior::NoStore`] keeps the refusing shape, which is a real vendor state — the
    /// E2E fixture is exactly that agent — and is what the two tests about "an agent with
    /// no native store" now name.
    async fn adopt(&self, id: &NativeId) -> Result<EventStream> {
        if self.behavior == Behavior::NoStore {
            return Err(anyhow!("the {} agent has no native store", self.name));
        }
        Ok(Box::pin(futures::stream::iter(stored_events(id))))
    }

    /// The same first pass as [`Self::adopt`], then nothing further.
    ///
    /// A real `watch` follows the store as it grows. This ends instead, and the difference
    /// is worth stating rather than glossing: a test asserting that a watched session keeps
    /// receiving events would pass vacuously here, so `watch` coverage belongs against a
    /// real adapter. What this does support is the *shape* — a first pass that ingests.
    async fn watch(&self, id: &NativeId) -> Result<EventStream> {
        if self.behavior == Behavior::NoStore {
            return Err(anyhow!("the {} agent has no native store", self.name));
        }
        Ok(Box::pin(futures::stream::iter(stored_events(id))))
    }

    /// What this machine's store holds: one session, or none for a store-less agent.
    ///
    /// **Every field the vendor would not record is absent, not defaulted.** A synthesized
    /// title or timestamp is a lie a journal makes permanent, and this row is what a picker
    /// renders — so the only fields set are the ones a real store genuinely holds.
    async fn enumerate(&self, _filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        if self.behavior == Behavior::NoStore {
            return Ok(Vec::new());
        }
        Ok(vec![NativeSummary {
            native_id: NativeId::new(STORED_NATIVE_ID),
            // Set by the harness, because containment is the interesting part of adopting:
            // a workdir is the `current_dir` of a spawned agent, so an adopted row's
            // directory goes through the same bound `POST /sessions` does.
            workdir: self.store_workdir.clone(),
            title: Some("A session read from a store".to_string()),
            started_at_ms: Some(1_700_000_000_000),
            ended_at_ms: Some(1_700_000_060_000),
            event_count: Some(2),
            parent_native_id: None,
            parent_relation: None,
            // **Never `Stream`.** No care recovers a timing that was never written, and the
            // handler refuses `stream` from a store outright — so a fixture reporting it
            // would be testing that refusal rather than the adopt path.
            fidelity: Fidelity::Blocks,
        }])
    }
}

/// The native id the mock's store holds.
const STORED_NATIVE_ID: &str = "stored-session";

/// What one stored session parses to: a user turn and an assistant reply.
///
/// Deliberately minimal, and `Blocks` fidelity by construction — there are no `delta`
/// phases, because a native store does not hold them. A fixture that emitted deltas here
/// would let a renderer's per-delta path pass against data no real store can produce.
fn stored_events(id: &NativeId) -> Vec<Result<SessionMessage>> {
    let _ = id;
    vec![
        Ok(SessionMessage::Event(
            Ev::new(EventKind::Turn, Some(Phase::Started)).0,
        )),
        Ok(SessionMessage::Event(
            Ev::new(EventKind::Assistant, Some(Phase::Completed))
                .stream_id("stored-a1")
                .text("read from the store")
                .0,
        )),
        Ok(SessionMessage::Event(
            Ev::new(EventKind::Turn, Some(Phase::Completed))
                .text("read from the store")
                .0,
        )),
    ]
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
    fn native_id(&self) -> Option<NativeId> {
        Some(self.native_id.clone())
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

    async fn send_initial(&self, turn: TurnInput) -> Result<NativeId> {
        self.send(turn).await?;
        Ok(self.native_id.clone())
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

    /// Accepted, so a test can drive the queued path.
    ///
    /// Distinct from [`Self::steer`] only in name here, which is deliberate: this mock exists
    /// to exercise the *server's* routing, and a fixture that refused one of the two could
    /// not observe a route that sent a queue where a steer was asked for. The vendors differ
    /// in what they report afterwards — `pi` emits `queue_update`, the other two report
    /// nothing — and that difference belongs in their own adapters' tests.
    async fn queue(&self, _text: &str) -> Result<()> {
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

    fn launch_flag_for_config(&self, id: &str, value: &Value) -> Option<(String, String)> {
        (id == "model")
            .then(|| {
                value
                    .as_str()
                    .map(|value| (id.to_string(), value.to_string()))
            })
            .flatten()
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
        Behavior::Success | Behavior::BrokenProbe | Behavior::NoStore => success(&tx).await,
        Behavior::RawRetentionFailure => {
            for value in ["first", "different"] {
                let raw = format!(r#"{{"value":"{value}"}}"#).into_bytes();
                let _ = tx
                    .send(Ok(SessionMessage::NativeRecord(NativeRecord {
                        source: "root.jsonl".to_string(),
                        version: 1,
                        raw_json: Some(raw),
                        payload: json!({"value": value}),
                        blobs: Vec::new(),
                    })))
                    .await;
            }
            emit(
                &tx,
                Ev::new(EventKind::Assistant, Some(Phase::Completed))
                    .stream_id("after-raw-failure")
                    .text("must not be projected"),
            )
            .await;
            emit(
                &tx,
                Ev::new(EventKind::Turn, Some(Phase::Completed)).text("done"),
            )
            .await;
        }
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
        Behavior::AbandonsMany => abandons_many(&tx).await,
        // Never completes: the turn stays in flight until interrupted or the link is torn
        // down, which is what a stop test needs.
        Behavior::Slow => {
            emit(
                &tx,
                Ev::new(EventKind::Status, Some(Phase::Started)).text("working"),
            )
            .await;
            // Keep the stream alive until the test (or a real caller) sends an
            // interrupt. Dropping this sender immediately would make the
            // supervisor interpret a deliberately running turn as a link
            // loss, creating a timing-dependent idle/running observation.
            std::future::pending::<()>().await;
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
/// How many delegations [`Behavior::AbandonsMany`] leaves open.
///
/// Each one costs the sweep a sequence allocation, a journal append, and a publish, so this
/// widens the window `a_terminal_turn_always_has_its_trajectories_closed` observes.
///
/// **The width is margin, not the instrument** — measured, not assumed, and the first guess
/// here was wrong. Against the pre-fix ordering that test fails 5/5 at this count *and* 5/5
/// at a single trajectory: what catches the defect is polling the row with no sleep, which
/// reads it the instant it goes terminal. The count is kept above one because the suite runs
/// tests concurrently and a wider sweep stays observable under load, not because detection
/// needs it.
pub const OPEN_TRAJECTORIES: usize = 48;

/// Open [`OPEN_TRAJECTORIES`] delegations and end the turn without closing any.
async fn abandons_many(tx: &mpsc::Sender<Result<SessionMessage>>) {
    for index in 0..OPEN_TRAJECTORIES {
        emit(
            tx,
            Ev::new(EventKind::Subagent, Some(Phase::Started))
                .stream_id(&format!("open-{index}"))
                .name(&format!("Worker {index}")),
        )
        .await;
    }

    emit(
        tx,
        Ev::new(EventKind::Turn, Some(Phase::Completed)).text("dispatched everything"),
    )
    .await;
}

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
        Ev::new(EventKind::Turn, Some(Phase::Completed)).text("the background worker finished"),
    )
    .await;
}
