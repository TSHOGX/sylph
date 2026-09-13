//! Adapter hosting and ack-gated event delivery.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, RwLock};
use tracing::{debug, warn};

use sylph_protocol::{
    AgentName, EnvironmentId, Event, EventStream, NativeId, RunSpec, Session, SessionId,
    SessionMessage, TurnInput,
};
use sylph_protocol::{EventBatch, ProducerId, ProducerSeq, SourceCursor, WireMessage};

use crate::adapters::AdapterSource;
use crate::attachments::{attachments_dir, stage as stage_attachments};
use crate::path::PathPolicy;
use crate::spool::{Spool, SpoolConfig, SpoolEvent};

/// Keep a prepared adapter's stream draining while the browser is still on
/// the draft screen.  The unbounded hand-off is intentionally local to the
/// envagent: it prevents a vendor process from blocking on a bounded channel,
/// while promotion still gates publication until the server has a durable row.
fn buffer_event_stream(mut events: sylph_protocol::EventStream) -> sylph_protocol::EventStream {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(message) = events.next().await {
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
}

/// A transport implementation receives a bounded batch and returns the largest
/// contiguous producer sequence durably accepted by the server.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn deliver(&self, events: &[SpoolEvent]) -> Result<u64>;
}

/// Network-backed event sink. A delivery is not considered durable until the
/// matching server ack arrives on the same sink.
pub struct ChannelEventSink {
    sender: tokio::sync::mpsc::Sender<WireMessage>,
    environment_id: EnvironmentId,
    producer_id: ProducerId,
    connection_epoch: u64,
    pending: Arc<Mutex<BTreeMap<u64, tokio::sync::oneshot::Sender<u64>>>>,
    /// Set when the underlying connection has ended. Delivery futures must
    /// be released at that edge so reconnect can replay the durable suffix.
    closed: Arc<AtomicBool>,
}

impl ChannelEventSink {
    pub fn new(
        sender: tokio::sync::mpsc::Sender<WireMessage>,
        environment_id: EnvironmentId,
        producer_id: ProducerId,
        connection_epoch: u64,
    ) -> Self {
        Self {
            sender,
            environment_id,
            producer_id,
            connection_epoch,
            pending: Arc::new(Mutex::new(BTreeMap::new())),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn acknowledge(&self, ack: &sylph_protocol::Ack) {
        // A connection can carry acknowledgements for more than one producer
        // after a reconnect or a protocol upgrade. Never let another
        // producer's prefix release this sink's waiters.
        if ack.producer_id != self.producer_id {
            return;
        }
        let mut pending = self.pending.lock().await;
        let keys: Vec<u64> = pending
            .range(..=ack.accepted_through.get())
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            if let Some(waiter) = pending.remove(&key) {
                let _ = waiter.send(ack.accepted_through.get());
            }
        }
    }

    /// Reject the in-flight delivery on a producer gap.  `EventSink::deliver`
    /// returns a numeric prefix, so wake its waiter with the prefix immediately
    /// before the requested sequence; the runtime's range check then turns it
    /// into a retryable error while preserving every spool row.
    pub async fn reject_gap(&self, expected_next: ProducerSeq) {
        let through = expected_next.get().saturating_sub(1);
        let mut pending = self.pending.lock().await;
        for (_, waiter) in std::mem::take(&mut *pending) {
            let _ = waiter.send(through);
        }
    }

    pub fn is_for_producer(&self, producer_id: &ProducerId) -> bool {
        &self.producer_id == producer_id
    }

    /// Release all deliveries waiting on this connection.
    ///
    /// A peer may close after consuming a frame but before acknowledging it.
    /// Dropping the oneshot senders wakes every waiter with an error, releasing
    /// the runtime send gate while leaving the durable spool row intact.
    pub async fn fail_pending(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending.lock().await.clear();
    }

    async fn register_waiter(&self, sequence: u64) -> Result<tokio::sync::oneshot::Receiver<u64>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("envagent connection is closed"));
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut pending = self.pending.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("envagent connection is closed"));
        }
        pending.insert(sequence, tx);
        Ok(rx)
    }

    async fn remove_waiter(&self, sequence: u64) {
        self.pending.lock().await.remove(&sequence);
    }
}

#[async_trait]
impl EventSink for ChannelEventSink {
    async fn deliver(&self, events: &[SpoolEvent]) -> Result<u64> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut event_group = Vec::new();
        let mut last = 0;
        for row in events {
            if row.kind != "event" {
                if !event_group.is_empty() {
                    last = last.max(self.deliver_event_group(&event_group).await?);
                    event_group.clear();
                }
                last = last.max(self.deliver_diagnostic(row).await?);
                continue;
            }
            event_group.push(row.clone());
        }
        if !event_group.is_empty() {
            last = last.max(self.deliver_event_group(&event_group).await?);
        }
        Ok(last)
    }
}

impl ChannelEventSink {
    async fn deliver_event_group(&self, events: &[SpoolEvent]) -> Result<u64> {
        let mut items = Vec::with_capacity(events.len());
        for row in events {
            let event: Event = serde_json::from_slice(&row.payload)
                .with_context(|| format!("decoding spooled event {}", row.producer_seq))?;
            let session_id = SessionId::decode(&row.session_id)
                .with_context(|| format!("decoding spooled session {}", row.session_id))?;
            let cursor = row
                .source_cursor
                .as_deref()
                .and_then(|raw| serde_json::from_str::<SourceCursor>(raw).ok());
            let mut item = sylph_protocol::SequencedEvent::pending(
                self.producer_id.clone(),
                ProducerSeq::new(row.producer_seq),
                session_id,
                event,
                cursor,
            );
            item.arrival_turn_id = row.arrival_turn_id.clone();
            items.push(item);
        }
        let batch = EventBatch::new(
            self.environment_id.clone(),
            self.producer_id.clone(),
            self.connection_epoch,
            items,
        )
        .map_err(|error| anyhow!(error))?;
        let through = batch.last_seq().get();
        let rx = self.register_waiter(through).await?;
        if self
            .sender
            .send(WireMessage::from_event_batch(batch))
            .await
            .is_err()
        {
            self.remove_waiter(through).await;
            return Err(anyhow!("envagent connection closed while sending events"));
        }
        rx.await
            .map_err(|_| anyhow!("envagent event ack waiter was dropped"))
    }

    async fn deliver_diagnostic(&self, row: &SpoolEvent) -> Result<u64> {
        let envelope: serde_json::Value = serde_json::from_slice(&row.payload)
            .with_context(|| format!("decoding spooled {} {}", row.kind, row.producer_seq))?;
        let session_id = SessionId::decode(&row.session_id).ok();
        let value = envelope.get("payload").cloned().unwrap_or(envelope.clone());
        let message = match row.kind.as_str() {
            "capabilities" => match (
                session_id.clone(),
                serde_json::from_value::<sylph_protocol::CapabilityFacets>(value.clone()),
            ) {
                (Some(session_id), Ok(facets)) => WireMessage::CapabilitySnapshot {
                    session_id,
                    facets,
                    link: None,
                    capabilities: None,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                _ => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            "request" => match (session_id.clone(), serde_json::from_value(value.clone())) {
                (Some(session_id), Ok(request)) => WireMessage::Request {
                    session_id,
                    request,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                _ => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            "native_record" => match (session_id.clone(), serde_json::from_value(value.clone())) {
                (Some(session_id), Ok(record)) => WireMessage::NativeRecord {
                    session_id,
                    record,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                _ => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            "native_blob_chunk" => match (
                session_id.clone(),
                serde_json::from_value::<sylph_protocol::NativeBlobChunk>(value.clone()),
            ) {
                (Some(session_id), Ok(chunk)) => WireMessage::NativeBlobChunk {
                    session_id,
                    chunk,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                _ => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            "source_state" => {
                match serde_json::from_value::<sylph_protocol::SourceState>(value.clone()) {
                    Ok(source) => WireMessage::SourceState {
                        session_id: source.session_id,
                        cursor: source.cursor,
                        state: source.state,
                        detail: source.detail,
                        producer_id: self.producer_id.clone(),
                        producer_seq: ProducerSeq::new(row.producer_seq),
                        connection_epoch: self.connection_epoch,
                    },
                    Err(_) => diagnostic_message(
                        row,
                        session_id,
                        value,
                        self.producer_id.as_str(),
                        self.connection_epoch,
                    ),
                }
            }
            "turn_input" => match (
                session_id.clone(),
                serde_json::from_value::<sylph_protocol::TurnInputFact>(value.clone()),
            ) {
                (Some(session_id), Ok(turn)) => WireMessage::TurnInput {
                    session_id,
                    turn_id: envelope
                        .get("turnId")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    turn,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                _ => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            "edge" => match serde_json::from_value::<sylph_protocol::SessionEdge>(value.clone()) {
                Ok(edge) => WireMessage::Edge {
                    edge,
                    producer_id: self.producer_id.clone(),
                    producer_seq: ProducerSeq::new(row.producer_seq),
                    connection_epoch: self.connection_epoch,
                },
                Err(_) => diagnostic_message(
                    row,
                    session_id,
                    value,
                    self.producer_id.as_str(),
                    self.connection_epoch,
                ),
            },
            _ => diagnostic_message(
                row,
                session_id,
                value,
                self.producer_id.as_str(),
                self.connection_epoch,
            ),
        };
        self.send_with_ack(row.producer_seq, message).await
    }

    async fn send_with_ack(&self, sequence: u64, message: WireMessage) -> Result<u64> {
        let rx = self.register_waiter(sequence).await?;
        if self.sender.send(message).await.is_err() {
            self.remove_waiter(sequence).await;
            return Err(anyhow!("envagent connection closed while sending message"));
        }
        rx.await
            .map_err(|_| anyhow!("envagent message ack waiter was dropped"))
    }
}

fn diagnostic_message(
    row: &SpoolEvent,
    session_id: Option<SessionId>,
    value: serde_json::Value,
    producer_id: &str,
    connection_epoch: u64,
) -> WireMessage {
    WireMessage::Diagnostic {
        level: sylph_protocol::DiagnosticLevel::Info,
        code: format!("spooled_{}", row.kind),
        message: "envagent replayed a non-event message".into(),
        session_id,
        details: BTreeMap::from([("payload".into(), value)]),
        producer_id: ProducerId::parse(producer_id).expect("validated at runtime open"),
        producer_seq: ProducerSeq::new(row.producer_seq),
        connection_epoch,
    }
}

/// Configuration for one environment installation.
#[derive(Debug, Clone)]
pub struct EnvAgentConfig {
    pub environment_id: String,
    pub producer_id: String,
    pub spool: SpoolConfig,
    pub path_policy: PathPolicy,
}

struct IdentityState {
    environment: EnvironmentId,
    agent: AgentName,
    native_id: RwLock<Option<NativeId>>,
    changed: Notify,
}

impl IdentityState {
    fn new(environment: EnvironmentId, agent: AgentName, native_id: Option<NativeId>) -> Self {
        Self {
            environment,
            agent,
            native_id: RwLock::new(native_id),
            changed: Notify::new(),
        }
    }

    async fn session_id(&self) -> Option<SessionId> {
        self.native_id.read().await.clone().map(|native_id| {
            SessionId::new(self.environment.clone(), self.agent.clone(), native_id)
        })
    }

    async fn set_native_id(&self, native_id: NativeId) {
        *self.native_id.write().await = Some(native_id);
        self.changed.notify_waiters();
    }
}

/// A running adapter session and its event pump.
#[derive(Clone)]
pub struct HostedSession {
    identity: Arc<IdentityState>,
    session: Arc<Mutex<Box<dyn Session>>>,
}

/// A native-store source followed by the envagent. It has no control handle,
/// but its observation stream is spooled through the same producer path.
#[derive(Debug, Clone)]
pub struct HostedSource {
    pub session_id: SessionId,
    pub workdir: String,
    pub fidelity: sylph_protocol::Fidelity,
    pub title: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub parent_native_id: Option<NativeId>,
    /// Which relationship [`Self::parent_native_id`] is, as the adapter read it.
    ///
    /// Carried beside the pointer rather than left for the server to infer, because
    /// the vendor's *key* is what distinguishes a delegation from a fork and the
    /// resulting id cannot (`sylph_protocol::NativeSummary::parent_relation`). The
    /// server writes no ancestry edge without it, so dropping it here would silently
    /// flatten every adopted tree — and look exactly like a vendor that recorded no
    /// parent.
    pub parent_relation: Option<sylph_protocol::SessionEdgeRelation>,
}

impl HostedSource {
    /// What `Watch` and `Adopt` return to the server, from one producer.
    ///
    /// Both commands answered with their own hand-written `json!` object, and that is
    /// how `parentRelation` came to be read by the server and written by neither: two
    /// literals of one payload, so a field added to the struct is added to zero, one,
    /// or both of them. The server treats a missing key as "the vendor recorded no
    /// parent", which is indistinguishable from a real root — every remotely adopted
    /// tree flattens and nothing errors.
    ///
    /// Keys are camelCase because this is a wire payload the server reads by name.
    pub fn to_command_result(&self) -> serde_json::Value {
        serde_json::json!({
            "sessionId": self.session_id,
            "workdir": self.workdir,
            "title": self.title,
            "fidelity": self.fidelity,
            "startedAtMs": self.started_at_ms,
            "endedAtMs": self.ended_at_ms,
            "parentNativeId": self.parent_native_id,
            // Both halves or neither: the server refuses to journal an edge whose
            // relation it would have to guess, so a pointer sent without its relation
            // is a session it records as a root.
            "parentRelation": self.parent_relation,
        })
    }
}

/// Which id to open a **new** session under, decided by what the agent reports.
///
/// `Dictate` when [`sylph_protocol::AgentInfo::dictates_native_id`] is true, `New` when it
/// is false. **Takes the probed `AgentInfo` rather than an agent name**, so the fact has one
/// derivation: a list of names held here would diverge from the adapters the moment either
/// grew a case, and the divergence this replaces was total. Both `RunSpec` sites hardcoded
/// `New` while `claude`, `pi`, and the E2E fixture all declare `true`, so `Dictate` was
/// never constructed outside a test — and the two failure modes are why it survived. `pi`
/// refuses `New` outright, so every `pi` session was a `502`; `claude` only pushes a
/// warning and runs on degraded, so it looked like it worked.
///
/// # Why a v4 UUID, and why generating one here is not a second id space
///
/// The value is handed to the vendor on `--session-id` and the vendor adopts it as its own
/// native id, so what Sylph keys on and what the vendor's store holds are one value in one
/// space (`docs/architecture.md` § Identity). What that invariant forbids is a Sylph id
/// held *beside* a vendor id, which is the pair that compares always-false at no layer.
///
/// A v4 UUID because `claude` requires a UUID specifically, and `pi` takes an arbitrary id
/// — the narrower format satisfies both, so there is one spelling rather than one per
/// vendor. Two rejected alternatives: a recognisable `sylph-` prefix, which advertises the
/// id as not-the-vendor's and so invites a reader to treat native ids as two kinds; and a
/// counter or timestamp, because an envagent restarts and a per-process counter repeats
/// ids after a reconnect, colliding on the server's `(environment, agent, native_id)` key
/// — the same reasoning `crates/adapters/src/e2e.rs` records for its own fixture id.
///
/// This is the **envagent's** job and not an adapter's, for the reason each adapter states
/// at its own refusal: an id an adapter minted would never have been given to the vendor,
/// so it would look like the vendor's while being Sylph's. `pi`'s v1 did exactly that with
/// `uuid4()`.
fn requested_native_id_for_new(
    info: &sylph_protocol::AgentInfo,
) -> sylph_protocol::RequestedNativeId {
    if info.dictates_native_id {
        sylph_protocol::RequestedNativeId::Dictate(NativeId::new(uuid::Uuid::new_v4().to_string()))
    } else {
        sylph_protocol::RequestedNativeId::New
    }
}

/// A prepared remote link owned by the envagent.  It has a process and a
/// contained workdir but may not have a durable native identity yet.
#[derive(Clone)]
struct PreparedHosted {
    session: HostedSession,
    events: Arc<Mutex<Option<EventStream>>>,
    workdir: std::path::PathBuf,
    agent: String,
}

impl HostedSession {
    pub fn native_id(&self) -> Option<NativeId> {
        self.identity
            .native_id
            .try_read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub async fn native_id_async(&self) -> Option<NativeId> {
        self.identity.native_id.read().await.clone()
    }

    pub async fn promote_native_id(&self, native_id: NativeId) {
        self.identity.set_native_id(native_id).await;
    }

    pub async fn session_id(&self) -> Option<SessionId> {
        self.identity.session_id().await
    }

    pub async fn send(&self, input: TurnInput) -> Result<()> {
        self.session.lock().await.send(input).await
    }

    pub async fn send_initial(&self, input: TurnInput) -> Result<NativeId> {
        let native_id = self.session.lock().await.send_initial(input).await?;
        self.identity.set_native_id(native_id.clone()).await;
        Ok(native_id)
    }

    pub async fn interrupt(&self) -> Result<()> {
        self.session.lock().await.interrupt().await
    }

    pub async fn steer(&self, text: &str, expected_turn_id: Option<&str>) -> Result<()> {
        self.session
            .lock()
            .await
            .steer(text, expected_turn_id)
            .await
    }

    pub async fn queue(&self, text: &str) -> Result<()> {
        self.session.lock().await.queue(text).await
    }

    pub async fn set_config(
        &self,
        id: &str,
        value: serde_json::Value,
    ) -> Result<Vec<sylph_protocol::ConfigOption>> {
        self.session.lock().await.set_config(id, value).await
    }

    pub async fn launch_flag_for_config(
        &self,
        id: &str,
        value: &serde_json::Value,
    ) -> Option<(String, String)> {
        self.session.lock().await.launch_flag_for_config(id, value)
    }

    pub async fn resolve(&self, resolution: sylph_protocol::Resolution) -> Result<()> {
        self.session.lock().await.resolve(resolution).await
    }

    pub async fn capabilities(&self) -> Result<sylph_protocol::SessionCapabilities> {
        self.session.lock().await.capabilities().await
    }

    pub async fn close(&self) -> Result<()> {
        self.session.lock().await.close().await
    }
}

/// The machine-local envagent runtime.
#[derive(Clone)]
pub struct EnvAgent {
    config: Arc<EnvAgentConfig>,
    environment: EnvironmentId,
    spool: Spool,
    sink: Arc<RwLock<Arc<dyn EventSink>>>,
    send_gate: Arc<Mutex<()>>,
    sessions: Arc<RwLock<std::collections::HashMap<SessionId, HostedSession>>>,
    /// Native-store sources are observable but not controllable.  They still
    /// retain a handle so `Close` can stop a watcher and release its file
    /// descriptors; putting them only in `workdirs` leaked the source task.
    native_sources: Arc<RwLock<HashSet<SessionId>>>,
    workdirs: Arc<RwLock<std::collections::HashMap<SessionId, std::path::PathBuf>>>,
    active_turns: Arc<RwLock<std::collections::HashMap<SessionId, String>>>,
    native_pump_handles:
        Arc<Mutex<std::collections::HashMap<SessionId, tokio::task::JoinHandle<()>>>>,
    /// Event pumps for remote commands wait until their command result has
    /// crossed the wire. This gives the server a deterministic point at which
    /// to create the canonical session row before the first batch arrives.
    deferred_events: Arc<Mutex<std::collections::HashMap<SessionId, Arc<Notify>>>>,
    prepared: Arc<RwLock<std::collections::HashMap<String, PreparedHosted>>>,
    promoted_drafts: Arc<RwLock<std::collections::HashMap<String, SessionId>>>,
    adapters: Arc<dyn AdapterSource>,
}

impl EnvAgent {
    async fn local_turn_input(
        &self,
        input: sylph_protocol::WireTurnInput,
        scope: &str,
        turn_id: &str,
    ) -> Result<TurnInput> {
        let root = self
            .config
            .spool
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let attachments =
            stage_attachments(&attachments_dir(root), scope, turn_id, &input.attachments).await?;
        Ok(TurnInput {
            text: input.text,
            attachments,
            command: input.command,
            args: input.args,
            config: input.config,
        })
    }
}

impl EnvAgent {
    pub async fn open(config: EnvAgentConfig, sink: Arc<dyn EventSink>) -> Result<Self> {
        Self::open_with_adapters(config, sink, crate::adapters::linked()).await
    }

    /// Open an envagent against an explicit adapter source.
    ///
    /// Production always uses the linked source. The override exists so integration tests
    /// can put their deterministic adapter behind the same envagent wire boundary as a real
    /// installation, without restoring a server-side execution path.
    pub async fn open_with_adapters(
        config: EnvAgentConfig,
        sink: Arc<dyn EventSink>,
        adapters: Arc<dyn AdapterSource>,
    ) -> Result<Self> {
        let environment = EnvironmentId::parse(config.environment_id.clone())
            .map_err(|error| anyhow!("invalid environment id: {error}"))?;
        let _ = sylph_protocol::ProducerId::parse(config.producer_id.clone())
            .map_err(|error| anyhow!("invalid producer id: {error}"))?;
        let spool = Spool::open(config.spool.clone())
            .await
            .context("opening envagent spool")?;
        Ok(Self {
            config: Arc::new(config),
            environment,
            spool,
            sink: Arc::new(RwLock::new(sink)),
            send_gate: Arc::new(Mutex::new(())),
            sessions: Arc::new(RwLock::new(std::collections::HashMap::new())),
            native_sources: Arc::new(RwLock::new(HashSet::new())),
            workdirs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            active_turns: Arc::new(RwLock::new(std::collections::HashMap::new())),
            native_pump_handles: Arc::new(Mutex::new(std::collections::HashMap::new())),
            deferred_events: Arc::new(Mutex::new(std::collections::HashMap::new())),
            prepared: Arc::new(RwLock::new(std::collections::HashMap::new())),
            promoted_drafts: Arc::new(RwLock::new(std::collections::HashMap::new())),
            adapters,
        })
    }

    pub fn spool(&self) -> &Spool {
        &self.spool
    }

    pub fn config(&self) -> &EnvAgentConfig {
        &self.config
    }

    pub fn environment_id(&self) -> &EnvironmentId {
        &self.environment
    }

    pub async fn inventory(&self) -> Vec<sylph_protocol::AgentInventory> {
        crate::health::HealthReporter::with_adapters(self.adapters.clone())
            .inventory()
            .await
    }

    pub async fn set_sink(&self, sink: Arc<dyn EventSink>) {
        *self.sink.write().await = sink;
    }

    /// Probe one agent and answer which id a **new** session should be opened under.
    ///
    /// For the path that has no probe of its own. [`Self::prepare_hosted`] already holds an
    /// [`sylph_protocol::AgentInfo`] by the time it needs this, and calls
    /// `requested_native_id_for_new` directly rather than probing a second time — one
    /// session start must not spawn `<cli> --version` twice.
    async fn probe_requested_native_id_for_new(
        &self,
        agent: &str,
    ) -> Result<sylph_protocol::RequestedNativeId> {
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent '{agent}' is not installed on this environment"))?;
        let info = adapter
            .probe()
            .await
            .with_context(|| format!("agent '{agent}' would not report what it is"))?;
        Ok(requested_native_id_for_new(&info))
    }

    /// Open a prepared link, containing the workdir and naming the session's id.
    ///
    /// **Sets `spec.native_id` itself**, from the probe it performs for the handshake check:
    /// a prepared link is always a new session, so whatever the caller put there is
    /// overwritten by [`requested_native_id_for_new`].
    async fn prepare_hosted(&self, agent: &str, mut spec: RunSpec) -> Result<PreparedHosted> {
        let contained = self
            .config
            .path_policy
            .contain(&spec.workdir)
            .map_err(|error| anyhow!(error))?;
        if !contained.is_dir() {
            return Err(anyhow!("working directory is not a directory"));
        }
        spec.workdir = contained.display().to_string();
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent '{agent}' is not installed on this environment"))?;
        let info = adapter
            .probe()
            .await
            .with_context(|| format!("agent '{agent}' would not report what it is"))?;
        if info.name != agent {
            return Err(anyhow!(
                "agent '{agent}' reported itself as '{}', which is a handshake failure",
                info.name
            ));
        }
        // Decided here rather than by the caller, from the `AgentInfo` this already holds:
        // a prepared link is always a new session, and probing again to learn one field of
        // an answer in hand would spawn `<cli> --version` twice per session start.
        spec.native_id = requested_native_id_for_new(&info);
        let mut session = adapter.run(spec).await?;
        let events = session
            .events()
            .ok_or_else(|| anyhow!("adapter '{agent}' returned no event stream"))?;
        let agent_name = AgentName::parse(agent.to_string())
            .map_err(|error| anyhow!("invalid agent name: {error}"))?;
        let identity = Arc::new(IdentityState::new(
            self.environment.clone(),
            agent_name,
            session.native_id(),
        ));
        Ok(PreparedHosted {
            session: HostedSession {
                identity,
                session: Arc::new(Mutex::new(session)),
            },
            // Keep consuming the adapter stream while the draft is idle, but do
            // not publish it yet.  The unbounded hand-off prevents a vendor
            // process from blocking on a full bounded channel before the first
            // turn promotes the draft.
            events: Arc::new(Mutex::new(Some(buffer_event_stream(events)))),
            workdir: contained,
            agent: agent.to_string(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_command(
        &self,
        draft_id: String,
        agent: String,
        workdir: String,
        flags: BTreeMap<String, String>,
        inject_identity: bool,
        identity_preamble: Option<String>,
        permission_mode: Option<String>,
    ) -> sylph_protocol::CommandOutcome {
        if draft_id.trim().is_empty() {
            return sylph_protocol::CommandOutcome::Rejected {
                code: sylph_protocol::RejectCode::InvalidDraftId.as_str().into(),
                message: "draft id must not be empty".into(),
            };
        }
        if self.prepared.read().await.contains_key(&draft_id) {
            return sylph_protocol::CommandOutcome::Accepted {
                result: Some(serde_json::json!({"draftId": draft_id})),
            };
        }
        // Classify inventory failures before probing. The bridge must preserve the
        // typed distinction between an unknown vendor and a known vendor whose CLI is
        // currently unavailable; collapsing both into PREPARE_FAILED makes a plausible
        // 503 out of a durable `no_agent` facet.
        if !self.adapters.is_known(&agent) {
            return sylph_protocol::CommandOutcome::Rejected {
                code: "AGENT_UNKNOWN".into(),
                message: format!("agent '{agent}' is not known on this environment"),
            };
        }
        if self.adapters.for_agent(&agent).is_none() {
            return sylph_protocol::CommandOutcome::Rejected {
                code: "NO_AGENT".into(),
                message: format!("agent '{agent}' is not installed on this environment"),
            };
        }
        // Quick Start is an environment-level action. The server cannot send its own
        // absolute path to a remote machine, so it sends this explicit sentinel and the
        // envagent resolves the directory from its local home (the first path-policy
        // root). This keeps workspace identity scoped to `(environment, path)` and
        // prevents a remote path from being interpreted on the server's filesystem.
        let workdir = if workdir == sylph_protocol::QUICK_START_SENTINEL {
            let Some(home) = self.config.path_policy.roots().first() else {
                return sylph_protocol::CommandOutcome::Rejected {
                    code: sylph_protocol::RejectCode::WorkdirMissing.as_str().into(),
                    message: "environment has no home directory".into(),
                };
            };
            let quick_start = home.join("scratch");
            if let Err(error) = std::fs::create_dir_all(&quick_start) {
                return sylph_protocol::CommandOutcome::Rejected {
                    code: sylph_protocol::RejectCode::PrepareFailed.as_str().into(),
                    message: format!("cannot create Quick Start directory: {error}"),
                };
            }
            quick_start.display().to_string()
        } else {
            workdir
        };
        let spec = RunSpec {
            workdir,
            // Overwritten by `prepare_hosted` from the agent's own `dictates_native_id`,
            // which it learns from the probe it already performs. `New` here is not a
            // decision — it is the field's only inhabitant that names no id, and the value
            // never reaches an adapter.
            native_id: sylph_protocol::RequestedNativeId::New,
            inject_identity,
            identity_preamble,
            flags,
            permission_mode,
        };
        match self.prepare_hosted(&agent, spec).await {
            Ok(prepared) => {
                let workdir = prepared.workdir.display().to_string();
                let native_id = prepared.session.native_id();
                self.prepared
                    .write()
                    .await
                    .insert(draft_id.clone(), prepared);
                sylph_protocol::CommandOutcome::accepted_with_result(serde_json::json!({
                    "draftId": draft_id,
                    "workdir": workdir,
                    "sessionId": native_id.map(|native| SessionId::new(
                        self.environment.clone(),
                        AgentName::parse(agent).expect("validated agent name"),
                        native,
                    )),
                }))
            }
            Err(error) => {
                let detail = format!("{error:#}");
                let code = match error.downcast_ref::<crate::path::ContainmentError>() {
                    Some(crate::path::ContainmentError::Missing(_)) => {
                        sylph_protocol::RejectCode::WorkdirMissing
                    }
                    Some(crate::path::ContainmentError::NotDirectory(_)) => {
                        sylph_protocol::RejectCode::WorkdirNotADirectory
                    }
                    Some(crate::path::ContainmentError::Outside(_))
                    | Some(crate::path::ContainmentError::SymlinkEscape(_)) => {
                        sylph_protocol::RejectCode::WorkdirOutsideRoots
                    }
                    _ if detail.contains("not a directory") => {
                        sylph_protocol::RejectCode::WorkdirNotADirectory
                    }
                    _ if detail.contains("outside") || detail.contains("escapes") => {
                        sylph_protocol::RejectCode::WorkdirOutsideRoots
                    }
                    _ => sylph_protocol::RejectCode::PrepareFailed,
                };
                sylph_protocol::CommandOutcome::Rejected {
                    code: code.as_str().into(),
                    message: detail,
                }
            }
        }
    }

    async fn clear_active_turn(&self, session_id: &SessionId) {
        self.active_turns.write().await.remove(session_id);
    }

    async fn clear_active_turn_if_matches(
        &self,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
    ) {
        let mut active = self.active_turns.write().await;
        if expected_turn_id.is_none_or(|expected| {
            active
                .get(session_id)
                .is_some_and(|actual| actual == expected)
        }) {
            active.remove(session_id);
        }
    }

    /// Remove every runtime-owned handle for a session and close its adapter.
    /// This is used by both explicit `Close` and compensation after the server
    /// fails while promoting a remote draft. Keeping the map cleanup together
    /// prevents a stale workdir, active-turn attribution, or deferred gate
    /// from surviving the process handle that created it.
    async fn close_hosted_session(&self, session_id: &SessionId) {
        let session = self.sessions.write().await.remove(session_id);
        let _source = self.native_sources.write().await.remove(session_id);
        self.promoted_drafts
            .write()
            .await
            .retain(|_, id| id != session_id);
        self.workdirs.write().await.remove(session_id);
        self.active_turns.write().await.remove(session_id);
        if let Some(gate) = self.deferred_events.lock().await.remove(session_id) {
            gate.notify_waiters();
        }
        if let Some(handle) = self.native_pump_handles.lock().await.remove(session_id) {
            handle.abort();
        }
        if let Some(session) = session {
            let _ = session.close().await;
        }
    }

    async fn close_prepared_failure(&self, prepared: PreparedHosted) {
        let _ = prepared.session.close().await;
        // Dropping the buffered stream closes its producer task; no native
        // session identity was published, so there is no other map to clear.
    }

    /// Start a live adapter session after applying local path policy.
    pub async fn run(&self, agent: &str, spec: RunSpec) -> Result<HostedSession> {
        self.run_with_event_gate(agent, spec, false, None, None)
            .await
    }

    async fn run_with_event_gate(
        &self,
        agent: &str,
        mut spec: RunSpec,
        defer_events: bool,
        initial_input: Option<TurnInput>,
        initial_turn_id: Option<String>,
    ) -> Result<HostedSession> {
        let contained = self
            .config
            .path_policy
            .contain(&spec.workdir)
            .map_err(|error| anyhow!(error))?;
        if !contained.is_dir() {
            return Err(anyhow!("working directory is not a directory"));
        }
        spec.workdir = contained.display().to_string();
        let agent_name = AgentName::parse(agent.to_string())
            .map_err(|error| anyhow!("invalid agent name: {error}"))?;
        let workdir = spec.workdir.clone();
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent '{agent}' is not installed on this environment"))?;
        let mut session = adapter.run(spec).await?;
        let events = session
            .events()
            .ok_or_else(|| anyhow!("adapter '{agent}' returned no event stream"))?;
        let mut assigned_native_id = None;
        if let Some(input) = initial_input {
            if session.native_id().is_some() {
                session.send(input).await?;
            } else {
                assigned_native_id = Some(session.send_initial(input).await?);
            }
        }
        let native_id = session.native_id().or(assigned_native_id);
        let identity = Arc::new(IdentityState::new(
            self.environment.clone(),
            agent_name,
            native_id.clone(),
        ));
        let hosted = HostedSession {
            identity: identity.clone(),
            session: Arc::new(Mutex::new(session)),
        };
        // A vendor that already assigned its id can safely wait behind the
        // server's activation barrier. Vendors that assign an id only after
        // the first turn (Codex) cannot return a durable session from `Run`
        // alone, so leave the gate unused and report that ambiguity explicitly.
        let gate = defer_events.then(|| Arc::new(Notify::new()));
        if let (Some(gate), Some(native_id)) = (gate.as_ref(), hosted.native_id()) {
            let key = SessionId::new(
                self.environment.clone(),
                hosted.identity.agent.clone(),
                native_id,
            );
            self.deferred_events.lock().await.insert(key, gate.clone());
        }
        // The server creates the first turn only after this command reports the
        // vendor's native identity.  Install its arrival attribution before the
        // event pump is released, so the first producer batch cannot outrun the
        // server-side turn row.
        if let (Some(turn_id), Some(session_id)) = (initial_turn_id, hosted.session_id().await) {
            self.active_turns.write().await.insert(session_id, turn_id);
        }
        self.spawn_event_pump(events, identity, gate);
        if let Some(session_id) = hosted.session_id().await {
            self.workdirs
                .write()
                .await
                .insert(session_id.clone(), std::path::PathBuf::from(workdir));
            self.sessions
                .write()
                .await
                .insert(session_id, hosted.clone());
        }
        Ok(hosted)
    }

    /// Release event pumps held behind the command-result barrier. Called by
    /// the connection loop only after the result frame has been written.
    pub async fn release_deferred_events(&self, session_id: &SessionId) -> bool {
        if let Some(gate) = self.deferred_events.lock().await.remove(session_id) {
            gate.notify_one();
            true
        } else {
            false
        }
    }

    pub async fn adopt(&self, agent: &str, native_id: NativeId) -> Result<HostedSource> {
        self.start_native_source(agent, native_id, false, false, None)
            .await
    }

    pub async fn watch(&self, agent: &str, native_id: NativeId) -> Result<HostedSource> {
        self.start_native_source(agent, native_id, true, false, None)
            .await
    }

    /// One bare parse of a native session, answered inline.
    ///
    /// Hosts nothing: no source registration, no pump, no workdir entry. The server owns
    /// deduplication against its own journal, so this reads the store and hands the whole
    /// pass back (`docs/environments.md` § Re-reading a session Sylph already holds).
    ///
    /// # Path policy still applies, and the reason is not obvious
    ///
    /// Nothing is spawned here, so there is no `current_dir` to bound — yet the recorded
    /// workdir is still contained before the parse. An adapter reading a native session
    /// also reads that session's *worktree*, for sidecar bodies the transcript points at,
    /// and a store holds every session any agent ever ran on this machine. Skipping the
    /// check because this command starts no process would make it a way to read files
    /// outside the configured roots by naming a session that ran there.
    pub async fn reread(
        &self,
        agent: &str,
        native_id: NativeId,
    ) -> Result<sylph_protocol::RereadPass> {
        // Through the injected `AdapterSource`, exactly as `start_native_source` resolves
        // it. The free `sylph_adapters::for_agent` answers from the hardcoded vendor list,
        // which bypasses the seam this field exists to provide — so a re-read would refuse
        // any agent that is not one of the three built-in names, and no test could drive
        // it at all.
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent '{agent}' is not installed on this environment"))?;
        let summary = adapter
            .enumerate(&sylph_protocol::StoreFilter {
                limit: None,
                ..Default::default()
            })
            .await?
            .into_iter()
            .find(|summary| summary.native_id == native_id)
            .ok_or_else(|| anyhow!("native session '{}' was not found", native_id))?;
        let recorded_workdir = summary
            .workdir
            .as_deref()
            .ok_or_else(|| anyhow!("native session '{}' has no recorded workdir", native_id))?;
        self.config
            .path_policy
            .contain(recorded_workdir)
            .map_err(|error| anyhow!(error))?;

        let mut stream = adapter.adopt(&native_id).await?;
        let mut pass = sylph_protocol::RereadPass::default();
        while let Some(item) = stream.next().await {
            match item {
                Ok(sylph_protocol::SessionMessage::Event(event)) => pass.events.push(event),
                Ok(sylph_protocol::SessionMessage::NativeRecord(record)) => {
                    // Kept, never dropped. A vendor's whole record is what makes a schema
                    // break cheap, and `AGENTS.md` § Compatibility forbids ingesting
                    // lossily — the server dedups these on `(session, source, version)`.
                    pass.records.push(record)
                }
                // A request read from a store was answered months ago, and a capability
                // surface describes a link this session does not have. Both are counted
                // rather than silently dropped, so a parser that starts emitting one is
                // visible here instead of invisible. The server's own drain draws the same
                // two distinctions for the same reasons.
                Ok(_) => pass.skipped += 1,
                Err(error) => {
                    pass.read_errors += 1;
                    if pass.read_errors == 1 {
                        warn!("could not read part of native session {native_id}: {error:#}");
                    } else {
                        debug!(
                            "further read error on {native_id} (#{}): {error:#}",
                            pass.read_errors
                        );
                    }
                }
            }
        }
        Ok(pass)
    }

    async fn reread_command(&self, agent: &str, native_id: NativeId) -> Result<serde_json::Value> {
        let session_id = SessionId::new(
            EnvironmentId::parse(self.config.environment_id.clone())?,
            AgentName::parse(agent.to_string())?,
            native_id.clone(),
        );
        let mut pass = self.reread(agent, native_id).await?;
        // Sidecars are transferred through the durable producer path before the
        // command result exposes their native records. The server can therefore
        // acknowledge the reread only after CAS retention has completed.
        let mut retained = Vec::with_capacity(pass.records.len());
        for record in pass.records {
            retained.push(self.retain_native_sidecars(&session_id, record).await?);
        }
        pass.records = retained;
        while self.flush_pending(64).await? > 0 {}
        Ok(serde_json::to_value(&pass)?)
    }

    async fn start_native_source(
        &self,
        agent: &str,
        native_id: NativeId,
        follow: bool,
        defer_events: bool,
        arrival_turn_id: Option<String>,
    ) -> Result<HostedSource> {
        let agent_name = AgentName::parse(agent.to_string())
            .map_err(|error| anyhow!("invalid agent name: {error}"))?;
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent '{agent}' is not installed on this environment"))?;
        // Native sources are still capable of running adapter code that reads
        // a vendor worktree.  Resolve the recorded directory on this machine
        // before starting the watcher; accepting an id first and checking later
        // would let `watch`/`adopt` bypass the same path policy `run` enforces.
        let summary = adapter
            .enumerate(&sylph_protocol::StoreFilter {
                limit: None,
                ..Default::default()
            })
            .await?
            .into_iter()
            .find(|summary| summary.native_id == native_id)
            .ok_or_else(|| anyhow!("native session '{}' was not found", native_id))?;
        let recorded_workdir = summary
            .workdir
            .as_deref()
            .ok_or_else(|| anyhow!("native session '{}' has no recorded workdir", native_id))?;
        let contained_workdir = self
            .config
            .path_policy
            .contain(recorded_workdir)
            .map_err(|error| anyhow!(error))?;
        let events = if follow {
            adapter.watch(&native_id).await?
        } else {
            adapter.adopt(&native_id).await?
        };
        let session_id = SessionId::new(self.environment.clone(), agent_name.clone(), native_id);
        // Native sources always have an identity before their first record.
        let identity = Arc::new(IdentityState::new(
            self.environment.clone(),
            agent_name,
            Some(session_id.native_id.clone()),
        ));
        let gate = defer_events.then(|| Arc::new(Notify::new()));
        if let Some(gate) = gate.as_ref() {
            self.deferred_events
                .lock()
                .await
                .insert(session_id.clone(), gate.clone());
        }
        if let Some(turn_id) = arrival_turn_id.clone() {
            self.active_turns
                .write()
                .await
                .insert(session_id.clone(), turn_id);
        }
        self.native_sources.write().await.insert(session_id.clone());
        let pump = self.spawn_event_pump(events, identity, gate);
        self.native_pump_handles
            .lock()
            .await
            .insert(session_id.clone(), pump);
        self.workdirs
            .write()
            .await
            .insert(session_id.clone(), contained_workdir.clone());
        Ok(HostedSource {
            session_id,
            workdir: contained_workdir.display().to_string(),
            fidelity: summary.fidelity,
            title: summary.title,
            started_at_ms: summary.started_at_ms,
            ended_at_ms: summary.ended_at_ms,
            parent_native_id: summary.parent_native_id,
            parent_relation: summary.parent_relation,
        })
    }

    /// Execute one server command on this environment. The returned outcome
    /// is safe to persist and may be Unknown only when the caller loses the
    /// connection after dispatching a non-idempotent operation.
    pub async fn handle_command(
        &self,
        command: sylph_protocol::EnvironmentCommand,
    ) -> sylph_protocol::CommandOutcome {
        use sylph_protocol::EnvironmentCommand;
        match command {
            EnvironmentCommand::Prepare {
                draft_id,
                agent,
                workdir,
                flags,
                inject_identity,
                identity_preamble,
                permission_mode,
            } => {
                self.prepare_command(
                    draft_id,
                    agent,
                    workdir,
                    flags,
                    inject_identity,
                    identity_preamble,
                    permission_mode,
                )
                .await
            }
            EnvironmentCommand::Run {
                session_id,
                draft_id,
                agent,
                workdir,
                flags,
                inject_identity,
                identity_preamble,
                permission_mode,
                initial_input,
                initial_turn_id,
            } => {
                if !self.adapters.is_known(&agent) {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: "AGENT_UNKNOWN".into(),
                        message: format!("agent '{agent}' is not known on this environment"),
                    };
                }
                if self.adapters.for_agent(&agent).is_none() {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: "NO_AGENT".into(),
                        message: format!("agent '{agent}' is not installed on this environment"),
                    };
                }
                // A prepared draft already owns the adapter process. Promote it
                // in place so retries cannot spawn a second vendor session.
                if let Some(draft_id) = draft_id {
                    if let Some(prepared) = self.prepared.write().await.remove(&draft_id) {
                        let initial_input = match initial_input {
                            Some(input) => match self
                                .local_turn_input(
                                    input,
                                    &draft_id,
                                    initial_turn_id.as_deref().unwrap_or("initial"),
                                )
                                .await
                            {
                                Ok(v) => Some(v),
                                Err(error) => {
                                    return sylph_protocol::CommandOutcome::Rejected {
                                        code: sylph_protocol::RejectCode::AttachmentInvalid
                                            .as_str()
                                            .into(),
                                        message: format!("{error:#}"),
                                    }
                                }
                            },
                            None => None,
                        };
                        let native_id = if let Some(input) = initial_input {
                            if prepared.session.native_id().is_some() {
                                if let Err(error) = prepared.session.send(input).await {
                                    self.close_prepared_failure(prepared).await;
                                    return sylph_protocol::CommandOutcome::Rejected {
                                        code: sylph_protocol::RejectCode::RunFailed.as_str().into(),
                                        message: format!("{error:#}"),
                                    };
                                }
                                prepared.session.native_id_async().await
                            } else {
                                match prepared.session.send_initial(input).await {
                                    Ok(native) => Some(native),
                                    Err(error) => {
                                        self.close_prepared_failure(prepared).await;
                                        return sylph_protocol::CommandOutcome::Rejected {
                                            code: sylph_protocol::RejectCode::RunFailed
                                                .as_str()
                                                .into(),
                                            message: format!("{error:#}"),
                                        };
                                    }
                                }
                            }
                        } else {
                            prepared.session.native_id_async().await
                        };
                        let Some(native_id) = native_id else {
                            self.close_prepared_failure(prepared).await;
                            return sylph_protocol::CommandOutcome::Unknown {
                                message: Some("prepared agent has not assigned a native id".into()),
                            };
                        };
                        let agent_name = match AgentName::parse(agent.clone()) {
                            Ok(agent_name) => agent_name,
                            Err(error) => {
                                self.close_prepared_failure(prepared).await;
                                return sylph_protocol::CommandOutcome::Rejected {
                                    code: "INVALID_AGENT".into(),
                                    message: format!("{error}"),
                                };
                            }
                        };
                        let session_id =
                            SessionId::new(self.environment.clone(), agent_name, native_id);
                        self.workdirs
                            .write()
                            .await
                            .insert(session_id.clone(), prepared.workdir.clone());
                        self.sessions
                            .write()
                            .await
                            .insert(session_id.clone(), prepared.session.clone());
                        self.promoted_drafts
                            .write()
                            .await
                            .insert(draft_id.clone(), session_id.clone());
                        if let Some(turn_id) = initial_turn_id {
                            self.active_turns
                                .write()
                                .await
                                .insert(session_id.clone(), turn_id);
                        }
                        // The prepared stream was not consumed while the draft was
                        // idle; attach it only after promotion and keep the same
                        // identity and turn attribution.
                        if let Some(events) = prepared.events.lock().await.take() {
                            let gate = Arc::new(Notify::new());
                            self.deferred_events
                                .lock()
                                .await
                                .insert(session_id.clone(), gate.clone());
                            self.spawn_event_pump(
                                events,
                                prepared.session.identity.clone(),
                                Some(gate),
                            );
                        }
                        return sylph_protocol::CommandOutcome::accepted_with_result(
                            serde_json::json!({
                                "sessionId": session_id,
                                "workdir": prepared.workdir.display().to_string(),
                                "fidelity": "stream"
                            }),
                        );
                    }
                }

                // Resuming names the id the vendor already has. With no session id this is
                // a *new* session reached through `Run` rather than through `Prepare`, so
                // it takes the same decision `Prepare` takes — the `unwrap_or(New)` this
                // replaces held the identical hole, and a dictating agent arriving here
                // was refused exactly as it was there.
                let native_id = match session_id.as_ref() {
                    Some(id) => sylph_protocol::RequestedNativeId::Resume(id.native_id.clone()),
                    None => match self.probe_requested_native_id_for_new(&agent).await {
                        Ok(native_id) => native_id,
                        Err(error) => {
                            return sylph_protocol::CommandOutcome::Rejected {
                                code: sylph_protocol::RejectCode::RunFailed.as_str().into(),
                                message: format!("{error:#}"),
                            }
                        }
                    },
                };
                let spec = RunSpec {
                    workdir,
                    native_id,
                    inject_identity,
                    identity_preamble,
                    flags,
                    permission_mode,
                };
                let initial_input = match initial_input {
                    Some(input) => match self
                        .local_turn_input(
                            input,
                            session_id
                                .as_ref()
                                .map(|s| s.to_string())
                                .as_deref()
                                .unwrap_or(&agent),
                            initial_turn_id.as_deref().unwrap_or("initial"),
                        )
                        .await
                    {
                        Ok(v) => Some(v),
                        Err(error) => {
                            return sylph_protocol::CommandOutcome::Rejected {
                                code: sylph_protocol::RejectCode::AttachmentInvalid
                                    .as_str()
                                    .into(),
                                message: format!("{error:#}"),
                            }
                        }
                    },
                    None => None,
                };
                let defer_events = initial_input.is_some();
                match self
                    .run_with_event_gate(&agent, spec, defer_events, initial_input, initial_turn_id)
                    .await
                {
                    Ok(session) => match session.session_id().await {
                        Some(id) => sylph_protocol::CommandOutcome::accepted_with_result(
                            serde_json::json!({
                                "sessionId": id,
                                "workdir": session
                                    .identity
                                    .session_id()
                                    .await
                                    .and_then(|_| self.workdirs.try_read().ok())
                                    .and_then(|dirs| dirs.get(&id).cloned())
                                    .map(|path| path.display().to_string()),
                                "fidelity": "stream"
                            }),
                        ),
                        None => {
                            let _ = session.close().await;
                            sylph_protocol::CommandOutcome::Unknown {
                                message: Some(
                                    "agent accepted the run but has not assigned an id".into(),
                                ),
                            }
                        }
                    },
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: sylph_protocol::RejectCode::RunFailed.as_str().into(),
                        message: format!("{error:#}"),
                    },
                }
            }
            EnvironmentCommand::Send {
                session_id,
                turn_id,
                input,
            } => {
                let session = self.sessions.read().await.get(&session_id).cloned();
                let Some(session) = session else {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: "SESSION_NOT_HOSTED".into(),
                        message: "no live session on this environment".into(),
                    };
                };
                let input = match self
                    .local_turn_input(input, &session_id.to_string(), &turn_id)
                    .await
                {
                    Ok(v) => v,
                    Err(error) => {
                        return sylph_protocol::CommandOutcome::Rejected {
                            code: sylph_protocol::RejectCode::AttachmentInvalid
                                .as_str()
                                .into(),
                            message: format!("{error:#}"),
                        }
                    }
                };
                self.active_turns
                    .write()
                    .await
                    .insert(session_id.clone(), turn_id);
                match session.send(input).await {
                    Ok(()) => sylph_protocol::CommandOutcome::accepted(),
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: sylph_protocol::RejectCode::SendFailed.as_str().into(),
                        message: format!("{error:#}"),
                    },
                }
            }
            EnvironmentCommand::Interrupt {
                session_id,
                expected_turn_id,
            } => {
                if let Some(expected) = expected_turn_id.as_deref() {
                    let current = self.active_turns.read().await.get(&session_id).cloned();
                    if current.as_deref() != Some(expected) {
                        return sylph_protocol::CommandOutcome::Rejected {
                            code: "TURN_MOVED".into(),
                            message: "the expected turn is no longer active".into(),
                        };
                    }
                }
                self.control_session(
                    &session_id,
                    |session| async move { session.interrupt().await },
                )
                .await
            }
            EnvironmentCommand::Steer {
                session_id,
                text,
                expected_turn_id,
            } => {
                self.control_session(&session_id, |session| async move {
                    session.steer(&text, expected_turn_id.as_deref()).await
                })
                .await
            }
            EnvironmentCommand::Queue { session_id, text } => {
                self.control_session(
                    &session_id,
                    |session| async move { session.queue(&text).await },
                )
                .await
            }
            EnvironmentCommand::SetConfig {
                session_id,
                id,
                value,
            } => match self.sessions.read().await.get(&session_id).cloned() {
                Some(session) => {
                    let persisted = session
                        .launch_flag_for_config(&id, &value)
                        .await
                        .map(|(key, value)| serde_json::json!({"key": key, "value": value}));
                    match session.set_config(&id, value).await {
                        Ok(_options) => match self
                            .capability_result(&session_id.agent.to_string(), &session)
                            .await
                        {
                            Ok(mut result) => {
                                result.persisted_launch_flag =
                                    persisted.and_then(|flag| serde_json::from_value(flag).ok());
                                sylph_protocol::CommandOutcome::accepted_with_result(
                                    serde_json::to_value(result).unwrap_or_default(),
                                )
                            }
                            Err(error) => sylph_protocol::CommandOutcome::Rejected {
                                code: sylph_protocol::RejectCode::CapabilitiesFailed
                                    .as_str()
                                    .into(),
                                message: format!("{error:#}"),
                            },
                        },
                        Err(error) => {
                            if let Some(refusal) =
                                error.downcast_ref::<sylph_protocol::AdapterError>()
                            {
                                sylph_protocol::CommandOutcome::Rejected {
                                    code: match refusal.code {
                                        sylph_protocol::RefusalCode::UnsupportedField => {
                                            "UNSUPPORTED_FIELD"
                                        }
                                        sylph_protocol::RefusalCode::InvalidValue => {
                                            "INVALID_VALUE"
                                        }
                                    }
                                    .into(),
                                    message: refusal.message.clone(),
                                }
                            } else {
                                sylph_protocol::CommandOutcome::Rejected {
                                    code: sylph_protocol::RejectCode::SetConfigFailed
                                        .as_str()
                                        .into(),
                                    message: format!("{error:#}"),
                                }
                            }
                        }
                    }
                }
                None => sylph_protocol::CommandOutcome::Rejected {
                    code: "SESSION_NOT_HOSTED".into(),
                    message: "no live session on this environment".into(),
                },
            },
            EnvironmentCommand::Watch {
                agent,
                native_id,
                turn_id,
                session_id: _,
            } => match self
                .start_native_source(&agent, native_id, true, true, turn_id)
                .await
            {
                Ok(source) => {
                    sylph_protocol::CommandOutcome::accepted_with_result(source.to_command_result())
                }
                Err(error) => sylph_protocol::CommandOutcome::Rejected {
                    code: sylph_protocol::RejectCode::WatchFailed.as_str().into(),
                    message: format!("{error:#}"),
                },
            },
            EnvironmentCommand::Adopt {
                agent,
                native_id,
                turn_id,
                session_id: _,
            } => match self
                .start_native_source(&agent, native_id, false, true, turn_id)
                .await
            {
                Ok(source) => {
                    sylph_protocol::CommandOutcome::accepted_with_result(source.to_command_result())
                }
                Err(error) => sylph_protocol::CommandOutcome::Rejected {
                    code: if self.adapters.is_known(&agent) {
                        "NATIVE_SESSION_NOT_FOUND"
                    } else {
                        "AGENT_UNKNOWN"
                    }
                    .into(),
                    message: format!("{error:#}"),
                },
            },
            // Parse the store again and answer with what it holds, inline.
            //
            // **No native source is registered and no pump is spawned**, which is what
            // separates this from `Adopt`. A re-read targets a session the server already
            // holds, so a source registration would either collide with a live watcher's
            // entry or replace it — and a pump would spool these events a second time,
            // through a path that has no content dedup, duplicating exactly the transcript
            // this operation exists to extend.
            //
            // The result carries the events rather than spooling them because suppression
            // is the server's: only the server knows what its journal already holds. Bulk
            // in a command result is the established shape here — `Enumerate` answers with
            // every native summary a machine holds the same way.
            EnvironmentCommand::Reread {
                agent,
                native_id,
                session_id: _,
            } => match self.reread_command(&agent, native_id).await {
                Ok(result) => sylph_protocol::CommandOutcome::accepted_with_result(result),
                Err(error) => sylph_protocol::CommandOutcome::Rejected {
                    code: "REREAD_FAILED".into(),
                    message: format!("{error:#}"),
                },
            },
            EnvironmentCommand::Reproject { agent, records, .. } => {
                let Some(adapter) = self.adapters.for_agent(&agent) else {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: "AGENT_UNAVAILABLE".into(),
                        message: format!("agent '{agent}' is not installed on this environment"),
                    };
                };
                match adapter.reproject(records).await {
                    Ok(mut stream) => {
                        let mut pass = sylph_protocol::RereadPass::default();
                        while let Some(item) = stream.next().await {
                            match item {
                                Ok(sylph_protocol::SessionMessage::Event(event)) => pass.events.push(event),
                                Ok(sylph_protocol::SessionMessage::NativeRecord(record)) => pass.records.push(record),
                                Ok(_) => pass.skipped += 1,
                                Err(error) => pass.read_errors += 1,
                            }
                        }
                        sylph_protocol::CommandOutcome::accepted_with_result(serde_json::to_value(pass).unwrap_or_default())
                    }
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: "REPROJECT_FAILED".into(),
                        message: format!("{error:#}"),
                    },
                }
            }
            EnvironmentCommand::Health => sylph_protocol::CommandOutcome::accepted_with_result(
                serde_json::to_value(
                    crate::health::HealthReporter::new()
                        .snapshot(self.environment.to_string())
                        .await,
                )
                .unwrap_or_default(),
            ),
            EnvironmentCommand::BrowseRoots => {
                let home = self
                    .config
                    .path_policy
                    .roots()
                    .first()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                let roots = self
                    .config
                    .path_policy
                    .roots()
                    .iter()
                    .map(|path| {
                        let display = path.display().to_string();
                        let name = if display == home {
                            "Home".to_string()
                        } else {
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or(&display)
                                .to_string()
                        };
                        serde_json::json!({"name": name, "path": display})
                    })
                    .collect::<Vec<_>>();
                sylph_protocol::CommandOutcome::accepted_with_result(
                    serde_json::json!({"roots": roots, "home": home}),
                )
            }
            EnvironmentCommand::BrowseTree { path } => {
                let base = path.unwrap_or_default();
                let base = if base.trim().is_empty() {
                    self.config
                        .path_policy
                        .roots()
                        .first()
                        .cloned()
                        .unwrap_or_default()
                        .display()
                        .to_string()
                } else {
                    base
                };
                match self.config.path_policy.contain(&base) {
                    Ok(dir) => match std::fs::read_dir(&dir) {
                        Ok(entries) => {
                            const SKIPPED: &[&str] = &[
                                ".git",
                                "node_modules",
                                "target",
                                ".next",
                                "__pycache__",
                                ".venv",
                                "venv",
                                ".mypy_cache",
                                ".pytest_cache",
                                "dist",
                                "build",
                            ];
                            let mut output = Vec::new();
                            let mut truncated = false;
                            for entry in entries.flatten() {
                                if output.len() >= 2000 {
                                    truncated = true;
                                    break;
                                }
                                let name = entry.file_name().to_string_lossy().to_string();
                                if SKIPPED.contains(&name.as_str()) {
                                    continue;
                                }
                                let file_type = match entry.file_type() {
                                    Ok(value) => value,
                                    Err(_) => continue,
                                };
                                let is_symlink = file_type.is_symlink();
                                let resolved = if is_symlink {
                                    match self.config.path_policy.contain(entry.path()) {
                                        Ok(value) => value,
                                        Err(_) => continue,
                                    }
                                } else {
                                    entry.path()
                                };
                                let metadata = match std::fs::metadata(&resolved) {
                                    Ok(value) => value,
                                    Err(_) => continue,
                                };
                                if !metadata.is_dir() {
                                    continue;
                                }
                                let modified_at_ms = metadata
                                    .modified()
                                    .ok()
                                    .and_then(|value| {
                                        value.duration_since(std::time::UNIX_EPOCH).ok()
                                    })
                                    .map(|value| value.as_millis() as i64)
                                    .unwrap_or(0);
                                output.push(serde_json::json!({"path": entry.path().display().to_string(), "name": name, "modifiedAtMs": modified_at_ms, "isSymlink": is_symlink}));
                            }
                            output.sort_by(|a, b| {
                                a["name"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_lowercase()
                                    .cmp(&b["name"].as_str().unwrap_or_default().to_lowercase())
                            });
                            let parent = dir
                                .parent()
                                .filter(|parent| self.config.path_policy.contains(parent))
                                .map(|parent| parent.display().to_string());
                            sylph_protocol::CommandOutcome::accepted_with_result(
                                serde_json::json!({"path": dir.display().to_string(), "parent": parent, "entries": output, "truncated": truncated}),
                            )
                        }
                        Err(error) => sylph_protocol::CommandOutcome::Rejected {
                            code: "INTERNAL".into(),
                            message: error.to_string(),
                        },
                    },
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: "POLICY_VIOLATION".into(),
                        message: error.to_string(),
                    },
                }
            }
            EnvironmentCommand::Capabilities { session_id } => {
                if self.native_sources.read().await.contains(&session_id) {
                    // The vendor answers this, not a literal here. Hardcoding it meant
                    // one object stood for every native source, which is only true while
                    // no vendor is drivable by a second party — and `codex` is, over its
                    // app-server (`sylph_adapters::native_link_facets`).
                    let link = sylph_adapters::native_link_facets(&session_id.agent.to_string());
                    return sylph_protocol::CommandOutcome::accepted_with_result(
                        serde_json::json!({
                            "capabilities": {"commands": [], "config": []},
                            "supports": [],
                            "link": link
                        }),
                    );
                }
                match self.sessions.read().await.get(&session_id).cloned() {
                    Some(session) => match self
                        .capability_result(&session_id.agent.to_string(), &session)
                        .await
                    {
                        Ok(value) => sylph_protocol::CommandOutcome::accepted_with_result(
                            serde_json::to_value(value).unwrap_or_default(),
                        ),
                        Err(error) => sylph_protocol::CommandOutcome::Rejected {
                            code: sylph_protocol::RejectCode::CapabilitiesFailed
                                .as_str()
                                .into(),
                            message: format!("{error:#}"),
                        },
                    },
                    None => sylph_protocol::CommandOutcome::Rejected {
                        code: "SESSION_NOT_HOSTED".into(),
                        message: "no live session on this environment".into(),
                    },
                }
            }
            EnvironmentCommand::Enumerate { agent, workdir } => {
                let Some(adapter) = self.adapters.for_agent(&agent) else {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: if self.adapters.is_known(&agent) {
                            "NATIVE_SESSION_NOT_FOUND"
                        } else {
                            "AGENT_UNKNOWN"
                        }
                        .into(),
                        message: format!("agent '{agent}' is not installed on this environment"),
                    };
                };
                let filter = sylph_protocol::StoreFilter {
                    workdir,
                    since_ms: None,
                    limit: None,
                };
                match adapter.enumerate(&filter).await {
                    Ok(rows) => sylph_protocol::CommandOutcome::accepted_with_result(
                        serde_json::to_value(rows).unwrap_or_default(),
                    ),
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: sylph_protocol::RejectCode::EnumerateFailed.as_str().into(),
                        message: format!("{error:#}"),
                    },
                }
            }
            EnvironmentCommand::Activate { session_id } => {
                if self.release_deferred_events(&session_id).await
                    || self.sessions.read().await.contains_key(&session_id)
                {
                    // The draft has crossed the server's durable activation
                    // barrier; a later DraftClose must not tear down this
                    // now-canonical session.
                    let mut promoted = self.promoted_drafts.write().await;
                    let draft = promoted
                        .iter()
                        .find_map(|(draft, id)| (id == &session_id).then(|| draft.clone()));
                    if let Some(draft) = draft {
                        promoted.remove(&draft);
                    }
                    sylph_protocol::CommandOutcome::accepted()
                } else {
                    sylph_protocol::CommandOutcome::Rejected {
                        code: "SESSION_NOT_HOSTED".into(),
                        message: "no live session on this environment".into(),
                    }
                }
            }
            EnvironmentCommand::Close { session_id } => {
                if let Some(session_id) = session_id {
                    self.close_hosted_session(&session_id).await;
                }
                sylph_protocol::CommandOutcome::accepted()
            }
            EnvironmentCommand::Files {
                session_id,
                operation,
            } => self.file_command(&session_id, operation).await,
            EnvironmentCommand::DraftCapabilities { draft_id } => {
                match self.prepared.read().await.get(&draft_id).cloned() {
                    Some(prepared) => match self
                        .capability_result(&prepared.agent, &prepared.session)
                        .await
                    {
                        Ok(value) => sylph_protocol::CommandOutcome::accepted_with_result(
                            serde_json::to_value(value).unwrap_or_default(),
                        ),
                        Err(error) => sylph_protocol::CommandOutcome::Rejected {
                            code: sylph_protocol::RejectCode::CapabilitiesFailed
                                .as_str()
                                .into(),
                            message: format!("{error:#}"),
                        },
                    },
                    None => sylph_protocol::CommandOutcome::Rejected {
                        code: "DRAFT_NOT_FOUND".into(),
                        message: "no prepared draft on this environment".into(),
                    },
                }
            }
            EnvironmentCommand::DraftSetConfig {
                draft_id,
                id,
                value,
            } => match self.prepared.read().await.get(&draft_id).cloned() {
                Some(prepared) => match prepared.session.set_config(&id, value).await {
                    Ok(_options) => match self
                        .capability_result(&prepared.agent, &prepared.session)
                        .await
                    {
                        Ok(value) => sylph_protocol::CommandOutcome::accepted_with_result(
                            serde_json::to_value(value).unwrap_or_default(),
                        ),
                        Err(error) => sylph_protocol::CommandOutcome::Rejected {
                            code: sylph_protocol::RejectCode::CapabilitiesFailed
                                .as_str()
                                .into(),
                            message: format!("{error:#}"),
                        },
                    },
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: sylph_protocol::RejectCode::SetConfigFailed.as_str().into(),
                        message: format!("{error:#}"),
                    },
                },
                None => sylph_protocol::CommandOutcome::Rejected {
                    code: "DRAFT_NOT_FOUND".into(),
                    message: "no prepared draft on this environment".into(),
                },
            },
            EnvironmentCommand::DraftFiles {
                draft_id,
                operation,
            } => match self.prepared.read().await.get(&draft_id).cloned() {
                Some(prepared) => {
                    self.file_command_at_root(&prepared.workdir, operation)
                        .await
                }
                None => sylph_protocol::CommandOutcome::Rejected {
                    code: "DRAFT_NOT_FOUND".into(),
                    message: "no prepared draft on this environment".into(),
                },
            },
            EnvironmentCommand::DraftClose { draft_id } => {
                if let Some(prepared) = self.prepared.write().await.remove(&draft_id) {
                    let _ = prepared.session.close().await;
                }
                if let Some(session_id) = self.promoted_drafts.write().await.remove(&draft_id) {
                    self.close_hosted_session(&session_id).await;
                }
                sylph_protocol::CommandOutcome::accepted()
            }
            EnvironmentCommand::Resolve {
                session_id,
                resolution,
            } => {
                let Some(session) = self.sessions.read().await.get(&session_id).cloned() else {
                    return sylph_protocol::CommandOutcome::Rejected {
                        code: "SESSION_NOT_HOSTED".into(),
                        message: "no live session on this environment".into(),
                    };
                };
                match serde_json::from_value::<sylph_protocol::Resolution>(resolution) {
                    Ok(resolution) => match session.resolve(resolution).await {
                        Ok(()) => sylph_protocol::CommandOutcome::accepted(),
                        Err(error) => sylph_protocol::CommandOutcome::Rejected {
                            code: "RESOLVE_FAILED".into(),
                            message: format!("{error:#}"),
                        },
                    },
                    Err(error) => sylph_protocol::CommandOutcome::Rejected {
                        code: "INVALID_RESOLUTION".into(),
                        message: format!("{error}"),
                    },
                }
            }
        }
    }

    async fn file_command(
        &self,
        session_id: &SessionId,
        operation: sylph_protocol::FileOperation,
    ) -> sylph_protocol::CommandOutcome {
        let Some(root) = self.workdirs.read().await.get(session_id).cloned() else {
            return sylph_protocol::CommandOutcome::Rejected {
                code: "WORKDIR_UNKNOWN".into(),
                message: "the envagent has no recorded workdir for this session".into(),
            };
        };
        self.file_command_at_root(&root, operation).await
    }

    async fn file_command_at_root(
        &self,
        root: &std::path::Path,
        operation: sylph_protocol::FileOperation,
    ) -> sylph_protocol::CommandOutcome {
        let policy = &self.config.path_policy;
        let result: Result<serde_json::Value> = (|| {
            use sylph_protocol::FileOperation;
            match operation {
                FileOperation::List { relative } => {
                    let path = contain_session_path(policy, root, &relative)?;
                    let mut entries = Vec::new();
                    for entry in std::fs::read_dir(path)? {
                        let entry = entry?;
                        let file_type = entry.file_type()?;
                        let resolved = if file_type.is_symlink() {
                            match entry.path().canonicalize() {
                                Ok(resolved) if resolved.starts_with(root) => resolved,
                                _ => continue,
                            }
                        } else {
                            entry.path()
                        };
                        let metadata = std::fs::metadata(resolved)?;
                        entries.push(serde_json::json!({
                            "name": entry.file_name().to_string_lossy(),
                            "directory": metadata.is_dir(),
                            "size": metadata.len(),
                            "symlink": file_type.is_symlink(),
                            "modifiedAtMs": metadata
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|duration| duration.as_millis() as i64),
                        }));
                    }
                    Ok(serde_json::json!(entries))
                }
                FileOperation::Read { relative, limit } => {
                    let path = contain_session_path(policy, root, &relative)?;
                    if path.is_dir() {
                        anyhow::bail!("path is a directory");
                    }
                    let metadata = std::fs::metadata(&path)?;
                    let size = metadata.len();
                    let maximum = sylph_protocol::MAX_FILE_READ_BYTES;
                    let requested = limit.unwrap_or(size);
                    if requested > maximum {
                        return Err(anyhow!("remote read exceeds the {maximum}-byte limit"));
                    }
                    let returned = requested.min(size);
                    let mut file = std::fs::File::open(path)?;
                    let mut bytes = Vec::with_capacity(returned as usize);
                    file.by_ref().take(returned).read_to_end(&mut bytes)?;
                    let modified_at_ms = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_millis() as i64);
                    Ok(serde_json::json!({
                        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                        "size": size,
                        "truncated": returned < size,
                        "modifiedAtMs": modified_at_ms
                    }))
                }
                FileOperation::Write {
                    relative,
                    data,
                    overwrite,
                } => {
                    let bytes =
                        base64::engine::general_purpose::STANDARD.decode(data.as_bytes())?;
                    let path = contain_session_write_target(policy, root, &relative)?;
                    if path.exists() && !overwrite {
                        return Err(anyhow!("file already exists"));
                    }
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, &bytes)?;
                    Ok(serde_json::json!({"written": bytes.len()}))
                }
                FileOperation::Stat { relative } => {
                    let path = contain_session_path(policy, root, &relative)?;
                    let metadata = std::fs::metadata(path)?;
                    Ok(serde_json::json!({
                        "directory": metadata.is_dir(),
                        "size": metadata.len(),
                    }))
                }
                FileOperation::Mkdir { relative } => {
                    let path = contain_session_write_target(policy, root, &relative)?;
                    std::fs::create_dir(&path)?;
                    Ok(serde_json::json!({"created": true}))
                }
                FileOperation::Remove {
                    relative,
                    recursive,
                } => {
                    // Leave the final component unresolved so a symlink is
                    // unlinked rather than followed to its target. The parent
                    // is canonicalized and contained by the write-target
                    // helper, so traversal and escaping parent symlinks still
                    // fail before anything is removed.
                    let path = contain_session_remove_target(policy, root, &relative)?;
                    let metadata = std::fs::symlink_metadata(&path)?;
                    if metadata.file_type().is_symlink() {
                        std::fs::remove_file(path)?;
                    } else if metadata.is_dir() {
                        if recursive {
                            std::fs::remove_dir_all(path)?;
                        } else {
                            std::fs::remove_dir(path)?;
                        }
                    } else {
                        std::fs::remove_file(path)?;
                    }
                    Ok(serde_json::json!({"removed": true}))
                }
            }
        })();
        match result {
            Ok(value) => sylph_protocol::CommandOutcome::accepted_with_result(value),
            Err(error) => sylph_protocol::CommandOutcome::Rejected {
                code: sylph_protocol::RejectCode::FilesFailed.as_str().into(),
                message: format!("{error:#}"),
            },
        }
    }

    async fn capability_result(
        &self,
        agent: &str,
        session: &HostedSession,
    ) -> Result<sylph_protocol::CapabilityResult> {
        let capabilities = session.capabilities().await?;
        let (supports, link) = match self.adapters.for_agent(agent) {
            Some(adapter) => match adapter.probe().await {
                Ok(info) => (
                    info.supports,
                    serde_json::json!({"observe": {"available": true}, "control": {"available": true}}),
                ),
                Err(_) => (
                    Vec::new(),
                    serde_json::json!({"observe": {"available": true}, "control": {"available": false, "reason": "handshake_failed"}}),
                ),
            },
            None => (
                Vec::new(),
                serde_json::json!({"observe": {"available": true}, "control": {"available": false, "reason": "no_agent"}}),
            ),
        };
        Ok(sylph_protocol::CapabilityResult {
            capabilities,
            supports,
            link: serde_json::from_value(link)?,
            persisted_launch_flag: None,
        })
    }

    async fn control_session<F, Fut>(
        &self,
        session_id: &SessionId,
        operation: F,
    ) -> sylph_protocol::CommandOutcome
    where
        F: FnOnce(HostedSession) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let session = self.sessions.read().await.get(session_id).cloned();
        match session {
            Some(session) => match operation(session).await {
                Ok(()) => sylph_protocol::CommandOutcome::accepted(),
                Err(error) => {
                    if let Some(refusal) = error.downcast_ref::<sylph_protocol::AdapterError>() {
                        sylph_protocol::CommandOutcome::Rejected {
                            code: match refusal.code {
                                sylph_protocol::RefusalCode::UnsupportedField => {
                                    "UNSUPPORTED_FIELD"
                                }
                                sylph_protocol::RefusalCode::InvalidValue => "INVALID_VALUE",
                            }
                            .into(),
                            message: refusal.message.clone(),
                        }
                    } else {
                        sylph_protocol::CommandOutcome::Rejected {
                            code: "CONTROL_FAILED".into(),
                            message: format!("{error:#}"),
                        }
                    }
                }
            },
            None => sylph_protocol::CommandOutcome::Rejected {
                code: "SESSION_NOT_HOSTED".into(),
                message: "no live session on this environment".into(),
            },
        }
    }

    /// Replay every unacknowledged event, preserving producer order.
    pub async fn flush_pending(&self, max_events: usize) -> Result<usize> {
        let _guard = self.send_gate.lock().await;
        let acked_through = self.spool.acked_through(&self.config.producer_id).await?;
        let pending = self
            .spool
            .pending(&self.config.producer_id, max_events)
            .await?;
        if pending.is_empty() {
            return Ok(0);
        }
        validate_pending(
            &pending,
            &self.config.producer_id,
            acked_through.saturating_add(1),
        )?;
        let sink = self.sink.read().await.clone();
        let through = sink.deliver(&pending).await?;
        let last = pending.last().expect("non-empty above").producer_seq;
        let first = pending.first().expect("non-empty above").producer_seq;
        if through < first.saturating_sub(1) || through > last {
            return Err(anyhow!(
                "event sink acknowledged producer sequence {through}, outside delivered range {first}..={last}"
            ));
        }
        Ok(self
            .spool
            .ack_through(&self.config.producer_id, through)
            .await?)
    }

    /// Queue a normalized event. The event is durably spooled before this
    /// method attempts delivery.
    pub async fn enqueue_event(&self, session_id: &SessionId, event: &Event) -> Result<u64> {
        self.enqueue_event_with_turn(session_id, event, None).await
    }

    pub async fn enqueue_event_with_turn(
        &self,
        session_id: &SessionId,
        event: &Event,
        arrival_turn_id: Option<String>,
    ) -> Result<u64> {
        let source_cursor = None;
        let payload = serde_json::to_vec(event)?;
        Ok(self
            .spool
            .append_next(SpoolEvent {
                producer_id: self.config.producer_id.clone(),
                producer_seq: 0,
                kind: "event".into(),
                session_id: session_id.encode(),
                arrival_turn_id,
                payload,
                source_cursor,
                recorded_at_ms: event.recorded_at_ms,
            })
            .await?)
    }

    pub async fn enqueue_event_with_cursor(
        &self,
        session_id: &SessionId,
        event: &Event,
        source_cursor: Option<SourceCursor>,
    ) -> Result<u64> {
        let payload = serde_json::to_vec(event)?;
        Ok(self
            .spool
            .append_next(SpoolEvent {
                producer_id: self.config.producer_id.clone(),
                producer_seq: 0,
                kind: "event".into(),
                session_id: session_id.encode(),
                arrival_turn_id: None,
                payload,
                source_cursor: source_cursor
                    .map(|cursor| serde_json::to_string(&cursor))
                    .transpose()?,
                recorded_at_ms: event.recorded_at_ms,
            })
            .await?)
    }

    /// Queue a source record or request as a lossless JSON envelope.
    pub async fn enqueue_json(
        &self,
        session_id: &str,
        kind: &str,
        value: &serde_json::Value,
        source_cursor: Option<String>,
    ) -> Result<u64> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "kind": kind,
            "payload": value,
        }))?;
        Ok(self
            .spool
            .append_next(SpoolEvent {
                producer_id: self.config.producer_id.clone(),
                producer_seq: 0,
                kind: kind.to_string(),
                session_id: session_id.to_string(),
                arrival_turn_id: None,
                payload,
                source_cursor,
                recorded_at_ms: None,
            })
            .await?)
    }

    pub async fn enqueue_source_state(&self, state: sylph_protocol::SourceState) -> Result<u64> {
        let session_id = state.session_id.encode();
        let value = serde_json::to_value(state)?;
        self.enqueue_json(&session_id, "source_state", &value, None)
            .await
    }

    pub async fn enqueue_turn_input(
        &self,
        session_id: &SessionId,
        turn_id: Option<String>,
        fact: sylph_protocol::TurnInputFact,
    ) -> Result<u64> {
        let value = serde_json::to_value(&fact)?;
        let payload = serde_json::to_vec(&serde_json::json!({
            "kind": "turn_input",
            "sessionId": session_id,
            "turnId": turn_id,
            "payload": value,
        }))?;
        let sequence = self
            .spool
            .append_next(SpoolEvent {
                producer_id: self.config.producer_id.clone(),
                producer_seq: 0,
                kind: "turn_input".into(),
                session_id: session_id.encode(),
                arrival_turn_id: turn_id.clone(),
                payload,
                source_cursor: fact
                    .source_cursor
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                recorded_at_ms: fact.recorded_at_ms,
            })
            .await?;
        Ok(sequence)
    }

    pub async fn enqueue_edge(&self, edge: sylph_protocol::SessionEdge) -> Result<u64> {
        let session_id = edge.child.encode();
        let value = serde_json::to_value(&edge)?;
        self.enqueue_json(&session_id, "edge", &value, None).await
    }

    fn spawn_event_pump(
        &self,
        mut events: EventStream,
        identity: Arc<IdentityState>,
        gate: Option<Arc<Notify>>,
    ) -> tokio::task::JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let mut pending_events: Vec<Event> = Vec::new();
            const MAX_PENDING_IDENTITY: usize = 256;
            while let Some(message) = events.next().await {
                match message {
                    Ok(SessionMessage::Event(event)) => {
                        let Some(session_id) = identity.session_id().await else {
                            if pending_events.len() >= MAX_PENDING_IDENTITY {
                                warn!(
                                    "adapter emitted too many events before assigning a native id"
                                );
                                break;
                            }
                            pending_events.push(event);
                            let notified = identity.changed.notified();
                            tokio::pin!(notified);
                            tokio::select! {
                                _ = &mut notified => {},
                                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
                            }
                            if let Some(session_id) = identity.session_id().await {
                                for buffered in pending_events.drain(..) {
                                    let turn_id =
                                        runtime.active_turns.read().await.get(&session_id).cloned();
                                    let terminal = buffered.kind == sylph_protocol::EventKind::Turn
                                        && buffered.phase == Some(sylph_protocol::Phase::Completed);
                                    if let Err(error) = runtime
                                        .enqueue_event_with_turn(
                                            &session_id,
                                            &buffered,
                                            turn_id.clone(),
                                        )
                                        .await
                                    {
                                        warn!("could not spool buffered adapter event: {error:#}");
                                        runtime
                                            .clear_active_turn_if_matches(
                                                &session_id,
                                                turn_id.as_deref(),
                                            )
                                            .await;
                                        return;
                                    }
                                    if terminal {
                                        runtime
                                            .clear_active_turn_if_matches(
                                                &session_id,
                                                turn_id.as_deref(),
                                            )
                                            .await;
                                    }
                                }
                            }
                            continue;
                        };
                        let turn_id = runtime.active_turns.read().await.get(&session_id).cloned();
                        let terminal = event.kind == sylph_protocol::EventKind::Turn
                            && event.phase == Some(sylph_protocol::Phase::Completed);
                        if let Err(error) = runtime
                            .enqueue_event_with_turn(&session_id, &event, turn_id.clone())
                            .await
                        {
                            warn!("could not spool adapter event: {error:#}");
                            break;
                        }
                        if terminal {
                            runtime
                                .clear_active_turn_if_matches(&session_id, turn_id.as_deref())
                                .await;
                        }
                    }
                    Ok(SessionMessage::NativeRecord(record)) => {
                        let Some(session) = identity.session_id().await else {
                            warn!("native record arrived before a native id; refusing a fake identity");
                            break;
                        };
                        let record = match runtime.retain_native_sidecars(&session, record).await {
                            Ok(record) => record,
                            Err(error) => {
                                warn!("could not snapshot native sidecar: {error:#}");
                                break;
                            }
                        };
                        let value = serde_json::to_value(&record).unwrap_or_default();
                        if let Err(error) = runtime
                            .enqueue_json(&session.encode(), "native_record", &value, None)
                            .await
                        {
                            warn!("could not spool native record: {error:#}");
                            break;
                        }
                    }
                    Ok(SessionMessage::Capabilities(capabilities)) => {
                        let Some(session) = identity.session_id().await else {
                            warn!("capability update arrived before a native id; refusing a fake identity");
                            break;
                        };
                        let value = serde_json::to_value(&capabilities).unwrap_or_default();
                        if let Err(error) = runtime
                            .enqueue_json(&session.encode(), "capabilities", &value, None)
                            .await
                        {
                            warn!("could not spool capability update: {error:#}");
                            break;
                        }
                    }
                    Ok(SessionMessage::Request(request)) => {
                        let Some(session) = identity.session_id().await else {
                            warn!("request arrived before a native id; refusing a fake identity");
                            break;
                        };
                        let value = serde_json::to_value(&request).unwrap_or_default();
                        if let Err(error) = runtime
                            .enqueue_json(&session.encode(), "request", &value, None)
                            .await
                        {
                            warn!("could not spool agent request: {error:#}");
                            break;
                        }
                    }
                    Ok(SessionMessage::TurnInput(fact)) => {
                        let Some(session_id) = identity.session_id().await else {
                            warn!("turn-input fact arrived before a native id; refusing a fake identity");
                            break;
                        };
                        let turn_id = runtime.active_turns.read().await.get(&session_id).cloned();
                        if let Err(error) =
                            runtime.enqueue_turn_input(&session_id, turn_id, fact).await
                        {
                            warn!("could not spool native turn-input fact: {error:#}");
                            break;
                        }
                    }
                    Err(error) => {
                        let Some(session) = identity.session_id().await else {
                            warn!("adapter read error arrived before a native id; refusing a fake identity");
                            break;
                        };
                        let value = serde_json::json!({"error": format!("{error:#}")});
                        if let Err(spool_error) = runtime
                            .enqueue_json(&session.encode(), "read_error", &value, None)
                            .await
                        {
                            warn!("could not spool adapter read error: {spool_error:#}");
                            break;
                        }
                        debug!("adapter event read error: {error:#}");
                    }
                }
                // Best effort delivery. Failure leaves rows in the spool;
                // reconnect calls flush_pending and starts at the same prefix.
                let _ = runtime.flush_pending(64).await;
            }
            // A stream ending means no later event can legitimately belong to
            // this turn. Drop the association even when the adapter ended
            // without a terminal turn event (for example after a link loss),
            // so a subsequent turn cannot inherit stale provenance.
            if let Some(session_id) = identity.session_id().await {
                let native_source = runtime.native_sources.read().await.contains(&session_id);
                runtime.clear_active_turn(&session_id).await;
                // A finite adopt pass or a watcher that reaches EOF no longer
                // owns a pump task, but it remains a native-store-only source:
                // file reads and the observe/control facet still need its
                // contained workdir until an explicit Close.
                runtime.native_pump_handles.lock().await.remove(&session_id);
                if native_source {
                    // A finite adopt pass may end without a vendor turn-close
                    // event. Keep the source termination visible to the
                    // server's diagnostic/settlement path.
                    let value = serde_json::json!({"reason": "source_closed"});
                    if let Err(error) = runtime
                        .enqueue_json(&session_id.encode(), "source_closed", &value, None)
                        .await
                    {
                        warn!("could not spool native source close: {error:#}");
                    } else {
                        let _ = runtime.flush_pending(64).await;
                    }
                }
            }
        })
    }

    /// Snapshot opaque vendor sidecars locally before their native record is sent.
    /// Chunks use the normal durable producer spool, so the server can retain the
    /// body before acknowledging the referencing record without putting large bytes
    /// into timeline events or relying on the vendor store later.
    async fn retain_native_sidecars(
        &self,
        session_id: &SessionId,
        mut record: sylph_protocol::NativeRecord,
    ) -> Result<sylph_protocol::NativeRecord> {
        use std::io::Read;
        for blob in &mut record.blobs {
            let Some(locator) = blob.locator.take() else {
                continue;
            };
            let path = match self.config.path_policy.contain(&locator) {
                Ok(path) => path,
                Err(error) => {
                    blob.error = Some(format!("sidecar locator refused: {error}"));
                    continue;
                }
            };
            let mut file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) => {
                    blob.error = Some(error.to_string());
                    continue;
                }
            };
            let metadata = match file.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    blob.error = Some(error.to_string());
                    continue;
                }
            };
            let size = metadata.len();
            blob.size = size;
            if !metadata.is_file() {
                blob.error = Some("native sidecar is not a regular file".into());
                continue;
            }
            if size > sylph_protocol::MAX_NATIVE_BLOB_BYTES {
                blob.error = Some(format!(
                    "native sidecar is {size} bytes; limit is {}",
                    sylph_protocol::MAX_NATIVE_BLOB_BYTES
                ));
                continue;
            }
            let mut offset = 0u64;
            while offset < size {
                // Never spool bytes beyond the declared snapshot. A growing file would
                // otherwise publish an invalid chunk that blocks the producer prefix.
                let length = (size - offset).min(sylph_protocol::NATIVE_BLOB_CHUNK_BYTES as u64);
                let mut bytes = vec![0u8; length as usize];
                if let Err(error) = file.read_exact(&mut bytes) {
                    blob.error = Some(error.to_string());
                    break;
                }
                let chunk = sylph_protocol::NativeBlobChunk {
                    source: record.source.clone(),
                    version: record.version,
                    name: blob.name.clone(),
                    size,
                    offset,
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    final_chunk: offset + length == size,
                };
                let value = serde_json::to_value(chunk)?;
                if let Err(error) = self
                    .enqueue_json(&session_id.encode(), "native_blob_chunk", &value, None)
                    .await
                {
                    if !matches!(error.downcast_ref::<crate::spool::SpoolError>(),
                        Some(crate::spool::SpoolError::Overflow { .. })) {
                        return Err(error);
                    }
                    // A connected server can consume an object larger than the spool.
                    // If delivery fails, every accepted chunk stays durable for reconnect.
                    while self.flush_pending(64).await? > 0 {}
                    self.enqueue_json(&session_id.encode(), "native_blob_chunk", &value, None)
                        .await?;
                }
                offset += length;
            }
            if blob.error.is_none() {
                let unchanged = file.metadata().is_ok_and(|after| {
                    after.len() == size && after.modified().ok() == metadata.modified().ok()
                });
                if !unchanged || offset != size {
                    blob.error = Some("native sidecar changed during snapshot".into());
                }
            }
            if size == 0 {
                let chunk = sylph_protocol::NativeBlobChunk {
                    source: record.source.clone(),
                    version: record.version,
                    name: blob.name.clone(),
                    size,
                    offset: 0,
                    data: String::new(),
                    final_chunk: true,
                };
                self.enqueue_json(
                    &session_id.encode(),
                    "native_blob_chunk",
                    &serde_json::to_value(chunk)?,
                    None,
                )
                .await?;
            }
        }
        Ok(record)
    }
}

/// Resolve a file operation relative to one session's workdir.
///
/// The global browse roots answer whether an envagent may run anywhere at all;
/// file operations have the narrower contract of staying inside the selected
/// session workdir. `PathBuf::join` silently discards its prefix for an
/// absolute child, so reject absolute names before joining and re-check the
/// canonical result component-wise after resolving `..` and symlinks.
fn contain_session_path(
    policy: &PathPolicy,
    root: &std::path::Path,
    relative: &str,
) -> Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    let relative_path = std::path::Path::new(relative);
    if relative_path.is_absolute() {
        anyhow::bail!("file path must be relative to the session workdir");
    }
    let mut depth = 0usize;
    for component in relative_path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::ParentDir if depth > 0 => depth -= 1,
            std::path::Component::ParentDir => {
                anyhow::bail!("file path escapes the session workdir")
            }
            _ => anyhow::bail!("file path escapes the session workdir"),
        }
    }
    let path = policy.contain(root.join(relative_path))?;
    if !path.starts_with(root) {
        anyhow::bail!("file path escapes the session workdir");
    }
    Ok(path)
}

fn contain_session_write_target(
    policy: &PathPolicy,
    root: &std::path::Path,
    relative: &str,
) -> Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    let relative_path = std::path::Path::new(relative);
    if relative_path.is_absolute() {
        anyhow::bail!("file path must be relative to the session workdir");
    }
    let target = policy.contain_write_target(root.join(relative_path))?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("file path has no parent"))?
        .canonicalize()?;
    if !parent.starts_with(root) {
        anyhow::bail!("file path escapes the session workdir");
    }
    Ok(target)
}

/// Resolve a delete target without following its final symlink component.
///
/// A write must reject an existing symlink that points outside the workdir,
/// because opening it would mutate the outside target. A delete is different:
/// unlinking that final symlink is safe and is the operation users need to
/// repair an escaping link. Only the parent is canonicalized and contained;
/// the leaf is inspected with `symlink_metadata` by the caller.
fn contain_session_remove_target(
    policy: &PathPolicy,
    root: &std::path::Path,
    relative: &str,
) -> Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    let relative_path = std::path::Path::new(relative);
    if relative_path.is_absolute() {
        anyhow::bail!("file path must be relative to the session workdir");
    }
    let parent = relative_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let leaf = relative_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("file path has no final component"))?;
    let canonical_parent = policy.contain(root.join(parent))?;
    if !canonical_parent.starts_with(&root) {
        anyhow::bail!("file path escapes the session workdir");
    }
    Ok(canonical_parent.join(leaf))
}

fn validate_pending(events: &[SpoolEvent], producer_id: &str, expected_first: u64) -> Result<()> {
    let mut expected = None;
    for event in events {
        if event.producer_id != producer_id {
            return Err(anyhow!("spool row belongs to a different producer"));
        }
        if let Some(expected) = expected {
            if event.producer_seq != expected {
                return Err(anyhow!(
                    "spool has a producer gap: expected {expected}, got {}",
                    event.producer_seq
                ));
            }
        } else if event.producer_seq != expected_first {
            return Err(anyhow!(
                "spool has a missing prefix: expected {}, got {}",
                expected_first,
                event.producer_seq
            ));
        }
        expected = Some(event.producer_seq.saturating_add(1));
    }
    Ok(())
}

/// A sink useful for deterministic protocol tests.
#[derive(Default)]
pub struct RecordingSink {
    accepted: Mutex<Vec<SpoolEvent>>,
}

impl RecordingSink {
    pub async fn events(&self) -> Vec<SpoolEvent> {
        self.accepted.lock().await.clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn deliver(&self, events: &[SpoolEvent]) -> Result<u64> {
        let mut accepted = self.accepted.lock().await;
        accepted.extend_from_slice(events);
        Ok(events.last().map(|event| event.producer_seq).unwrap_or(0))
    }
}

/// A compact serializable command envelope used by fixture integrations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCommand {
    pub request_id: String,
    pub session_id: String,
    pub operation: String,
    #[serde(default)]
    pub args: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PathPolicy;

    /// An agent that declares whether it dictates, and records the spec it was opened with.
    ///
    /// The recorded [`RunSpec`] is the observable the id-naming tests assert on, because the
    /// defect they cover is invisible in the *outcome*: `claude` accepts `New` with only a
    /// warning and runs, so a test reading the reply cannot tell a dictated open from a
    /// degraded one. What distinguishes them is the variant the envagent handed the adapter.
    #[derive(Clone, Default)]
    struct SpySource {
        dictates: bool,
        specs: Arc<std::sync::Mutex<Vec<RunSpec>>>,
    }

    impl SpySource {
        fn new(dictates: bool) -> Self {
            Self {
                dictates,
                specs: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn opened(&self) -> Vec<RunSpec> {
            self.specs.lock().expect("spy is not poisoned").clone()
        }
    }

    impl AdapterSource for SpySource {
        fn for_agent(&self, name: &str) -> Option<Box<dyn sylph_protocol::Adapter>> {
            (name == SPY_AGENT).then(|| {
                Box::new(SpyAdapter {
                    dictates: self.dictates,
                    specs: self.specs.clone(),
                }) as Box<dyn sylph_protocol::Adapter>
            })
        }

        fn known_agents(&self) -> Vec<String> {
            vec![SPY_AGENT.to_string()]
        }
    }

    /// Named to parse as an [`AgentName`], since the runtime validates one before running.
    const SPY_AGENT: &str = "spy";

    struct SpyAdapter {
        dictates: bool,
        specs: Arc<std::sync::Mutex<Vec<RunSpec>>>,
    }

    #[async_trait]
    impl sylph_protocol::Adapter for SpyAdapter {
        async fn probe(&self) -> Result<sylph_protocol::AgentInfo> {
            Ok(sylph_protocol::AgentInfo {
                name: SPY_AGENT.to_string(),
                version: Some("1.0.0".to_string()),
                flags: sylph_protocol::FlagSchema::new(),
                supports: Vec::new(),
                dictates_native_id: self.dictates,
                native_fidelity: sylph_protocol::Fidelity::Blocks,
            })
        }

        async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>> {
            let native_id = match &spec.native_id {
                sylph_protocol::RequestedNativeId::Dictate(id)
                | sylph_protocol::RequestedNativeId::Resume(id) => id.clone(),
                // What a non-dictating vendor does: assign one and report it.
                sylph_protocol::RequestedNativeId::New => {
                    NativeId::new(format!("spy-{}", uuid::Uuid::new_v4()))
                }
            };
            self.specs.lock().expect("spy is not poisoned").push(spec);
            Ok(Box::new(SpySession {
                native_id,
                events: Mutex::new(Some(())),
            }))
        }

        async fn adopt(&self, _id: &NativeId) -> Result<EventStream> {
            Err(anyhow!("the spy agent has no native store"))
        }

        async fn reproject(&self, _records: Vec<sylph_protocol::NativeRecord>) -> Result<EventStream> {
            Err(anyhow!("the spy agent has no native records"))
        }

        async fn watch(&self, _id: &NativeId) -> Result<EventStream> {
            Err(anyhow!("the spy agent has no native store"))
        }

        async fn enumerate(
            &self,
            _filter: &sylph_protocol::StoreFilter,
        ) -> Result<Vec<sylph_protocol::NativeSummary>> {
            Ok(Vec::new())
        }
    }

    struct SpySession {
        native_id: NativeId,
        /// Yielded once, because [`Session::events`] is a hand-off and not a subscription.
        events: Mutex<Option<()>>,
    }

    #[async_trait]
    impl Session for SpySession {
        fn native_id(&self) -> Option<NativeId> {
            Some(self.native_id.clone())
        }

        async fn capabilities(&self) -> Result<sylph_protocol::SessionCapabilities> {
            Ok(sylph_protocol::SessionCapabilities::default())
        }

        async fn send(&self, _turn: TurnInput) -> Result<()> {
            Ok(())
        }

        async fn send_initial(&self, _turn: TurnInput) -> Result<NativeId> {
            Ok(self.native_id.clone())
        }

        async fn interrupt(&self) -> Result<()> {
            Ok(())
        }

        async fn steer(&self, _text: &str, _expected: Option<&str>) -> Result<()> {
            Ok(())
        }

        async fn queue(&self, _text: &str) -> Result<()> {
            Ok(())
        }

        async fn set_config(
            &self,
            _id: &str,
            _value: serde_json::Value,
        ) -> Result<Vec<sylph_protocol::ConfigOption>> {
            Ok(Vec::new())
        }

        async fn resolve(&self, _resolution: sylph_protocol::Resolution) -> Result<()> {
            Ok(())
        }

        fn events(&mut self) -> Option<EventStream> {
            self.events.try_lock().ok()?.take()?;
            Some(Box::pin(futures::stream::empty()))
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    /// A runtime whose only agent is the spy, plus the spy itself.
    async fn spy_runtime(dictates: bool) -> (EnvAgent, SpySource, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        let source = SpySource::new(dictates);
        let runtime = EnvAgent::open_with_adapters(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "p".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 1024 * 1024),
                path_policy: PathPolicy::from_roots([root]).unwrap(),
            },
            Arc::new(RecordingSink::default()),
            Arc::new(source.clone()),
        )
        .await
        .unwrap();
        (runtime, source, dir)
    }

    fn workdir_of(runtime: &EnvAgent) -> String {
        runtime
            .config
            .path_policy
            .roots()
            .first()
            .expect("the harness configured one root")
            .display()
            .to_string()
    }

    /// Preparing a session for a dictating agent names the id, so identity precedes the
    /// first event.
    ///
    /// The promise this pins is the one the defect broke: `prepare` hardcoded `New`, which
    /// `pi` refuses outright and `claude` accepts while warning, so the whole `Dictate`
    /// branch was unreachable in production.
    #[tokio::test]
    async fn preparing_a_dictating_agent_names_the_native_id() {
        let (runtime, spy, _dir) = spy_runtime(true).await;
        let outcome = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Prepare {
                draft_id: "draft-1".into(),
                agent: SPY_AGENT.into(),
                workdir: workdir_of(&runtime),
                flags: BTreeMap::new(),
                inject_identity: false,
                identity_preamble: None,
                permission_mode: None,
            })
            .await;
        assert!(
            matches!(outcome, sylph_protocol::CommandOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        let opened = spy.opened();
        assert_eq!(opened.len(), 1, "one prepare opens one vendor session");
        let sylph_protocol::RequestedNativeId::Dictate(id) = &opened[0].native_id else {
            panic!(
                "a dictating agent must be opened with a dictated id, got {:?}",
                opened[0].native_id
            );
        };
        // A parseable v4 UUID, because `claude` accepts nothing looser, and the value is the
        // vendor's own native id once it takes it.
        assert!(
            uuid::Uuid::parse_str(id.as_str()).is_ok(),
            "the dictated id must be a UUID claude would accept, got {id}"
        );
    }

    /// A non-dictating agent is still opened with `New`, so the vendor assigns and reports.
    ///
    /// The other half of the same decision, and the reason it reads `dictates_native_id`
    /// rather than always dictating: `codex` has no `threadId` parameter on `thread/start`,
    /// so a dictated id there is a refusal.
    #[tokio::test]
    async fn preparing_a_non_dictating_agent_lets_the_vendor_assign_its_own_id() {
        let (runtime, spy, _dir) = spy_runtime(false).await;
        let outcome = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Prepare {
                draft_id: "draft-1".into(),
                agent: SPY_AGENT.into(),
                workdir: workdir_of(&runtime),
                flags: BTreeMap::new(),
                inject_identity: false,
                identity_preamble: None,
                permission_mode: None,
            })
            .await;
        assert!(
            matches!(outcome, sylph_protocol::CommandOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            spy.opened()[0].native_id,
            sylph_protocol::RequestedNativeId::New,
            "a vendor that assigns its own id must not be handed one"
        );
    }

    /// Running a dictating agent with no session id names the id too.
    ///
    /// A separate test from the prepare case because it is a separate call site with the
    /// same hole: the resume path resolved `Resume(id)` when a session id was known and fell
    /// back to `New` when it was not, so a dictating agent reaching `Run` directly was
    /// refused exactly as it was through `Prepare`.
    #[tokio::test]
    async fn running_a_dictating_agent_without_a_session_id_names_the_native_id() {
        let (runtime, spy, _dir) = spy_runtime(true).await;
        let outcome = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Run {
                session_id: None,
                draft_id: None,
                agent: SPY_AGENT.into(),
                workdir: workdir_of(&runtime),
                flags: BTreeMap::new(),
                inject_identity: false,
                identity_preamble: None,
                permission_mode: None,
                initial_input: None,
                initial_turn_id: None,
            })
            .await;
        assert!(
            matches!(outcome, sylph_protocol::CommandOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        let opened = spy.opened();
        let sylph_protocol::RequestedNativeId::Dictate(id) = &opened[0].native_id else {
            panic!(
                "run with no session id opens a new session, so a dictating agent is \
                 handed an id; got {:?}",
                opened[0].native_id
            );
        };
        assert!(uuid::Uuid::parse_str(id.as_str()).is_ok(), "got {id}");
    }

    /// A known session id still resumes, and is never re-dictated.
    ///
    /// The guard on the fix: dictating a *new* id where a resume was asked for would open a
    /// second conversation under a fresh key rather than continuing the one that exists.
    #[tokio::test]
    async fn running_with_a_session_id_resumes_that_id_rather_than_dictating_a_new_one() {
        let (runtime, spy, _dir) = spy_runtime(true).await;
        let existing = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse(SPY_AGENT).unwrap(),
            NativeId::new("already-running"),
        );
        let outcome = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Run {
                session_id: Some(existing.clone()),
                draft_id: None,
                agent: SPY_AGENT.into(),
                workdir: workdir_of(&runtime),
                flags: BTreeMap::new(),
                inject_identity: false,
                identity_preamble: None,
                permission_mode: None,
                initial_input: None,
                initial_turn_id: None,
            })
            .await;
        assert!(
            matches!(outcome, sylph_protocol::CommandOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            spy.opened()[0].native_id,
            sylph_protocol::RequestedNativeId::Resume(existing.native_id.clone()),
            "a resume must carry the id that already names the session"
        );
    }

    /// Two prepares never dictate the same id.
    ///
    /// Stated because the collision is silent at this layer and fatal one layer up: a
    /// repeated id is a second row under one `(environment, agent, native_id)` key, which
    /// the store correctly refuses as a `409`. A per-process counter has exactly this
    /// defect after an envagent restart, which is why the value is a v4 UUID.
    #[tokio::test]
    async fn two_prepared_sessions_are_dictated_distinct_ids() {
        let (runtime, spy, _dir) = spy_runtime(true).await;
        for draft in ["draft-1", "draft-2"] {
            let outcome = runtime
                .handle_command(sylph_protocol::EnvironmentCommand::Prepare {
                    draft_id: draft.into(),
                    agent: SPY_AGENT.into(),
                    workdir: workdir_of(&runtime),
                    flags: BTreeMap::new(),
                    inject_identity: false,
                    identity_preamble: None,
                    permission_mode: None,
                })
                .await;
            assert!(
                matches!(outcome, sylph_protocol::CommandOutcome::Accepted { .. }),
                "{draft}: {outcome:?}"
            );
        }
        let opened = spy.opened();
        assert_eq!(opened.len(), 2);
        assert_ne!(
            opened[0].native_id, opened[1].native_id,
            "two sessions dictated one id collide on the store's identity key"
        );
    }

    #[tokio::test]
    async fn enqueue_then_flush_is_spooled_before_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        let policy = PathPolicy::from_roots([root]).unwrap();
        let sink = Arc::new(RecordingSink::default());
        let runtime = EnvAgent::open(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "producer-a".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 1024 * 1024),
                path_policy: policy,
            },
            sink.clone(),
        )
        .await
        .unwrap();
        let session = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("fixture").unwrap(),
            NativeId::new("n1"),
        );
        let event = Event::new(sylph_protocol::EventKind::Status, None, 1);
        assert_eq!(runtime.enqueue_event(&session, &event).await.unwrap(), 1);
        assert_eq!(
            runtime.spool().size_bytes().await.unwrap(),
            serde_json::to_vec(&event).unwrap().len() as u64
        );
        runtime.flush_pending(10).await.unwrap();
        assert!(runtime.spool().is_empty().await.unwrap());
        assert_eq!(sink.events().await.len(), 1);
    }

    #[tokio::test]
    async fn native_sidecars_larger_than_the_spool_survive_vendor_removal() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("tool-output.txt");
        let bytes = vec![b'x'; sylph_protocol::NATIVE_BLOB_CHUNK_BYTES * 2 + 17];
        std::fs::write(&sidecar, &bytes).unwrap();
        let sink = Arc::new(RecordingSink::default());
        let runtime = EnvAgent::open(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "p".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 2 * 1024 * 1024),
                path_policy: PathPolicy::from_roots([dir.path().to_path_buf()]).unwrap(),
            },
            sink.clone(),
        )
        .await
        .unwrap();
        let session = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("claude").unwrap(),
            NativeId::new("n1"),
        );
        let record = sylph_protocol::NativeRecord {
            source: "transcript.jsonl".into(),
            version: 1,
            payload: serde_json::json!({"toolUseResult": {"persistedOutputPath": sidecar}}),
            raw_json: None,
            blobs: vec![sylph_protocol::NativeBlob {
                name: "tool-output.txt".into(),
                size: bytes.len() as u64,
                bytes: None,
                locator: Some(sidecar.to_string_lossy().into()),
                error: None,
            }],
        };
        let retained = runtime
            .retain_native_sidecars(&session, record)
            .await
            .unwrap();
        std::fs::remove_file(&sidecar).unwrap();
        assert!(retained.blobs[0].locator.is_none());
        assert!(retained.blobs[0].bytes.is_none());
        let mut pending = sink.events().await;
        pending.extend(runtime.spool.pending("p", 10).await.unwrap());
        assert_eq!(pending.len(), 3);
        let mut reconstructed = Vec::new();
        for (index, row) in pending.iter().enumerate() {
            assert_eq!(row.kind, "native_blob_chunk");
            let envelope: serde_json::Value = serde_json::from_slice(&row.payload).unwrap();
            let chunk: sylph_protocol::NativeBlobChunk =
                serde_json::from_value(envelope["payload"].clone()).unwrap();
            assert_eq!(chunk.offset, reconstructed.len() as u64);
            assert_eq!(chunk.final_chunk, index == 2);
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(chunk.data)
                .unwrap();
            assert!(decoded.len() <= sylph_protocol::NATIVE_BLOB_CHUNK_BYTES);
            reconstructed.extend(decoded);
        }
        assert_eq!(reconstructed, bytes);
    }

    #[tokio::test]
    async fn absent_native_sidecars_are_retained_as_explicit_errors() {
        let (runtime, _spy, dir) = spy_runtime(false).await;
        let session = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("claude").unwrap(),
            NativeId::new("n1"),
        );
        let record = sylph_protocol::NativeRecord {
            source: "transcript.jsonl".into(),
            version: 1,
            raw_json: None,
            payload: serde_json::json!({}),
            blobs: vec![sylph_protocol::NativeBlob {
                name: "missing".into(),
                size: 42,
                bytes: None,
                locator: Some(dir.path().join("missing").to_string_lossy().into()),
                error: None,
            }],
        };
        let retained = runtime
            .retain_native_sidecars(&session, record)
            .await
            .unwrap();
        assert!(retained.blobs[0].locator.is_none());
        assert!(retained.blobs[0].error.is_some());
        assert_eq!(retained.blobs[0].size, 42);
    }

    #[tokio::test]
    async fn run_rejects_a_workdir_outside_the_local_policy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let sink = Arc::new(RecordingSink::default());
        let runtime = EnvAgent::open(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "p".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 1024),
                path_policy: PathPolicy::from_roots([root]).unwrap(),
            },
            sink,
        )
        .await
        .unwrap();
        let spec = RunSpec {
            workdir: outside.display().to_string(),
            native_id: sylph_protocol::RequestedNativeId::New,
            inject_identity: false,
            identity_preamble: None,
            flags: BTreeMap::new(),
            permission_mode: None,
        };
        assert!(runtime.run("not-installed", spec).await.is_err());
    }

    #[tokio::test]
    async fn file_commands_are_contained_and_refuse_overwrite_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"secret").unwrap();
        let runtime = EnvAgent::open(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "p".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 1024),
                path_policy: PathPolicy::from_roots([root.clone()]).unwrap(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .unwrap();
        let session = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("fixture").unwrap(),
            NativeId::new("n1"),
        );
        runtime.workdirs.write().await.insert(session.clone(), root);
        let created = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Files {
                session_id: session.clone(),
                operation: sylph_protocol::FileOperation::Write {
                    relative: "a.txt".into(),
                    data: base64::engine::general_purpose::STANDARD.encode(b"one"),
                    overwrite: false,
                },
            })
            .await;
        assert!(matches!(
            created,
            sylph_protocol::CommandOutcome::Accepted { .. }
        ));
        let refused = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Files {
                session_id: session.clone(),
                operation: sylph_protocol::FileOperation::Write {
                    relative: "a.txt".into(),
                    data: base64::engine::general_purpose::STANDARD.encode(b"two"),
                    overwrite: false,
                },
            })
            .await;
        assert!(matches!(
            refused,
            sylph_protocol::CommandOutcome::Rejected { code, .. } if code == "FILES_FAILED"
        ));

        let escaped = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Files {
                session_id: session.clone(),
                operation: sylph_protocol::FileOperation::Read {
                    relative: "../outside.txt".into(),
                    limit: None,
                },
            })
            .await;
        assert!(matches!(
            escaped,
            sylph_protocol::CommandOutcome::Rejected { code, .. } if code == "FILES_FAILED"
        ));

        let absolute = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Files {
                session_id: session,
                operation: sylph_protocol::FileOperation::Read {
                    relative: "/etc/passwd".into(),
                    limit: None,
                },
            })
            .await;
        assert!(matches!(
            absolute,
            sylph_protocol::CommandOutcome::Rejected { code, .. } if code == "FILES_FAILED"
        ));
    }

    #[tokio::test]
    async fn file_read_returns_bounded_base64_with_size_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        let payload = vec![b'x'; 1024];
        std::fs::write(root.join("large.bin"), &payload).unwrap();
        let runtime = EnvAgent::open(
            EnvAgentConfig {
                environment_id: "env7fk2".into(),
                producer_id: "p".into(),
                spool: SpoolConfig::new(dir.path().join("spool.db"), 1024),
                path_policy: PathPolicy::from_roots([root.clone()]).unwrap(),
            },
            Arc::new(RecordingSink::default()),
        )
        .await
        .unwrap();
        let session = SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("fixture").unwrap(),
            NativeId::new("n1"),
        );
        runtime.workdirs.write().await.insert(session.clone(), root);

        let outcome = runtime
            .handle_command(sylph_protocol::EnvironmentCommand::Files {
                session_id: session,
                operation: sylph_protocol::FileOperation::Read {
                    relative: "large.bin".into(),
                    limit: Some(128),
                },
            })
            .await;
        let sylph_protocol::CommandOutcome::Accepted {
            result: Some(result),
        } = outcome
        else {
            panic!("read was not accepted: {outcome:?}");
        };
        assert_eq!(result["size"], 1024);
        assert_eq!(result["truncated"], true);
        assert_eq!(
            result["data"],
            base64::engine::general_purpose::STANDARD.encode(&payload[..128])
        );
        assert!(
            result.get("bytes").is_none(),
            "wire reads must not use JSON byte arrays"
        );
    }

    #[tokio::test]
    async fn an_ack_for_another_producer_does_not_release_waiters() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let sink = ChannelEventSink::new(
            sender,
            EnvironmentId::parse("env7fk2").unwrap(),
            ProducerId::parse("producer-a").unwrap(),
            1,
        );
        let (waiter, receiver) = tokio::sync::oneshot::channel();
        sink.pending.lock().await.insert(1, waiter);
        sink.acknowledge(&sylph_protocol::Ack {
            producer_id: ProducerId::parse("producer-b").unwrap(),
            accepted_through: ProducerSeq::new(1),
            server_seq_through: None,
        })
        .await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), receiver)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn channel_sink_waits_for_the_server_ack_before_returning() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let sink = Arc::new(ChannelEventSink::new(
            sender,
            EnvironmentId::parse("env7fk2").unwrap(),
            ProducerId::parse("producer-a").unwrap(),
            1,
        ));
        let row = SpoolEvent {
            producer_id: "producer-a".into(),
            producer_seq: 1,
            kind: "event".into(),
            session_id: "env7fk2:fixture:n".into(),
            arrival_turn_id: None,
            payload: serde_json::to_vec(&Event::new(sylph_protocol::EventKind::Status, None, 1))
                .unwrap(),
            source_cursor: None,
            recorded_at_ms: None,
        };
        let sink_for_send = sink.clone();
        let send = tokio::spawn(async move { sink_for_send.deliver(&[row]).await.unwrap() });
        let message = receiver.recv().await.unwrap();
        assert!(matches!(message, WireMessage::EventBatch { .. }));
        sink.acknowledge(&sylph_protocol::Ack {
            producer_id: ProducerId::parse("producer-a").unwrap(),
            accepted_through: ProducerSeq::new(1),
            server_seq_through: Some(sylph_protocol::ServerSeq::new(7)),
        })
        .await;
        assert_eq!(send.await.unwrap(), 1);
    }

    /// The adopt payload carries the relation, read back the way the server reads it.
    ///
    /// **Asserting on the key would prove only that this test and the producer agree.**
    /// The defect was a real mismatch across the seam — the server did
    /// `result.get("parentRelation")` and deserialized into
    /// `Option<SessionEdgeRelation>`, and nothing wrote the key — so what has to be
    /// exercised is that expression, against this payload, for both a `delegation` and
    /// an absent relation. A rename on either side then fails here rather than
    /// flattening every adopted tree in silence.
    #[test]
    fn the_adopt_payload_carries_the_relation_the_server_deserializes() {
        let source = HostedSource {
            session_id: SessionId::new(
                EnvironmentId::parse("env7fk2").unwrap(),
                sylph_protocol::AgentName::parse("codex").unwrap(),
                NativeId::new("child"),
            ),
            workdir: "/tmp/project".into(),
            fidelity: sylph_protocol::Fidelity::Messages,
            title: None,
            started_at_ms: None,
            ended_at_ms: None,
            parent_native_id: Some(NativeId::new("parent")),
            parent_relation: Some(sylph_protocol::SessionEdgeRelation::Delegation),
        };

        // Verbatim from `crates/server/src/api/sessions.rs`'s remote adopt path.
        let read = |result: &serde_json::Value| -> Option<sylph_protocol::SessionEdgeRelation> {
            result
                .get("parentRelation")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        };

        assert_eq!(
            read(&source.to_command_result()),
            Some(sylph_protocol::SessionEdgeRelation::Delegation),
            "the server cannot journal an edge whose relation it never received"
        );
        assert_eq!(
            source.to_command_result()["parentNativeId"],
            "parent",
            "a relation with no pointer is not an edge either"
        );

        // The other half of the contract: absent stays absent rather than defaulting to
        // a relation, since a guessed one is not withdrawable once journaled.
        let rootless = HostedSource {
            parent_native_id: None,
            parent_relation: None,
            ..source
        };
        assert_eq!(read(&rootless.to_command_result()), None);
    }
}
