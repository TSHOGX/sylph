//! Vendor process hosting, local path authority and durable capture upload.

use crate::{
    adapters::AdapterSource,
    attachments::{attachments_dir, stage as stage_attachments},
    path::PathPolicy,
    spool::{
        capture::{
            CaptureAuthorization as LocalAuthorization, CaptureLimits,
            CaptureReceipt as LocalReceipt, CaptureRegistry,
        },
        Spool, SpoolConfig,
    },
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use sylph_protocol::{
    trace::{Issuer, ObservationRef, TimeBasis, TimeEvidence, TimeUnit},
    *,
};
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};

mod attachments;
mod browse;
mod database;
mod requests;

#[async_trait]
pub trait CaptureTransport: Send + Sync {
    async fn upload(&self, operation: CaptureUpload) -> Result<CaptureReply>;
    async fn notify(&self, message: WireMessage) -> Result<()>;
}

pub struct ChannelCaptureTransport {
    sender: mpsc::Sender<WireMessage>,
    connection_epoch: u64,
    pending: Mutex<HashMap<String, oneshot::Sender<CaptureReply>>>,
    closed: AtomicBool,
}
impl ChannelCaptureTransport {
    pub fn new(sender: mpsc::Sender<WireMessage>, connection_epoch: u64) -> Self {
        Self {
            sender,
            connection_epoch,
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }
    pub async fn reply(&self, request_id: &str, outcome: CaptureReply, connection_epoch: u64) {
        if connection_epoch != self.connection_epoch {
            return;
        }
        if let Some(waiter) = self.pending.lock().await.remove(request_id) {
            let _ = waiter.send(outcome);
        }
    }
    pub async fn fail_pending(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending.lock().await.clear();
    }
}
#[async_trait]
impl CaptureTransport for ChannelCaptureTransport {
    async fn upload(&self, operation: CaptureUpload) -> Result<CaptureReply> {
        if self.closed.load(Ordering::Acquire) {
            anyhow::bail!("capture connection is closed");
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);
        if self
            .sender
            .send(WireMessage::CaptureUpload {
                request_id: request_id.clone(),
                operation,
                connection_epoch: self.connection_epoch,
            })
            .await
            .is_err()
        {
            self.pending.lock().await.remove(&request_id);
            anyhow::bail!("capture writer closed");
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), rx).await;
        self.pending.lock().await.remove(&request_id);
        result
            .context("capture receipt timed out")?
            .context("capture connection closed before receipt")
    }
    async fn notify(&self, mut message: WireMessage) -> Result<()> {
        match &mut message {
            WireMessage::CapabilitySnapshot {
                connection_epoch, ..
            }
            | WireMessage::Request {
                connection_epoch, ..
            }
            | WireMessage::SessionControlState {
                connection_epoch, ..
            } => *connection_epoch = self.connection_epoch,
            _ => {}
        }
        self.sender
            .send(message)
            .await
            .map_err(|_| anyhow!("envagent writer closed"))
    }
}

#[derive(Debug, Clone)]
pub struct EnvAgentConfig {
    pub environment_id: String,
    pub producer_id: String,
    pub spool: SpoolConfig,
    pub path_policy: PathPolicy,
}

#[derive(Debug, thiserror::Error)]
#[error("vendor delivery is unconfirmed: {0}")]
struct UnconfirmedVendorOperation(anyhow::Error);
fn vendor_error(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<AdapterError>().is_some()
        || error.downcast_ref::<VendorRefusal>().is_some()
    {
        error
    } else {
        UnconfirmedVendorOperation(error).into()
    }
}

#[derive(Clone)]
pub struct HostedSession {
    session: Arc<Mutex<Box<dyn Session>>>,
    native: Arc<RwLock<Option<SessionId>>>,
    pub agent: String,
    pub workdir: PathBuf,
    authorization: CaptureAuthorization,
    lease: Arc<RwLock<Option<String>>>,
    native_turn: Arc<RwLock<Option<String>>>,
    requests: Arc<RwLock<BTreeMap<String, Request>>>,
    request_scope: Arc<requests::RequestScope>,
    closed: Arc<AtomicBool>,
}
impl HostedSession {
    pub async fn session_id(&self) -> Option<SessionId> {
        self.native.read().await.clone()
    }
    pub async fn capabilities(&self) -> Result<SessionCapabilities> {
        self.session.lock().await.capabilities().await
    }
    pub async fn close(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        self.session.lock().await.close().await
    }
    pub async fn send(&self, command_id: &CommandRequestId, input: TurnInput) -> Result<()> {
        self.session
            .lock()
            .await
            .send(command_id, input)
            .await
            .map_err(vendor_error)
    }
    pub async fn identity_evidence(&self) -> Vec<ObservationRef> {
        self.session.lock().await.identity_evidence().await
    }
}

#[derive(Clone)]
struct ProcessCapture {
    registry: CaptureRegistry,
    authorization: CaptureAuthorization,
    wake: Arc<Notify>,
}
#[async_trait]
impl RawCaptureSink for ProcessCapture {
    async fn capture(&self, capture: RawCapture) -> Result<ObservationRef> {
        let reference = self
            .registry
            .capture_frame(
                &capture.stream,
                capture.transport,
                capture.bytes,
                local_authorization(&self.authorization, capture.captured_at_ms),
            )
            .await?;
        self.wake.notify_one();
        Ok(reference)
    }
}
fn local_authorization(authorization: &CaptureAuthorization, at_ms: i64) -> LocalAuthorization {
    LocalAuthorization {
        epoch: authorization.epoch.clone(),
        subjects: match &authorization.scope {
            trace::IngestionScope::Agent(agent) => vec![agent.clone()],
            trace::IngestionScope::Inventory(_) => vec![],
        },
        captured_at: TimeEvidence {
            domain: format!("envagent:{}", authorization.scope.environment()),
            value: at_ms,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        },
        vendor_build: None,
    }
}

#[derive(Clone)]
pub struct EnvAgent {
    config: Arc<EnvAgentConfig>,
    environment: EnvironmentId,
    spool: Spool,
    registry: CaptureRegistry,
    sink: Arc<RwLock<Arc<dyn CaptureTransport>>>,
    send_gate: Arc<Mutex<()>>,
    source_state_gate: Arc<Mutex<()>>,
    wake: Arc<Notify>,
    sessions: Arc<RwLock<HashMap<SessionId, HostedSession>>>,
    prepared: Arc<RwLock<HashMap<String, HostedSession>>>,
    promoted: Arc<RwLock<HashMap<String, SessionId>>>,
    native_tasks: Arc<Mutex<HashMap<SessionId, tokio::task::JoinHandle<()>>>>,
    source_definitions: Arc<Mutex<std::collections::HashSet<String>>>,
    command_gates: Arc<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>,
    adapters: Arc<dyn AdapterSource>,
}
impl EnvAgent {
    pub async fn open(config: EnvAgentConfig, sink: Arc<dyn CaptureTransport>) -> Result<Self> {
        Self::open_with_adapters(config, sink, crate::adapters::linked()).await
    }
    pub async fn open_with_adapters(
        config: EnvAgentConfig,
        sink: Arc<dyn CaptureTransport>,
        adapters: Arc<dyn AdapterSource>,
    ) -> Result<Self> {
        let environment = EnvironmentId::parse(&config.environment_id)?;
        let spool = Spool::open(config.spool.clone()).await?;
        let registry = CaptureRegistry::open(
            spool.clone(),
            Issuer::Environment(environment.clone()),
            config.path_policy.clone(),
            CaptureLimits::default(),
        )
        .await?;
        let runtime = Self {
            config: Arc::new(config),
            environment,
            spool,
            registry,
            sink: Arc::new(RwLock::new(sink)),
            send_gate: Arc::new(Mutex::new(())),
            source_state_gate: Arc::new(Mutex::new(())),
            wake: Arc::new(Notify::new()),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            prepared: Arc::new(RwLock::new(HashMap::new())),
            promoted: Arc::new(RwLock::new(HashMap::new())),
            native_tasks: Arc::new(Mutex::new(HashMap::new())),
            source_definitions: Arc::new(Mutex::new(std::collections::HashSet::new())),
            command_gates: Arc::new(Mutex::new(HashMap::new())),
            adapters,
        };
        let delivery = runtime.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            interval.tick().await;
            loop {
                tokio::select! { _=delivery.wake.notified()=>{},_=interval.tick()=>{} }
                if let Err(error) = delivery.flush_snapshot().await {
                    tracing::debug!(%error,"capture delivery remains durably queued");
                }
            }
        });
        Ok(runtime)
    }
    pub fn spool(&self) -> &Spool {
        &self.spool
    }
    pub fn config(&self) -> &EnvAgentConfig {
        &self.config
    }
    pub async fn inventory(&self) -> Vec<AgentInventory> {
        crate::health::HealthReporter::with_adapters(self.adapters.clone())
            .inventory()
            .await
    }
    pub async fn set_sink(&self, sink: Arc<dyn CaptureTransport>) {
        *self.sink.write().await = sink;
        self.wake.notify_one();
        for session in self.sessions.read().await.values() {
            self.report_state(session).await;
            self.report_capabilities(session).await;
            for request in session.requests.read().await.values() {
                if let Some(session_id) = session.session_id().await {
                    let _ = self
                        .sink
                        .read()
                        .await
                        .notify(WireMessage::Request {
                            session_id,
                            request: request.clone(),
                            connection_epoch: 0,
                        })
                        .await;
                }
            }
        }
    }
    fn authorize_capture(&self, authorization: &CaptureAuthorization, agent: &str) -> Result<()> {
        authorization.scope.validate()?;
        anyhow::ensure!(
            authorization.scope.environment() == &self.environment
                && authorization.scope.vendor().as_str() == agent,
            "capture scope does not name this environment and vendor"
        );
        Ok(())
    }
    async fn local_input(
        &self,
        input: WireTurnInput,
        scope: &str,
        command: &str,
    ) -> Result<TurnInput> {
        let root = self
            .config
            .spool
            .path
            .parent()
            .context("spool lacks parent")?;
        Ok(TurnInput {
            text: input.text,
            attachments: stage_attachments(
                &attachments_dir(root),
                scope,
                command,
                &input.attachments,
            )
            .await?,
            command: input.command,
            args: input.args,
            config: input.config,
        })
    }
    async fn open_session(
        &self,
        agent: &str,
        mut spec: RunSpec,
        authorization: CaptureAuthorization,
    ) -> Result<HostedSession> {
        self.authorize_capture(&authorization, agent)?;
        spec.workdir = self
            .config
            .path_policy
            .contain(self.resolve_workdir(&spec.workdir)?)?
            .to_string_lossy()
            .into_owned();
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent is unavailable"))?;
        let capture = Arc::new(ProcessCapture {
            registry: self.registry.clone(),
            authorization: authorization.clone(),
            wake: self.wake.clone(),
        });
        let mut session = adapter
            .run(spec.clone(), capture)
            .await
            .map_err(vendor_error)?;
        let stream = session.messages();
        let native = session.native_id().map(|native| {
            SessionId::new(
                self.environment.clone(),
                AgentName::parse(agent).unwrap(),
                native,
            )
        });
        let hosted = HostedSession {
            session: Arc::new(Mutex::new(session)),
            native: Arc::new(RwLock::new(native)),
            agent: agent.into(),
            workdir: PathBuf::from(spec.workdir),
            authorization,
            lease: Arc::new(RwLock::new(None)),
            native_turn: Arc::new(RwLock::new(None)),
            requests: Arc::new(RwLock::new(BTreeMap::new())),
            request_scope: Arc::new(requests::RequestScope::default()),
            closed: Arc::new(AtomicBool::new(false)),
        };
        if let Some(stream) = stream {
            self.pump(stream, hosted.clone());
        }
        Ok(hosted)
    }
    async fn new_native(&self, agent: &str) -> Result<RequestedNativeId> {
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow!("agent unavailable"))?;
        Ok(if adapter.probe().await?.dictates_native_id {
            RequestedNativeId::Dictate(NativeId::new(uuid::Uuid::new_v4().to_string()))
        } else {
            RequestedNativeId::New
        })
    }
    fn pump(&self, mut stream: ControlStream, session: HostedSession) {
        let runtime = self.clone();
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Ok(SessionMessage::Request(request)) => {
                        let request = session.request_scope.publish(request);
                        session
                            .requests
                            .write()
                            .await
                            .insert(request.id.clone(), request.clone());
                        if let Some(session_id) = session.session_id().await {
                            let _ = runtime
                                .sink
                                .read()
                                .await
                                .notify(WireMessage::Request {
                                    session_id,
                                    request,
                                    connection_epoch: 0,
                                })
                                .await;
                        }
                    }
                    Ok(SessionMessage::Capabilities(_)) => {
                        runtime.report_capabilities(&session).await
                    }
                    Ok(SessionMessage::Control(ControlNotice::Identity { native_id, .. })) => {
                        *session.native.write().await = Some(SessionId::new(
                            runtime.environment.clone(),
                            AgentName::parse(&session.agent).unwrap(),
                            native_id,
                        ));
                    }
                    Ok(SessionMessage::Control(ControlNotice::Active {
                        native_turn_id, ..
                    })) => {
                        *session.native_turn.write().await = native_turn_id;
                        runtime.report_state(&session).await;
                    }
                    Ok(SessionMessage::Control(ControlNotice::Settled {
                        native_turn_id, ..
                    })) => {
                        let current = session.native_turn.read().await.clone();
                        if native_turn_id.is_none() || current == native_turn_id {
                            *session.lease.write().await = None;
                            *session.native_turn.write().await = None;
                            runtime.report_state(&session).await;
                        }
                    }
                    Err(error) => tracing::warn!(%error,"vendor control stream diagnostic"),
                }
            }
            session.closed.store(true, Ordering::Release);
            *session.lease.write().await = None;
            *session.native_turn.write().await = None;
            runtime.report_state(&session).await;
            runtime.report_capabilities(&session).await;
        });
    }
    async fn report_state(&self, session: &HostedSession) {
        if let Some(session_id) = session.session_id().await {
            let lease = session.lease.read().await.clone();
            let native_turn_id = session.native_turn.read().await.clone();
            let _ = self
                .sink
                .read()
                .await
                .notify(WireMessage::SessionControlState {
                    session_id,
                    active: lease.is_some() || native_turn_id.is_some(),
                    lease,
                    native_turn_id,
                    connection_epoch: 0,
                })
                .await;
        }
    }
    async fn report_capabilities(&self, session: &HostedSession) {
        if let Some(session_id) = session.session_id().await {
            if session.closed.load(Ordering::Acquire) {
                let _ = self
                    .sink
                    .read()
                    .await
                    .notify(WireMessage::CapabilitySnapshot {
                        session_id,
                        facets: CapabilityFacets::default(),
                        link: Some(LinkFacets {
                            observe: Facet::available(),
                            control: Facet::unavailable(UnavailableReason::HandshakeFailed),
                        }),
                        capabilities: None,
                        connection_epoch: 0,
                    })
                    .await;
                return;
            }
            if let Ok(capabilities) = session.capabilities().await {
                let _ = self
                    .sink
                    .read()
                    .await
                    .notify(WireMessage::CapabilitySnapshot {
                        session_id,
                        facets: CapabilityFacets {
                            commands: Some(capabilities.commands.clone()),
                            subagents: Some(capabilities.subagents.clone()),
                            config: Some(capabilities.config.clone()),
                            permission_mode: capabilities.permission_mode.clone(),
                            context_window: capabilities.context_window,
                        },
                        link: None,
                        capabilities: Some(capabilities),
                        connection_epoch: 0,
                    })
                    .await;
            }
        }
    }
    async fn capability_result(&self, session: &HostedSession) -> Result<serde_json::Value> {
        if session.closed.load(Ordering::Acquire) {
            return Ok(
                serde_json::json!({"workdir":session.workdir,"capabilities":SessionCapabilities::default(),"supports":[],"link":LinkFacets {observe:Facet::available(),control:Facet::unavailable(UnavailableReason::HandshakeFailed)}}),
            );
        }
        let capabilities = session.capabilities().await?;
        let supports = self
            .adapters
            .for_agent(&session.agent)
            .ok_or_else(|| anyhow!("agent unavailable"))?
            .probe()
            .await?
            .supports;
        Ok(
            serde_json::json!({"workdir":session.workdir,"capabilities":capabilities,"supports":supports,"link":{"observe":{"available":true},"control":{"available":true}}}),
        )
    }
    async fn hosted(&self, id: &SessionId) -> Result<HostedSession> {
        self.sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("session is not hosted"))
    }
    pub async fn execute_command(
        &self,
        request_id: CommandRequestId,
        policy: IdempotencyPolicy,
        command: EnvironmentCommand,
    ) -> CommandOutcome {
        // Read commands have fresh request ids and no durable effects. Their response
        // bodies must not accumulate in the operational deduplication ledger.
        if policy == IdempotencyPolicy::Replayable {
            return self.handle_command(&request_id, command).await;
        }
        let gate = {
            let mut gates = self.command_gates.lock().await;
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates
                .get(request_id.as_str())
                .and_then(std::sync::Weak::upgrade)
            {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(Mutex::new(()));
                    gates.insert(request_id.to_string(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        let _held = gate.lock().await;
        match self
            .spool
            .begin_command(&request_id, policy, &command)
            .await
        {
            Ok(Some(outcome)) => return outcome,
            Ok(None) => {}
            Err(error) => {
                return CommandOutcome::Unknown {
                    message: Some(format!("command could not be durably reserved: {error}")),
                }
            }
        }
        let outcome = self.handle_command(&request_id, command).await;
        if let Err(error) = self.spool.finish_command(&request_id, &outcome).await {
            return CommandOutcome::Unknown {
                message: Some(format!(
                    "vendor operation ended but its receipt could not be retained: {error}"
                )),
            };
        }
        outcome
    }

    async fn handle_command(
        &self,
        request_id: &CommandRequestId,
        command: EnvironmentCommand,
    ) -> CommandOutcome {
        match self.command(request_id, command).await {
            Ok(value) => CommandOutcome::accepted_with_result(value),
            Err(error) => {
                if let Some(adapter) = error.downcast_ref::<AdapterError>() {
                    return CommandOutcome::Rejected {
                        code: match adapter.code {
                            RefusalCode::UnsupportedField => "UNSUPPORTED_FIELD",
                            RefusalCode::InvalidValue => "INVALID_VALUE",
                        }
                        .into(),
                        message: adapter.to_string(),
                    };
                }
                if error.downcast_ref::<VendorRefusal>().is_some() {
                    return CommandOutcome::Rejected {
                        code: "VENDOR_REFUSED".into(),
                        message: error.to_string(),
                    };
                }
                if error.downcast_ref::<UnconfirmedVendorOperation>().is_some() {
                    return CommandOutcome::Unknown {
                        message: Some(error.to_string()),
                    };
                }
                CommandOutcome::Rejected {
                    code: "ENVIRONMENT_COMMAND_FAILED".into(),
                    message: format!("{error:#}"),
                }
            }
        }
    }
    async fn command(
        &self,
        request_id: &CommandRequestId,
        command: EnvironmentCommand,
    ) -> Result<serde_json::Value> {
        let follow_native = matches!(&command, EnvironmentCommand::Watch { .. });
        match command {
            EnvironmentCommand::Prepare {
                capture_authorization,
                draft_id,
                agent,
                workdir,
                flags,
                inject_identity,
                identity_preamble,
                permission_mode,
            } => {
                if let Some(prepared) = self.prepared.read().await.get(&draft_id).cloned() {
                    return self.capability_result(&prepared).await;
                }
                let native_id = self.new_native(&agent).await?;
                let spec = RunSpec {
                    workdir,
                    native_id,
                    flags,
                    inject_identity,
                    identity_preamble,
                    permission_mode,
                };
                let session = self
                    .open_session(&agent, spec, capture_authorization)
                    .await?;
                let result = self.capability_result(&session).await?;
                self.prepared.write().await.insert(draft_id, session);
                Ok(result)
            }
            EnvironmentCommand::Run {
                capture_authorization,
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
                self.authorize_capture(&capture_authorization, &agent)?;
                if let Some(draft) = &draft_id {
                    if let Some(id) = self.promoted.read().await.get(draft).cloned() {
                        let session = self.hosted(&id).await?;
                        return Ok(
                            serde_json::json!({"sessionId":id,"workdir":session.workdir,"sourceEvidence":session.identity_evidence().await,"fidelity":"stream"}),
                        );
                    }
                }
                let prepared = if let Some(draft) = &draft_id {
                    self.prepared.read().await.get(draft).cloned()
                } else {
                    None
                };
                let session = if let Some(prepared) = prepared {
                    anyhow::ensure!(
                        prepared.authorization == capture_authorization,
                        "prepared capture authorization changed"
                    );
                    prepared
                } else {
                    let native_id = match &session_id {
                        Some(session) => RequestedNativeId::Resume(session.native_id.clone()),
                        None => self.new_native(&agent).await?,
                    };
                    self.open_session(
                        &agent,
                        RunSpec {
                            workdir,
                            native_id,
                            flags,
                            inject_identity,
                            identity_preamble,
                            permission_mode,
                        },
                        capture_authorization,
                    )
                    .await?
                };
                if let Some(input) = initial_input {
                    let lease = initial_turn_id
                        .clone()
                        .context("initial input lacks command lease")?;
                    let input = self
                        .local_input(input, draft_id.as_deref().unwrap_or(&agent), &lease)
                        .await?;
                    *session.lease.write().await = Some(lease.clone());
                    let native = session
                        .session
                        .lock()
                        .await
                        .send_initial(request_id, input)
                        .await
                        .map_err(vendor_error)?;
                    *session.native.write().await = Some(SessionId::new(
                        self.environment.clone(),
                        AgentName::parse(&agent)?,
                        native,
                    ));
                }
                let identity = session
                    .session_id()
                    .await
                    .context("vendor did not report native identity")
                    .map_err(vendor_error)?;
                let evidence = session.identity_evidence().await;
                if evidence.is_empty() {
                    return Err(vendor_error(anyhow!(
                        "vendor native identity has no retained source evidence"
                    )));
                }
                self.flush_snapshot().await.map_err(vendor_error)?;
                self.sessions
                    .write()
                    .await
                    .insert(identity.clone(), session.clone());
                if let Some(draft) = draft_id {
                    self.prepared.write().await.remove(&draft);
                    self.promoted.write().await.insert(draft, identity.clone());
                }
                self.start_native(
                    identity.clone(),
                    agent,
                    session.authorization.clone(),
                    false,
                )
                .await?;
                self.report_state(&session).await;
                Ok(
                    serde_json::json!({"sessionId":identity,"workdir":session.workdir,"sourceEvidence":evidence,"fidelity":"stream"}),
                )
            }
            EnvironmentCommand::Send {
                session_id,
                turn_id,
                input,
            } => {
                let session = self.hosted(&session_id).await?;
                anyhow::ensure!(
                    session.lease.read().await.is_none(),
                    "another command is active"
                );
                let input = self
                    .local_input(input, &session_id.encode(), &turn_id)
                    .await?;
                *session.lease.write().await = Some(turn_id.clone());
                if let Err(error) = session.send(request_id, input).await {
                    if error.downcast_ref::<UnconfirmedVendorOperation>().is_none() {
                        *session.lease.write().await = None;
                    }
                    return Err(error);
                }
                if session.lease.read().await.as_deref() == Some(&turn_id) {
                    *session.native_turn.write().await =
                        session.session.lock().await.last_sent_turn_id().await;
                }
                self.report_state(&session).await;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Interrupt {
                session_id,
                expected_turn_id,
            } => {
                let session = self.hosted(&session_id).await?;
                self.check_lease(&session, expected_turn_id.as_deref())
                    .await?;
                session
                    .session
                    .lock()
                    .await
                    .interrupt()
                    .await
                    .map_err(vendor_error)?;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Steer {
                session_id,
                text,
                expected_turn_id,
            } => {
                let session = self.hosted(&session_id).await?;
                self.check_lease(&session, expected_turn_id.as_deref())
                    .await?;
                let turn = session.native_turn.read().await.clone();
                session
                    .session
                    .lock()
                    .await
                    .steer(request_id, &text, turn.as_deref())
                    .await
                    .map_err(vendor_error)?;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::QueueSteering { session_id, text } => {
                self.hosted(&session_id)
                    .await?
                    .session
                    .lock()
                    .await
                    .queue_steering(request_id, &text)
                    .await
                    .map_err(vendor_error)?;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Queue { session_id, text } => {
                self.hosted(&session_id)
                    .await?
                    .session
                    .lock()
                    .await
                    .queue(request_id, &text)
                    .await
                    .map_err(vendor_error)?;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::SetConfig {
                session_id,
                id,
                value,
            } => {
                let session = self.hosted(&session_id).await?;
                session
                    .session
                    .lock()
                    .await
                    .set_config(&id, value)
                    .await
                    .map_err(vendor_error)?;
                self.capability_result(&session).await.map_err(vendor_error)
            }
            EnvironmentCommand::Resolve {
                session_id,
                resolution,
            } => {
                let session = self.hosted(&session_id).await?;
                let resolution: Resolution = serde_json::from_value(resolution)?;
                let public_request_id = resolution.request_id.clone();
                let resolution = session.request_scope.resolve(resolution)?;
                session
                    .session
                    .lock()
                    .await
                    .resolve(resolution.clone())
                    .await
                    .map_err(vendor_error)?;
                session.requests.write().await.remove(&public_request_id);
                session.request_scope.retire(&public_request_id);
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Capabilities { session_id } => {
                self.capability_result(&self.hosted(&session_id).await?)
                    .await
            }
            EnvironmentCommand::DraftCapabilities { draft_id } => {
                self.capability_result(
                    &self
                        .prepared
                        .read()
                        .await
                        .get(&draft_id)
                        .cloned()
                        .context("draft unavailable")?,
                )
                .await
            }
            EnvironmentCommand::DraftSetConfig {
                draft_id,
                id,
                value,
            } => {
                let session = self
                    .prepared
                    .read()
                    .await
                    .get(&draft_id)
                    .cloned()
                    .context("draft unavailable")?;
                session
                    .session
                    .lock()
                    .await
                    .set_config(&id, value)
                    .await
                    .map_err(vendor_error)?;
                self.capability_result(&session).await.map_err(vendor_error)
            }
            EnvironmentCommand::DraftClose { draft_id } => {
                if let Some(session) = self.prepared.write().await.remove(&draft_id) {
                    session.close().await?;
                }
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Close { session_id } => {
                if let Some(id) = session_id {
                    self.close(&id).await?;
                } else {
                    let sessions: Vec<_> = self.sessions.read().await.keys().cloned().collect();
                    for id in sessions {
                        self.close(&id).await?;
                    }
                }
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Watch {
                capture_authorization,
                session_id,
                agent,
                native_id,
                ..
            }
            | EnvironmentCommand::Adopt {
                capture_authorization,
                session_id,
                agent,
                native_id,
                ..
            } => {
                anyhow::ensure!(
                    session_id.native_id == native_id,
                    "native identity mismatch"
                );
                let adapter = self
                    .adapters
                    .for_agent(&agent)
                    .context("agent unavailable")?;
                let summary = adapter
                    .lookup(&native_id)
                    .await?
                    .context("native source unavailable")?;
                let workdir = summary
                    .workdir
                    .clone()
                    .context("native source has no workdir")?;
                self.config.path_policy.contain(&workdir)?;
                if follow_native {
                    self.start_native(session_id.clone(), agent, capture_authorization, true)
                        .await?;
                } else {
                    self.capture_native(&session_id, &agent, &capture_authorization, false)
                        .await?;
                    self.flush_snapshot().await?;
                }
                Ok(
                    serde_json::json!({"sessionId":session_id,"workdir":workdir,"summary":summary,"fidelity":summary.fidelity}),
                )
            }
            EnvironmentCommand::Reread {
                capture_authorization,
                session_id,
                agent,
                native_id,
            } => {
                anyhow::ensure!(
                    session_id.native_id == native_id,
                    "native reread identity mismatch"
                );
                let following = self.native_tasks.lock().await.contains_key(&session_id);
                self.capture_native(&session_id, &agent, &capture_authorization, following)
                    .await?;
                self.flush_snapshot().await?;
                Ok(serde_json::json!({}))
            }
            EnvironmentCommand::Enumerate { agent, workdir } => {
                let adapter = self
                    .adapters
                    .for_agent(&agent)
                    .context("agent unavailable")?;
                Ok(serde_json::to_value(
                    adapter
                        .enumerate(&StoreFilter {
                            workdir,
                            ..StoreFilter::default()
                        })
                        .await?,
                )?)
            }
            EnvironmentCommand::Files {
                session_id: _,
                workdir,
                operation,
            } => Ok(command_value(
                self.file_command_at_root(Path::new(&workdir), operation)
                    .await,
            )?),
            EnvironmentCommand::DraftFiles {
                draft_id,
                operation,
            } => {
                let session = self
                    .prepared
                    .read()
                    .await
                    .get(&draft_id)
                    .cloned()
                    .context("draft unavailable")?;
                Ok(command_value(
                    self.file_command_at_root(&session.workdir, operation).await,
                )?)
            }
            EnvironmentCommand::Health => Ok(serde_json::to_value(
                crate::health::HealthReporter::new()
                    .snapshot(self.environment.to_string())
                    .await,
            )?),
            EnvironmentCommand::BrowseRoots => Ok(self.browse_roots()),
            EnvironmentCommand::BrowseTree { path } => self.browse_tree(path),
        }
    }
    async fn check_lease(&self, session: &HostedSession, expected: Option<&str>) -> Result<()> {
        let lease = session.lease.read().await;
        anyhow::ensure!(
            expected.is_none_or(|expected| lease.as_deref() == Some(expected)),
            "active command lease changed"
        );
        Ok(())
    }
    async fn close(&self, id: &SessionId) -> Result<()> {
        if let Some(task) = self.native_tasks.lock().await.remove(id) {
            task.abort();
            let _ = task.await;
        }
        for (mut event, mut authorization) in self.spool.source_states(id).await? {
            if event.following {
                event.following = false;
                authorization.captured_at.value = sylph_adapters::now_ms();
                self.record_source_state(id, event, authorization).await?;
            }
        }
        if let Some(session) = self.sessions.write().await.remove(id) {
            session.close().await?;
        }
        Ok(())
    }
    async fn start_native(
        &self,
        id: SessionId,
        agent: String,
        authorization: CaptureAuthorization,
        require_available: bool,
    ) -> Result<()> {
        self.authorize_capture(&authorization, &agent)?;
        if require_available {
            self.capture_native(&id, &agent, &authorization, true)
                .await?;
        }
        let mut tasks = self.native_tasks.lock().await;
        if tasks.contains_key(&id) {
            return Ok(());
        }
        let runtime = self.clone();
        let key = id.clone();
        let task = tokio::spawn(async move {
            let mut failed = false;
            loop {
                match runtime
                    .capture_native(&id, &agent, &authorization, true)
                    .await
                {
                    Ok(()) => failed = false,
                    Err(error) => {
                        if !failed {
                            tracing::debug!(%error,"native source not yet available");
                        }
                        failed = true;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
        tasks.insert(key, task);
        Ok(())
    }
    async fn capture_native(
        &self,
        id: &SessionId,
        agent: &str,
        authorization: &CaptureAuthorization,
        following: bool,
    ) -> Result<()> {
        self.authorize_capture(authorization, agent)?;
        anyhow::ensure!(
            id.environment == self.environment && id.agent.as_str() == agent,
            "native acquisition identity does not match this environment and vendor"
        );
        let adapter = self
            .adapters
            .for_agent(agent)
            .context("agent unavailable")?;
        match adapter.native_sources(&id.native_id).await {
            Ok(discovered) => {
                self.spool
                    .remember_native_sources(id, &authorization.epoch, &discovered)
                    .await?
            }
            Err(error) => {
                if self
                    .spool
                    .native_sources(id, &authorization.epoch)
                    .await?
                    .is_empty()
                {
                    return Err(error);
                }
            }
        }
        let mut sources = self.spool.native_sources(id, &authorization.epoch).await?;
        let mut databases = Vec::new();
        for source in sources
            .iter()
            .filter(|source| matches!(source.format,NativeSourceFormat::CodexThreadMetadata|NativeSourceFormat::CodexGoalMetadata))
        {
            let path = self
                .config
                .path_policy
                .contain_write_target(&source.locator)?;
            let (snapshot, identity) = match if source.format==NativeSourceFormat::CodexGoalMetadata {database::read_goal(&path,id.native_id.as_str()).await} else {database::read(&path,id.native_id.as_str()).await} {
                Ok(value) => value,
                Err(error) => (
                    NativeDatabaseSnapshot {
                        database_locator: source.locator.clone(),
                        table: "threads".into(),
                        columns: vec![],
                        status: NativeDatabaseStatus::Unreadable {
                            reason: format!("{error:#}"),
                        },
                        row: BTreeMap::new(),
                        selected_source: None,
                        selected_generation: None,
                        selected_record_count: None,
                        selected_pending_bytes: None,
                    },
                    "unreadable".into(),
                ),
            };
            databases.push((source.clone(), snapshot, identity, sylph_adapters::now_ms()));
        }
        for (_, snapshot, _, _) in &databases {
            if let Some(NativeDatabaseValue::Text(path)) = snapshot.row.get("rollout_path") {
                if !sources.iter().any(|source| source.locator == *path)
                    && self.config.path_policy.contain_write_target(path).is_ok()
                {
                    sources.push(NativeSource {
                        locator: path.clone(),
                        namespace: "codex.native".into(),
                        format: NativeSourceFormat::Jsonl,
                        companion_locator: None,
                        owner: None,
                        identity_record_ordinal: None,
                    });
                }
            }
        }
        let mut captured = BTreeMap::new();
        for source in &sources {
            if matches!(source.format,NativeSourceFormat::CodexThreadMetadata|NativeSourceFormat::CodexGoalMetadata) {
                continue;
            }
            let mut registered = self
                .registry
                .register_jsonl(&source.namespace, &source.locator)
                .await?;
            registered.identity_record_ordinal = source.identity_record_ordinal;
            let result = async {
                let pass = match source.format {
                    NativeSourceFormat::CodexThreadMetadata|NativeSourceFormat::CodexGoalMetadata => {
                        unreachable!("database sources are acquired separately")
                    }
                    NativeSourceFormat::Jsonl => {
                        let mut pass = self
                            .registry
                            .capture_jsonl(
                                &registered,
                                local_authorization(authorization, sylph_adapters::now_ms()),
                            )
                            .await?;
                        while !pass.observations.is_empty() {
                            self.wake.notify_one();
                            pass = self
                                .registry
                                .capture_jsonl(
                                    &registered,
                                    local_authorization(authorization, sylph_adapters::now_ms()),
                                )
                                .await?;
                        }
                        pass
                    }
                    NativeSourceFormat::Snapshot | NativeSourceFormat::Bytes => {
                        self.registry
                            .capture_snapshot_as(
                                &registered,
                                local_authorization(authorization, sylph_adapters::now_ms()),
                                if source.format == NativeSourceFormat::Bytes {
                                    trace::TransportKind::PayloadBytes
                                } else {
                                    trace::TransportKind::Sidecar
                                },
                            )
                            .await?
                    }
                };
                Ok::<_, crate::spool::capture::CaptureSpoolError>(pass)
            }
            .await;
            let (generation, complete_records, complete_bytes, pending_bytes, missing) = self
                .registry
                .source_extent(&registered, &authorization.epoch)
                .await?;
            let state = match result {
                Ok(_) if missing => NativeSourceState::Missing,
                Ok(_) => NativeSourceState::Present,
                Err(error) => NativeSourceState::Unreadable {
                    reason: error.to_string(),
                },
            };
            self.record_source_state(
                id,
                SourceStateEvent {
                    source: registered.reference.clone(),
                    generation,
                    complete_records,
                    complete_bytes,
                    pending_bytes,
                    following,
                    state,
                },
                local_authorization(authorization, sylph_adapters::now_ms()),
            )
            .await?;
            captured.insert(source.locator.clone(), (registered, generation));
            self.wake.notify_one();
        }
        for (source, mut snapshot, identity, captured_at) in databases {
            if let Some(NativeDatabaseValue::Text(path)) = snapshot.row.get("rollout_path") {
                if let Some((registered, generation)) = captured.get(path) {
                    snapshot.selected_source = Some(registered.reference.clone());
                    snapshot.selected_generation = Some(*generation);
                    let (current, count, _, pending, missing) = self
                        .registry
                        .source_extent(registered, &authorization.epoch)
                        .await?;
                    if current == *generation && !missing {
                        snapshot.selected_record_count = Some(count);
                        snapshot.selected_pending_bytes = Some(pending);
                    }
                }
            }
            let registered = self
                .registry
                .register_jsonl(&source.namespace, &source.locator)
                .await?;
            self.registry
                .capture_database_snapshot(
                    &registered,
                    local_authorization(authorization, captured_at),
                    serde_json::to_vec(&snapshot)?,
                    identity,
                )
                .await?;
            self.wake.notify_one();
        }
        for source in &sources {
            if matches!(source.format,NativeSourceFormat::CodexThreadMetadata|NativeSourceFormat::CodexGoalMetadata) {
                continue;
            }
            let Some(owner) = &source.owner else {
                continue;
            };
            let owner = match owner {
                NativeOwner::Session { native_id } => trace::AgentRef::Session(SessionId::new(
                    self.environment.clone(),
                    AgentName::parse(agent)?,
                    native_id.clone(),
                )),
                NativeOwner::Embedded {
                    anchor_native_id,
                    namespace,
                    native_child_id,
                } => trace::AgentRef::Embedded {
                    anchor_session: SessionId::new(
                        self.environment.clone(),
                        AgentName::parse(agent)?,
                        anchor_native_id.clone(),
                    ),
                    namespace: namespace.clone(),
                    native_child_id: native_child_id.clone(),
                },
            };
            let (registered, generation) = &captured[&source.locator];
            let basis = if let Some(companion) = &source.companion_locator {
                let Some((parent, parent_generation)) = captured.get(companion) else {
                    continue;
                };
                trace::SourceOwnerBasis::CompanionTranscript {
                    source: parent.reference.clone(),
                    generation: *parent_generation,
                }
            } else {
                let Some(observation) = self
                    .registry
                    .source_proof(&registered.reference, *generation, &authorization.epoch)
                    .await?
                else {
                    continue;
                };
                trace::SourceOwnerBasis::NativeIdentity { observation }
            };
            let definition = trace::SourceDefinition {
                source: registered.reference.clone(),
                generation: *generation,
                locator: source.locator.clone(),
                owner,
                basis,
            };
            let key = serde_json::to_string(&(&authorization.epoch, &definition))?;
            let mut seen = self.source_definitions.lock().await;
            if seen.contains(&key) {
                continue;
            }
            self.registry
                .capture_frame(
                    &format!("inventory:{}", authorization.epoch.as_str()),
                    trace::TransportKind::SourceInventory,
                    serde_json::to_vec(&definition)?,
                    local_authorization(authorization, sylph_adapters::now_ms()),
                )
                .await?;
            seen.insert(key);
            self.wake.notify_one();
        }
        Ok(())
    }

    async fn record_source_state(
        &self,
        session: &SessionId,
        event: SourceStateEvent,
        authorization: LocalAuthorization,
    ) -> Result<()> {
        let _held = self.source_state_gate.lock().await;
        if self
            .spool
            .source_state_matches(session, &event, &authorization.epoch)
            .await?
        {
            return Ok(());
        }
        self.registry
            .capture_frame(
                "source-state",
                trace::TransportKind::SourceState,
                serde_json::to_vec(&event)?,
                authorization.clone(),
            )
            .await?;
        self.spool
            .remember_source_state(session, &event, &authorization)
            .await?;
        self.wake.notify_one();
        Ok(())
    }

    pub async fn flush_snapshot(&self) -> Result<()> {
        // Freeze closed-generation jobs as well as each producer prefix. A finite
        // import waits for its already-observed tails; ongoing acquisition cannot
        // keep extending this barrier with later generation closures.
        let tails = self.registry.closed_tail_frontier().await?;
        loop {
            self.registry.drain_closed_tails(tails).await?;
            for _ in 0..2 {
                let through = self.registry.pending_frontiers().await?;
                while self.flush_pending_through(32, Some(&through)).await? != 0 {}
            }
            if !self.registry.closed_tails_pending(tails).await? { break; }
        }
        Ok(())
    }
    pub async fn flush_pending(&self, maximum: usize) -> Result<usize> {
        self.flush_pending_through(maximum, None).await
    }
    async fn flush_pending_through(
        &self,
        maximum: usize,
        through: Option<&[ObservationRef]>,
    ) -> Result<usize> {
        let _held = self.send_gate.lock().await;
        self.refresh_attachments().await?;
        let captures = self
            .registry
            .pending_metadata_through(maximum, through)
            .await?;
        let sink = self.sink.read().await.clone();
        let mut count = 0;
        for pending in captures {
            if self.discover_attachments(&pending).await? {
                self.refresh_attachments().await?;
            }
            let mut declarations = Vec::new();
            if let Some(manifest) = &pending.manifest {
                declarations.push(PayloadDeclaration::Manifest {
                    reference: pending.capture.payload_ref.clone(),
                    manifest: manifest.clone(),
                });
                for chunk in &manifest.chunks {
                    declarations.push(PayloadDeclaration::Bytes {
                        reference: chunk.digest.clone(),
                        length: chunk.length,
                    });
                }
            } else {
                let (_, bytes) = self
                    .registry
                    .queued_chunk(&pending.capture.observation_ref, 0)
                    .await?
                    .context("capture chunk absent")?;
                declarations.push(PayloadDeclaration::Bytes {
                    reference: pending.capture.payload_ref.clone(),
                    length: bytes.len() as u64,
                });
            }
            let reply = sink
                .upload(CaptureUpload::Begin {
                    capture: pending.capture.clone(),
                    declarations,
                })
                .await?;
            let receipt = match reply {
                CaptureReply::Pending { ticket } => {
                    for ordinal in 0..pending.chunk_count {
                        let (reference, bytes) = self
                            .registry
                            .queued_chunk(&pending.capture.observation_ref, ordinal)
                            .await?
                            .context("capture bytes missing")?;
                        match sink
                            .upload(CaptureUpload::Chunk {
                                ticket: ticket.clone(),
                                reference,
                                bytes,
                            })
                            .await?
                        {
                            CaptureReply::ChunkStored => {}
                            CaptureReply::Rejected { code, message } => {
                                anyhow::bail!("{code}: {message}")
                            }
                            other => anyhow::bail!("unexpected chunk receipt {other:?}"),
                        }
                    }
                    sink.upload(CaptureUpload::Commit { ticket }).await?
                }
                receipt => receipt,
            };
            match receipt {
                CaptureReply::Admitted { receipts, .. } => {
                    let receipt = receipts
                        .into_iter()
                        .find(|receipt| receipt.observation == pending.capture.observation_ref)
                        .context("capture receipt did not name offered observation")?;
                    let local = match receipt.disposition {
                        CaptureDisposition::Retained { admission_seq } => LocalReceipt::Retained {
                            observation: receipt.observation,
                            admission_seq,
                        },
                        CaptureDisposition::DiscardedByDeletion { tombstone_id } => {
                            LocalReceipt::DiscardedByDeletion {
                                observation: receipt.observation,
                                tombstone: tombstone_id,
                            }
                        }
                    };
                    self.registry.acknowledge(&[local]).await?;
                    count += 1;
                }
                CaptureReply::Rejected { code, message } => anyhow::bail!("{code}: {message}"),
                other => anyhow::bail!("unexpected capture receipt {other:?}"),
            }
        }
        Ok(count)
    }
}
fn command_value(outcome: CommandOutcome) -> Result<serde_json::Value> {
    match outcome {
        CommandOutcome::Accepted { result, .. } => Ok(result.unwrap_or_default()),
        other => Err(anyhow!("file command refused: {other:?}")),
    }
}

impl EnvAgent {
    async fn file_command_at_root(
        &self,
        root: &std::path::Path,
        operation: sylph_protocol::FileOperation,
    ) -> sylph_protocol::CommandOutcome {
        let result: Result<serde_json::Value> = (|| {
            // The server supplies the durable root. Browse roots govern new sessions;
            // existing file operations stay bounded by this directory after a restart
            // or a narrowed browse policy, without querying a vendor's native store.
            let policy = PathPolicy::from_roots([root.to_path_buf()])?;
            let root = &policy.roots()[0];
            let policy = &policy;
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
}

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

#[cfg(test)]
mod tests;
