//! Outward envagent transport, current capability state and durable capture admission.
use super::registry::{EnvironmentRegistry, EnvironmentSnapshot, PairingGrant, PairingOffer};
use crate::{db::ControlStore, models::now_ms, trace::service::TraceService};
use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use sylph_protocol::CaptureAuthorization;
use sylph_protocol::trace::{AgentRef, IngestionScope, InventoryRef};
use sylph_protocol::{
    Command, CommandOutcome, DecodedMessage, ENV_PROTOCOL, EnvironmentId, IdempotencyPolicy,
    WireMessage, negotiate,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, RwLock, mpsc},
};
use tokio_rustls::TlsAcceptor;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Sent,
    AlreadyAccepted,
    AlreadyRejected,
    UnknownNotRetried,
}
#[derive(Debug, thiserror::Error)]
pub enum RemoteCommandError {
    #[error("remote command rejected ({code}): {message}")]
    Rejected { code: String, message: String },
    #[error("remote command outcome is unknown: {message}")]
    Unknown { message: String },
}

impl RemoteCommandError {
    pub fn rejected(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Rejected {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Rejected { code, .. } => Some(code),
            Self::Unknown { .. } => None,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Rejected { message, .. } | Self::Unknown { message } => message,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("unsupported envagent protocol '{0}'")]
    UnsupportedProtocol(String),
    #[error("no compatible envagent protocol version")]
    VersionMismatch,
    #[error("environment is not paired")]
    Unpaired,
    #[error("envagent credential is invalid")]
    InvalidCredential,
    #[error("connection epoch is stale")]
    StaleEpoch,
    #[error("producer id does not match the connection hello")]
    ProducerMismatch,
    #[error("event batch is invalid: {0}")]
    InvalidBatch(String),
    #[error("command has an unknown outcome and is not safe to retry")]
    AmbiguousCommand,
    #[error("connection is not currently available")]
    NotConnected,
}

struct Lease {
    epoch: u64,
    sender: mpsc::Sender<WireMessage>,
}

#[derive(Debug, Clone)]
pub struct RemoteDraft {
    pub draft_id: String,
    pub creator_subject: String,
    pub environment: EnvironmentId,
    pub session_id: Option<sylph_protocol::SessionId>,
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    pub inject_identity: bool,
    pub flags: HashMap<String, String>,
    pub identity_preamble: Option<String>,
    pub permission_mode: Option<String>,
    pub capture_authorization: CaptureAuthorization,
    pub promoting: bool,
    pub first_command_id: Option<String>,
}
struct CommandWaiter {
    id: u64,
    sender: tokio::sync::oneshot::Sender<CommandOutcome>,
}

type CommandWaiters = Arc<Mutex<HashMap<(EnvironmentId, String), Vec<CommandWaiter>>>>;

const EPHEMERAL_READ_PREFIX: &str = "read-";
type ReadWaiters = Arc<
    std::sync::Mutex<
        HashMap<(EnvironmentId, String), tokio::sync::oneshot::Sender<CommandOutcome>>,
    >,
>;

struct ReadRegistration {
    key: (EnvironmentId, String),
    waiters: ReadWaiters,
}

impl Drop for ReadRegistration {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("read waiter mutex poisoned")
            .remove(&self.key);
    }
}

#[derive(Clone)]
pub struct EnvBridge {
    pub registry: EnvironmentRegistry,
    store: ControlStore,
    trace: Arc<TraceService>,
    leases: Arc<RwLock<HashMap<EnvironmentId, Lease>>>,
    command_policies: Arc<RwLock<HashMap<(EnvironmentId, String), IdempotencyPolicy>>>,
    command_waiters: CommandWaiters,
    read_waiters: ReadWaiters,
    next_waiter_id: Arc<AtomicU64>,
    remote_drafts: Arc<Mutex<HashMap<String, RemoteDraft>>>,
    session_command_locks: Arc<Mutex<HashMap<sylph_protocol::SessionId, Arc<Mutex<()>>>>>,
    remote_promotion_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    server_epoch: u64,
}
impl EnvBridge {
    pub fn new(store: ControlStore, trace: Arc<TraceService>) -> Self {
        Self {
            registry: EnvironmentRegistry::new(),
            store,
            trace,
            leases: Arc::new(RwLock::new(HashMap::new())),
            command_policies: Arc::new(RwLock::new(HashMap::new())),
            command_waiters: Arc::new(Mutex::new(HashMap::new())),
            read_waiters: Arc::new(std::sync::Mutex::new(HashMap::new())),
            next_waiter_id: Arc::new(AtomicU64::new(1)),
            remote_drafts: Arc::new(Mutex::new(HashMap::new())),
            session_command_locks: Arc::new(Mutex::new(HashMap::new())),
            remote_promotion_locks: Arc::new(Mutex::new(HashMap::new())),
            server_epoch: now_ms() as u64,
        }
    }

    async fn serve_connection(&self, stream: TcpStream) -> Result<()> {
        // Capture RPCs exchange short receipts between length/body frame writes.
        // Nagle and the peer's delayed ACK otherwise add a timer to each reply.
        stream.set_nodelay(true)?;
        let (reader, writer) = stream.into_split();
        self.serve_io(reader, writer).await
    }

    pub async fn serve_io<R, W>(&self, mut reader: R, mut writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let first = read_frame(&mut reader).await?;
        let hello = match sylph_protocol::decode_message(&first)? {
            DecodedMessage::Known(WireMessage::Hello {
                protocol,
                versions,
                environment_id,
                installation_id,
                producer_id,
                connection_epoch,
                agents,
                health,
            }) => sylph_protocol::Hello {
                protocol,
                versions,
                environment_id,
                installation_id,
                producer_id,
                connection_epoch,
                agents,
                health,
            },
            _ => return Err(anyhow!("first envagent frame must be hello")),
        };
        let token: String = serde_json::from_slice(&read_frame(&mut reader).await?)?;
        let (tx, mut rx) = mpsc::channel::<WireMessage>(64);
        let welcome = self.register_connection(&hello, &token, tx.clone()).await?;
        write_frame(
            &mut writer,
            &serde_json::to_vec(&WireMessage::Welcome {
                protocol: welcome.protocol,
                version: welcome.version,
                environment_id: welcome.environment_id.clone(),
                connection_epoch: welcome.connection_epoch,
                server_epoch: welcome.server_epoch,
                heartbeat_interval_ms: welcome.heartbeat_interval_ms,
            })?,
        )
        .await?;
        let output = tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                write_frame(&mut writer, &serde_json::to_vec(&frame)?).await?;
            }
            Ok::<_, anyhow::Error>(())
        });
        let environment = welcome.environment_id;
        let epoch = welcome.connection_epoch;
        let result: Result<()> = async {
            loop {
                let frame = sylph_protocol::decode_message(&read_frame(&mut reader).await?)?;
                match frame {
                    DecodedMessage::Known(WireMessage::CommandResult {
                        request_id,
                        outcome,
                        connection_epoch,
                        idempotency,
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        self.record_command_result(
                            &environment,
                            &sylph_protocol::CommandResultMessage {
                                request_id,
                                outcome,
                                connection_epoch,
                                idempotency,
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::CaptureUpload {
                        request_id,
                        operation,
                        connection_epoch,
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        let outcome = self.receive_capture(&environment, operation).await;
                        tx.send(WireMessage::CaptureReply {
                            request_id,
                            outcome,
                            connection_epoch: epoch,
                        })
                        .await
                        .map_err(|_| BridgeError::NotConnected)?;
                    }
                    DecodedMessage::Known(WireMessage::Heartbeat {
                        connection_epoch, ..
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        if let Some(row) = self.store.environment_row(&environment).await? {
                            self.store
                                .set_environment_connection(
                                    &environment,
                                    &row.token_hash,
                                    epoch,
                                    true,
                                    row.reachable_via.as_deref(),
                                    &row.agents_json,
                                )
                                .await?;
                        }
                    }
                    DecodedMessage::Known(WireMessage::Agents {
                        agents,
                        connection_epoch,
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        self.registry
                            .update_agents(&environment, epoch, agents)
                            .await;
                    }
                    DecodedMessage::Known(WireMessage::SessionControlState {
                        session_id,
                        lease,
                        native_turn_id,
                        active,
                        connection_epoch,
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        self.store
                            .set_control_state(
                                &session_id,
                                lease.as_deref(),
                                native_turn_id.as_deref(),
                                active,
                                epoch,
                            )
                            .await?;
                    }
                    DecodedMessage::Known(WireMessage::Request {
                        session_id,
                        request,
                        connection_epoch,
                    }) => {
                        self.check_epoch(&environment, epoch, connection_epoch)
                            .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        self.store
                            .set_current_request(&session_id, &request, epoch)
                            .await?;
                    }
                    DecodedMessage::Known(other) => tracing::debug!(
                        kind = other.kind(),
                        "unhandled current envagent notification"
                    ),
                    DecodedMessage::Unknown { kind, .. } => {
                        return Err(anyhow!("unknown capture wire envelope {kind}"));
                    }
                }
            }
        }
        .await;
        let _ = self.disconnect(&environment, epoch).await;
        output.abort();
        result
    }

    async fn check_epoch(
        &self,
        environment: &EnvironmentId,
        expected: u64,
        received: u64,
    ) -> Result<()> {
        if expected != received
            || self
                .leases
                .read()
                .await
                .get(environment)
                .is_none_or(|lease| lease.epoch != expected)
        {
            return Err(BridgeError::StaleEpoch.into());
        }
        Ok(())
    }

    async fn receive_capture(
        &self,
        environment: &EnvironmentId,
        operation: sylph_protocol::CaptureUpload,
    ) -> sylph_protocol::CaptureReply {
        use sylph_protocol::{CaptureReply, CaptureUpload};
        let issuer = sylph_protocol::trace::Issuer::Environment(environment.clone());
        let accepted: Result<CaptureReply> = async {
            match operation {
                CaptureUpload::Begin {
                    capture,
                    declarations,
                } => match self
                    .trace
                    .uploads()
                    .begin(&issuer, capture, declarations)
                    .await?
                {
                    crate::trace::upload::BeginCapture::Pending(ticket) => {
                        Ok(CaptureReply::Pending { ticket })
                    }
                    crate::trace::upload::BeginCapture::Complete(batch) => Ok(capture_reply(batch)),
                },
                CaptureUpload::Chunk {
                    ticket,
                    reference,
                    bytes,
                } => {
                    self.trace
                        .uploads()
                        .put_chunk(&issuer, &ticket, &reference, &bytes)
                        .await?;
                    Ok(CaptureReply::ChunkStored)
                }
                CaptureUpload::Commit { ticket } => {
                    let batch = self.trace.uploads().finish(&issuer, &ticket).await?;
                    self.trace.notify_admission();
                    Ok(capture_reply(batch))
                }
            }
        }
        .await;
        match accepted {
            Ok(reply) => reply,
            Err(error) => CaptureReply::Rejected {
                code: "CAPTURE_REJECTED".into(),
                message: format!("{error:#}"),
            },
        }
    }

    pub async fn disconnect(&self, environment: &EnvironmentId, epoch: u64) -> Result<()> {
        let mut leases = self.leases.write().await;
        if leases
            .get(environment)
            .is_some_and(|lease| lease.epoch != epoch)
        {
            return Err(BridgeError::StaleEpoch.into());
        }
        leases.remove(environment);
        drop(leases);
        if let Some(row) = self.store.environment_row(environment).await? {
            self.store
                .clear_environment_connection_if_epoch(environment, &row.token_hash, epoch)
                .await?;
        }
        self.registry.disconnect(environment, epoch).await?;
        self.remote_drafts
            .lock()
            .await
            .retain(|_, draft| &draft.environment != environment);
        self.store.clear_environment_runtime(environment).await?;
        let keys: Vec<_> = self
            .command_waiters
            .lock()
            .await
            .keys()
            .filter(|(env, _)| env == environment)
            .cloned()
            .collect();
        for key in keys {
            for waiter in take_command_waiters(&self.command_waiters, &key).await {
                let _ = waiter.sender.send(CommandOutcome::Unknown {
                    message: Some("envagent connection closed".into()),
                });
            }
        }
        self.read_waiters
            .lock()
            .expect("read waiters")
            .retain(|(env, _), _| env != environment);
        Ok(())
    }

    pub async fn unpair(&self, environment: &EnvironmentId) -> Result<bool> {
        let epoch = self
            .leases
            .read()
            .await
            .get(environment)
            .map(|lease| lease.epoch);
        if let Some(epoch) = epoch {
            self.disconnect(environment, epoch).await?;
        }
        let _ = self.registry.unpair(environment).await;
        Ok(self.store.delete_environment(environment).await?)
    }

    pub async fn revoke_subject_control(
        &self,
        subject: &str,
        environment: &EnvironmentId,
    ) -> Result<()> {
        let sessions = self
            .store
            .list_sessions(&crate::db::SessionFilter {
                environment: Some(environment.to_string()),
                limit: 10000,
                ..Default::default()
            })
            .await?;
        for session in sessions {
            if let Some((Some(lease), true)) = self.store.control_state(&session.id).await? {
                if self
                    .store
                    .command(&lease)
                    .await?
                    .is_some_and(|command| command.acceptance.authorization.subject == subject)
                {
                    let _ = self
                        .remote_session_command(
                            &session.id,
                            sylph_protocol::EnvironmentCommand::Interrupt {
                                session_id: session.id.clone(),
                                expected_turn_id: Some(lease),
                            },
                            IdempotencyPolicy::AtMostOnce,
                        )
                        .await;
                }
            }
        }
        let drafts: Vec<_> = self
            .remote_drafts
            .lock()
            .await
            .values()
            .filter(|draft| &draft.environment == environment && draft.creator_subject == subject)
            .map(|draft| draft.draft_id.clone())
            .collect();
        for draft in drafts {
            self.discard_remote_draft(&draft).await;
        }
        Ok(())
    }

    pub fn spawn_health_reaper(self: &Arc<Self>) {
        let bridge = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let Some(bridge) = bridge.upgrade() else {
                    break;
                };
                match bridge
                    .store
                    .disconnect_stale_environments(now_ms() - 45_000)
                    .await
                {
                    Ok(envs) => {
                        for env in envs {
                            let epoch = bridge
                                .leases
                                .read()
                                .await
                                .get(&env)
                                .map(|lease| lease.epoch);
                            if let Some(epoch) = epoch {
                                let _ = bridge.disconnect(&env, epoch).await;
                            }
                        }
                    }
                    Err(error) => tracing::error!(%error,"environment heartbeat sweep failed"),
                }
            }
        });
    }

    pub async fn discard_remote_draft(&self, id: &str) -> Option<RemoteDraft> {
        let draft = self.remote_drafts.lock().await.remove(id)?;
        let _ = self
            .dispatch_remote_operation(
                &draft.environment,
                sylph_protocol::EnvironmentCommand::DraftClose {
                    draft_id: id.into(),
                },
                IdempotencyPolicy::AtMostOnce,
            )
            .await;
        Some(draft)
    }

    pub async fn mark_draft_command(&self, id: &str, command: &str) -> Result<RemoteDraft> {
        let mut drafts = self.remote_drafts.lock().await;
        let draft = drafts
            .get_mut(id)
            .ok_or_else(|| anyhow!("draft not found"))?;
        if draft
            .first_command_id
            .as_deref()
            .is_some_and(|old| old != command)
        {
            return Err(anyhow!("draft already dispatched another command"));
        }
        draft.promoting = true;
        draft.first_command_id = Some(command.into());
        Ok(draft.clone())
    }

    pub async fn remote_attach(&self, session: &crate::models::Session) -> Result<CommandOutcome> {
        let scope = IngestionScope::Agent(AgentRef::Session(session.id.clone()));
        let epoch = self
            .trace
            .catalogue()
            .evidence()
            .authorize_scope(&scope, &[AgentRef::Session(session.id.clone())])
            .await?;
        self.remote_session_command(
            &session.id,
            sylph_protocol::EnvironmentCommand::Run {
                capture_authorization: CaptureAuthorization { scope, epoch },
                session_id: Some(session.id.clone()),
                draft_id: None,
                agent: session.id.agent.to_string(),
                workdir: session
                    .workdir
                    .clone()
                    .ok_or_else(|| anyhow!("session workdir unknown"))?,
                flags: serde_json::from_str::<HashMap<String, String>>(&session.flags_json)?
                    .into_iter()
                    .collect(),
                inject_identity: session.inject_identity,
                identity_preamble: None,
                permission_mode: None,
                initial_input: None,
                initial_turn_id: None,
            },
            IdempotencyPolicy::Replayable,
        )
        .await
    }
    pub async fn authorize_remote_draft(
        &self,
        draft_id: &str,
        subject: &str,
        capability: &str,
    ) -> Result<RemoteDraft> {
        let draft = self
            .remote_draft(draft_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("draft not found"))?;
        if subject != "bootstrap" && draft.creator_subject != subject {
            return Err(anyhow::anyhow!("draft belongs to another Subject"));
        }
        if !self
            .store
            .has_capability(subject, draft.environment.as_str(), capability)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
        {
            return Err(anyhow::anyhow!("{capability} capability required"));
        }
        Ok(draft)
    }
    pub async fn serve(&self, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            let bridge = self.clone();
            tokio::spawn(async move {
                if let Err(error) = bridge.serve_connection(stream).await {
                    tracing::warn!("envagent connection ended: {error:#}");
                }
            });
        }
    }
    pub async fn serve_tls(&self, listener: TcpListener, acceptor: TlsAcceptor) -> Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            stream.set_nodelay(true)?;
            let bridge = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                match acceptor.accept(stream).await {
                    Ok(stream) => {
                        let (reader, writer) = tokio::io::split(stream);
                        if let Err(error) = bridge.serve_io(reader, writer).await {
                            tracing::warn!("TLS envagent connection ended: {error:#}");
                        }
                    }
                    Err(error) => tracing::warn!("TLS envagent handshake failed: {error:#}"),
                }
            });
        }
    }
    pub async fn create_pairing(&self) -> Result<PairingOffer> {
        let offer = self.registry.create_pairing().await;
        self.store
            .put_pairing(&hash_secret(&offer.code), offer.expires_at_ms)
            .await?;
        Ok(offer)
    }
    pub async fn create_invite(
        &self,
        environment: &EnvironmentId,
        installation_id: &str,
        token: &str,
        capability: &str,
        ttl_ms: i64,
    ) -> Result<PairingOffer> {
        if !matches!(capability, "observe" | "control") || ttl_ms <= 0 {
            return Err(BridgeError::InvalidCredential.into());
        }
        // A local invite proves the OS-user through the machine installation
        // credential. Server-authority (sandbox) environments have no such
        // local principal; their grants are issued by the server admin route.
        let row = self
            .store
            .environment_row(environment)
            .await?
            .ok_or(BridgeError::Unpaired)?;
        if self
            .store
            .grant_authority(environment.as_str())
            .await?
            .as_deref()
            != Some("machine")
            && row.origin != "development"
        {
            return Err(BridgeError::InvalidCredential.into());
        }
        if row.installation_id != installation_id
            || !constant_time_eq(row.token_hash.as_bytes(), hash_secret(token).as_bytes())
        {
            return Err(BridgeError::InvalidCredential.into());
        }
        let code = format!("invite-{}", uuid::Uuid::new_v4().simple());
        let expires_at_ms = now_ms().saturating_add(ttl_ms);
        self.store
            .put_invite(
                &super::hash_invite_code(&code),
                environment,
                installation_id,
                capability,
                expires_at_ms,
            )
            .await?;
        Ok(PairingOffer {
            code,
            expires_at_ms,
        })
    }
    pub async fn claim_pairing(
        &self,
        code: &str,
        name: impl Into<String>,
        origin: impl Into<String>,
        agents: Vec<sylph_protocol::AgentInfo>,
    ) -> Result<PairingGrant> {
        if !self.store.consume_pairing(&hash_secret(code)).await? {
            return Err(BridgeError::Unpaired.into());
        }
        self.registry.discard_pairing(code).await;
        let environment_id = mint_environment_id(&self.store).await?;
        let token = format!("envtok-{}", uuid::Uuid::new_v4().simple());
        let name = name.into();
        let origin = origin.into();
        let installation_id = format!("install-{}", uuid::Uuid::new_v4().simple());
        self.store
            .upsert_environment(
                &environment_id,
                &installation_id,
                &name,
                &origin,
                &hash_secret(&token),
                &serde_json::to_string(&agents)?,
            )
            .await?;
        // Keep the in-memory registry in sync for immediate API reads.  The
        // database remains authoritative across a server restart.
        let inventory = agents
            .iter()
            .cloned()
            .map(sylph_protocol::AgentInventory::available)
            .collect();
        self.registry
            .register_existing(
                environment_id.clone(),
                name,
                origin,
                token.clone(),
                inventory,
            )
            .await;
        Ok(PairingGrant {
            environment_id,
            installation_id,
            credential: token,
            expires_at_ms: now_ms() + 365 * 24 * 60 * 60 * 1000,
        })
    }
    pub async fn list_environments(&self) -> Result<Vec<EnvironmentSnapshot>> {
        // The operational store keeps the last known connection for restart diagnostics, but it is
        // not the current link. A server restart cannot inherit an envagent socket, and a
        // stale `connected = 1` row otherwise makes health/settings claim reachability
        // while every command correctly fails against the empty lease map. The lease is
        // the only live fact; persistence supplies identity and the last inventory.
        let leases = self.leases.read().await;
        let mut rows = Vec::new();
        for row in self.store.list_environment_rows().await? {
            let connected = leases.contains_key(&row.id);
            let agents = serde_json::from_str(&row.agents_json).map_err(|error| {
                anyhow!("invalid agent inventory in operational store: {error}")
            })?;
            let facts = serde_json::from_str(&row.facts_json).map_err(|error| {
                anyhow!("invalid environment facts in operational store: {error}")
            })?;
            rows.push(EnvironmentSnapshot {
                id: row.id,
                name: row.name,
                origin: row.origin,
                connected,
                reachable_via: row.reachable_via,
                connection_epoch: row.connection_epoch,
                agents,
                last_seen_at_ms: row.last_seen_at_ms,
                facts,
            });
        }
        // A freshly paired installation may only exist in memory when a
        // fixture uses the registry directly. Include it without duplicating
        // persisted rows.
        for snapshot in self.registry.list().await {
            if !rows.iter().any(|row| row.id == snapshot.id) {
                rows.push(snapshot);
            }
        }
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(rows)
    }
    pub async fn environment_connected(&self, environment: &EnvironmentId) -> bool {
        self.leases.read().await.get(environment).is_some()
    }
    pub async fn session_command_lock(
        &self,
        session_id: &sylph_protocol::SessionId,
    ) -> Arc<Mutex<()>> {
        let mut locks = self.session_command_locks.lock().await;
        locks
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    pub async fn remote_draft(&self, draft_id: &str) -> Option<RemoteDraft> {
        self.remote_drafts.lock().await.get(draft_id).cloned()
    }
    pub async fn remote_draft_known(&self, draft_id: &str) -> bool {
        self.remote_drafts.lock().await.contains_key(draft_id)
    }
    pub async fn remote_draft_promoting(&self, draft_id: &str) -> bool {
        self.remote_drafts
            .lock()
            .await
            .get(draft_id)
            .is_some_and(|draft| draft.promoting)
    }
    pub async fn remote_promotion_lock(&self, draft_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.remote_promotion_locks.lock().await;
        locks
            .entry(draft_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    pub async fn remote_draft_capabilities(
        &self,
        draft: &RemoteDraft,
    ) -> Result<serde_json::Value> {
        match self
            .dispatch_remote_operation(
                &draft.environment,
                sylph_protocol::EnvironmentCommand::DraftCapabilities {
                    draft_id: draft.draft_id.clone(),
                },
                IdempotencyPolicy::Replayable,
            )
            .await?
        {
            CommandOutcome::Accepted { result } => Ok(result.unwrap_or_default()),
            CommandOutcome::Rejected { code, message } => {
                Err(RemoteCommandError::rejected(code, message).into())
            }
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote draft capability outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }
    pub async fn remote_draft_set_config(
        &self,
        draft: &RemoteDraft,
        id: String,
        value: serde_json::Value,
    ) -> Result<CommandOutcome> {
        let requested_id = id.clone();
        let requested_value = value.clone();
        let outcome = self
            .dispatch_remote_operation(
                &draft.environment,
                sylph_protocol::EnvironmentCommand::DraftSetConfig {
                    draft_id: draft.draft_id.clone(),
                    id,
                    value,
                },
                IdempotencyPolicy::AtMostOnce,
            )
            .await?;
        if matches!(outcome, CommandOutcome::Accepted { .. }) {
            if let Some(value) = requested_value.as_str() {
                if let Some(current) = self.remote_drafts.lock().await.get_mut(&draft.draft_id) {
                    current.flags.insert(requested_id, value.to_string());
                }
            }
        }
        Ok(outcome)
    }
    pub async fn remote_files(
        &self,
        environment: &EnvironmentId,
        draft_id: Option<String>,
        session_id: Option<sylph_protocol::SessionId>,
        operation: sylph_protocol::FileOperation,
    ) -> Result<serde_json::Value> {
        let command = match draft_id {
            Some(draft_id) => sylph_protocol::EnvironmentCommand::DraftFiles {
                draft_id,
                operation,
            },
            None => {
                let session_id = session_id.ok_or_else(|| anyhow!("missing remote session id"))?;
                let workdir = self
                    .store
                    .get_session(&session_id)
                    .await?
                    .ok_or_else(|| anyhow!("session not found"))?
                    .workdir
                    .ok_or_else(|| anyhow!("session has no recorded working directory"))?;
                sylph_protocol::EnvironmentCommand::Files {
                    session_id,
                    workdir,
                    operation,
                }
            }
        };
        let idempotency = match &command {
            sylph_protocol::EnvironmentCommand::Files { operation, .. }
            | sylph_protocol::EnvironmentCommand::DraftFiles { operation, .. } => match operation {
                sylph_protocol::FileOperation::Write { .. }
                | sylph_protocol::FileOperation::Mkdir { .. }
                | sylph_protocol::FileOperation::Remove { .. } => IdempotencyPolicy::AtMostOnce,
                _ => IdempotencyPolicy::Replayable,
            },
            _ => IdempotencyPolicy::Replayable,
        };
        match self
            .dispatch_remote_operation(environment, command, idempotency)
            .await?
        {
            CommandOutcome::Accepted { result } => Ok(result.unwrap_or_default()),
            CommandOutcome::Rejected { code, message } => Err(anyhow!("{code}: {message}")),
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote file operation outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }
    pub async fn remote_browse(
        &self,
        environment: &EnvironmentId,
        command: sylph_protocol::EnvironmentCommand,
    ) -> Result<serde_json::Value> {
        match self
            .dispatch_remote_operation(environment, command, IdempotencyPolicy::Replayable)
            .await?
        {
            CommandOutcome::Accepted { result } => Ok(result.unwrap_or_default()),
            CommandOutcome::Rejected { code, message } => Err(anyhow!("{code}: {message}")),
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote browse outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }
    pub async fn remote_session_command(
        &self,
        session_id: &sylph_protocol::SessionId,
        command: sylph_protocol::EnvironmentCommand,
        idempotency: IdempotencyPolicy,
    ) -> Result<CommandOutcome> {
        // Independent actions receive independent request identities. Hashing
        // their payload would incorrectly replay a repeated steer or a config
        // change back to its earlier value. An actual retry preserves its request
        // identity in dispatch_and_wait and follows its declared retry policy.
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "remote-{}-{}-{}",
            session_id.environment,
            command_name(&command),
            uuid::Uuid::new_v4().simple()
        ))?;
        let timeout = command_timeout(&command);
        self.dispatch_and_wait(
            &session_id.environment,
            Command::new(request_id, idempotency, command),
            timeout,
        )
        .await
    }
    pub async fn close_remote_session(&self, session_id: &sylph_protocol::SessionId) -> Result<()> {
        let outcome = self
            .remote_session_command(
                session_id,
                sylph_protocol::EnvironmentCommand::Close {
                    session_id: Some(session_id.clone()),
                },
                IdempotencyPolicy::Replayable,
            )
            .await?;
        match outcome {
            CommandOutcome::Accepted { .. } => Ok(()),
            CommandOutcome::Rejected { code, message } => Err(anyhow!("{code}: {message}")),
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote close outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }
    pub async fn remote_outcome_value(outcome: CommandOutcome) -> Result<serde_json::Value> {
        match outcome {
            CommandOutcome::Accepted { result } => Ok(result.unwrap_or_default()),
            CommandOutcome::Rejected { code, message } => Err(anyhow!("{code}: {message}")),
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote command outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_remote(
        &self,
        creator_subject: String,
        environment: &EnvironmentId,
        agent: String,
        workdir: String,
        title: Option<String>,
        inject_identity: bool,
        flags: std::collections::HashMap<String, String>,
        identity_preamble: Option<String>,
        permission_mode: Option<String>,
    ) -> Result<(String, String)> {
        let draft_id = format!("remote-draft-{}", uuid::Uuid::new_v4().simple());
        let vendor = sylph_protocol::AgentName::parse(agent.clone())?;
        let scope = IngestionScope::Inventory(InventoryRef {
            environment: environment.clone(),
            vendor,
            instance: draft_id.clone(),
        });
        let epoch = self
            .trace
            .catalogue()
            .evidence()
            .authorize_scope(&scope, &[])
            .await?;
        let capture_authorization = CaptureAuthorization { scope, epoch };
        let request_id = sylph_protocol::CommandRequestId::parse(format!("prepare-{draft_id}"))?;
        let outcome = self
            .dispatch_and_wait(
                environment,
                Command::new(
                    request_id,
                    IdempotencyPolicy::AtMostOnce,
                    sylph_protocol::EnvironmentCommand::Prepare {
                        capture_authorization: capture_authorization.clone(),
                        draft_id: draft_id.clone(),
                        agent: agent.clone(),
                        workdir: workdir.clone(),
                        flags: flags.clone().into_iter().collect(),
                        inject_identity,
                        identity_preamble: identity_preamble.clone(),
                        permission_mode: permission_mode.clone(),
                    },
                ),
                std::time::Duration::from_secs(30),
            )
            .await?;
        let result = match outcome {
            CommandOutcome::Accepted {
                result: Some(value),
            } => value,
            CommandOutcome::Accepted { result: None } => serde_json::json!({}),
            CommandOutcome::Rejected { code, message } => {
                return Err(anyhow::Error::new(RemoteCommandError::rejected(
                    code, message,
                )));
            }
            CommandOutcome::Unknown { message } => {
                return Err(anyhow::Error::new(RemoteCommandError::Unknown {
                    message: message.unwrap_or_default(),
                }));
            }
        };
        let contained_workdir = result
            .get("workdir")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("remote prepare returned no contained workdir"))?
            .to_string();
        self.remote_drafts.lock().await.insert(
            draft_id.clone(),
            RemoteDraft {
                draft_id: draft_id.clone(),
                creator_subject,
                environment: environment.clone(),
                session_id: None,
                agent,
                workdir: contained_workdir.clone(),
                title,
                inject_identity,
                flags,
                identity_preamble,
                permission_mode,
                promoting: false,
                first_command_id: None,
                capture_authorization,
            },
        );
        Ok((draft_id, contained_workdir))
    }
    pub async fn register_connection(
        &self,
        hello: &sylph_protocol::Hello,
        token: &str,
        sender: mpsc::Sender<WireMessage>,
    ) -> Result<sylph_protocol::Welcome> {
        if hello.protocol != ENV_PROTOCOL {
            return Err(BridgeError::UnsupportedProtocol(hello.protocol.clone()).into());
        }
        let version = negotiate(&hello.versions).map_err(|_| BridgeError::VersionMismatch)?;
        let environment = hello.environment_id.clone().ok_or(BridgeError::Unpaired)?;
        let row = self
            .store
            .environment_row(&environment)
            .await?
            .ok_or(BridgeError::Unpaired)?;
        if row.installation_id != hello.installation_id {
            return Err(BridgeError::InvalidCredential.into());
        }
        if !constant_time_eq(row.token_hash.as_bytes(), hash_secret(token).as_bytes()) {
            return Err(BridgeError::InvalidCredential.into());
        }
        let agents_json = serde_json::to_string(&hello.agents)?;
        let facts_json = serde_json::to_string(
            &hello
                .health
                .as_ref()
                .map(|health| &health.facts)
                .cloned()
                .unwrap_or_default(),
        )?;
        let epoch = self
            .store
            .lease_environment_connection(
                &environment,
                &hash_secret(token),
                &agents_json,
                &facts_json,
            )
            .await?
            .ok_or(BridgeError::InvalidCredential)?;
        self.registry
            .register_existing(
                environment.clone(),
                row.name,
                row.origin,
                token.to_string(),
                hello.agents.clone(),
            )
            .await;
        self.registry
            .connect_trusted(
                &environment,
                epoch,
                hello.agents.clone(),
                hello
                    .health
                    .as_ref()
                    .map(|health| health.facts.clone())
                    .unwrap_or_default(),
            )
            .await;
        self.leases
            .write()
            .await
            .insert(environment.clone(), Lease { epoch, sender });
        let mut welcome = sylph_protocol::Welcome::current(environment, epoch, self.server_epoch);
        welcome.version = version;
        Ok(welcome)
    }
    pub async fn dispatch(
        &self,
        environment: &EnvironmentId,
        command: Command,
    ) -> Result<DispatchResult> {
        anyhow::ensure!(
            !is_stateless_read(&command.command),
            "stateless reads must use the read waiter dispatch path"
        );
        anyhow::ensure!(
            !command
                .request_id
                .as_str()
                .starts_with(EPHEMERAL_READ_PREFIX),
            "read request ids are reserved for stateless reads"
        );
        if let Some(outcome) = self
            .store
            .command_outcome(environment, command.request_id.as_str())
            .await?
        {
            match outcome {
                CommandOutcome::Accepted { .. } => return Ok(DispatchResult::AlreadyAccepted),
                CommandOutcome::Rejected { .. } => return Ok(DispatchResult::AlreadyRejected),
                CommandOutcome::Unknown { .. } if !command.idempotency.retry_allowed() => {
                    return Ok(DispatchResult::UnknownNotRetried);
                }
                // Replayable unknown outcomes continue through the send path
                // below with the same request id.
                CommandOutcome::Unknown { .. } => {}
            }
        }
        let sender = self
            .leases
            .read()
            .await
            .get(environment)
            .map(|lease| lease.sender.clone())
            .ok_or(BridgeError::NotConnected)?;
        let request_id = command.request_id.to_string();
        self.command_policies
            .write()
            .await
            .insert((environment.clone(), request_id), command.idempotency);
        // Record the ambiguous boundary before writing to the socket. If the
        // server crashes after the bytes leave but before a result returns,
        // an at-most-once retry sees Unknown and is not sent again.
        self.store
            .record_command_outcome(
                environment,
                command.request_id.as_str(),
                command.idempotency,
                &CommandOutcome::Unknown {
                    message: Some("command dispatched; result not yet durable".into()),
                },
            )
            .await?;
        sender
            .send(WireMessage::Command {
                request_id: command.request_id,
                idempotency: command.idempotency,
                command: command.command,
            })
            .await
            .map_err(|_| BridgeError::NotConnected)?;
        Ok(DispatchResult::Sent)
    }
    pub async fn dispatch_and_wait(
        &self,
        environment: &EnvironmentId,
        command: Command,
        timeout_duration: std::time::Duration,
    ) -> Result<CommandOutcome> {
        if is_stateless_read(&command.command) {
            return self
                .dispatch_read_and_wait(environment, command.command, timeout_duration)
                .await;
        }
        let key = (environment.clone(), command.request_id.to_string());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let waiter_id = self.next_waiter_id.fetch_add(1, Ordering::Relaxed);
        let already_waiting = {
            let mut waiters = self.command_waiters.lock().await;
            let entry = waiters.entry(key.clone()).or_default();
            let already_waiting = !entry.is_empty();
            entry.push(CommandWaiter {
                id: waiter_id,
                sender: tx,
            });
            already_waiting
        };
        // A retry that arrives while the original request is still in flight
        // joins its result instead of writing the same non-idempotent command
        // to the socket a second time.
        let dispatch = if already_waiting {
            Ok(DispatchResult::Sent)
        } else {
            self.dispatch(environment, command).await
        };
        let dispatch = match dispatch {
            Ok(value) => value,
            Err(error) => {
                let waiters = take_command_waiters(&self.command_waiters, &key).await;
                for waiter in waiters {
                    let _ = waiter.sender.send(CommandOutcome::Unknown {
                        message: Some(format!("command dispatch failed: {error:#}")),
                    });
                }
                return Err(error);
            }
        };
        match dispatch {
            DispatchResult::AlreadyAccepted | DispatchResult::AlreadyRejected => {
                let outcome = self
                    .store
                    .command_outcome(environment, &key.1)
                    .await?
                    .ok_or_else(|| {
                        anyhow!("command outcome disappeared before it could be read")
                    })?;
                let waiters = take_command_waiters(&self.command_waiters, &key).await;
                for waiter in waiters {
                    let _ = waiter.sender.send(outcome.clone());
                }
                Ok(outcome)
            }
            DispatchResult::UnknownNotRetried => {
                let outcome = CommandOutcome::Unknown {
                    message: Some("command outcome remains ambiguous".into()),
                };
                let waiters = take_command_waiters(&self.command_waiters, &key).await;
                for waiter in waiters {
                    let _ = waiter.sender.send(outcome.clone());
                }
                Ok(outcome)
            }
            DispatchResult::Sent => match tokio::time::timeout(timeout_duration, rx).await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(_)) => {
                    remove_command_waiter(&self.command_waiters, &key, waiter_id).await;
                    Ok(CommandOutcome::Unknown {
                        message: Some("connection closed before command result".into()),
                    })
                }
                Err(_) => {
                    remove_command_waiter(&self.command_waiters, &key, waiter_id).await;
                    Ok(CommandOutcome::Unknown {
                        message: Some("command result timed out".into()),
                    })
                }
            },
        }
    }
    async fn dispatch_read_and_wait(
        &self,
        environment: &EnvironmentId,
        command: sylph_protocol::EnvironmentCommand,
        timeout_duration: std::time::Duration,
    ) -> Result<CommandOutcome> {
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "{EPHEMERAL_READ_PREFIX}{}",
            uuid::Uuid::new_v4().simple()
        ))?;
        let key = (environment.clone(), request_id.to_string());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (outgoing, registration) = {
            let leases = self.leases.read().await;
            let lease = leases.get(environment).ok_or(BridgeError::NotConnected)?;
            self.read_waiters
                .lock()
                .expect("read waiter mutex poisoned")
                .insert(key.clone(), sender);
            (
                lease.sender.clone(),
                ReadRegistration {
                    key,
                    waiters: self.read_waiters.clone(),
                },
            )
        };
        // The deadline includes waiting for a socket queue slot. A synchronous
        // registration guard also removes the waiter when an HTTP future is dropped.
        let outcome = tokio::time::timeout(timeout_duration, async {
            outgoing
                .send(WireMessage::Command {
                    request_id,
                    idempotency: IdempotencyPolicy::Replayable,
                    command,
                })
                .await
                .map_err(|_| BridgeError::NotConnected)?;
            Ok::<_, anyhow::Error>(receiver.await.unwrap_or_else(|_| CommandOutcome::Unknown {
                message: Some("connection closed before read result".into()),
            }))
        })
        .await;
        drop(registration);
        outcome.unwrap_or_else(|_| {
            Ok(CommandOutcome::Unknown {
                message: Some("read result timed out".into()),
            })
        })
    }
    pub async fn record_command_result(
        &self,
        environment: &EnvironmentId,
        result: &sylph_protocol::CommandResultMessage,
    ) -> Result<()> {
        // This namespace is minted only by typed stateless dispatch and rejected
        // for durable commands. It identifies late replies after timeout/reconnect
        // without retaining unbounded tombstones or writing read payloads to SQLite.
        if result
            .request_id
            .as_str()
            .starts_with(EPHEMERAL_READ_PREFIX)
        {
            let waiter = self
                .read_waiters
                .lock()
                .expect("read waiter mutex poisoned")
                .remove(&(environment.clone(), result.request_id.to_string()));
            if let Some(waiter) = waiter {
                let _ = waiter.send(result.outcome.clone());
            }
            return Ok(());
        }
        let policy = if let Some(policy) = result.idempotency {
            policy
        } else {
            self.command_policies
                .read()
                .await
                .get(&(environment.clone(), result.request_id.to_string()))
                .copied()
                .unwrap_or(IdempotencyPolicy::AtMostOnce)
        };
        self.store
            .record_command_outcome(
                environment,
                result.request_id.as_str(),
                policy,
                &result.outcome,
            )
            .await?;
        let waiters = self
            .command_waiters
            .lock()
            .await
            .remove(&(environment.clone(), result.request_id.to_string()));
        if let Some(waiters) = waiters {
            for waiter in waiters {
                let _ = waiter.sender.send(result.outcome.clone());
            }
        }
        if result.outcome.is_terminal() {
            self.command_policies
                .write()
                .await
                .remove(&(environment.clone(), result.request_id.to_string()));
        }
        Ok(())
    }
    async fn dispatch_remote_operation(
        &self,
        environment: &EnvironmentId,
        command: sylph_protocol::EnvironmentCommand,
        idempotency: IdempotencyPolicy,
    ) -> Result<CommandOutcome> {
        let request_id = remote_request_id(environment, &command, idempotency)?;
        let timeout = command_timeout(&command);
        self.dispatch_and_wait(
            environment,
            Command::new(request_id, idempotency, command),
            timeout,
        )
        .await
    }
}
fn is_stateless_read(command: &sylph_protocol::EnvironmentCommand) -> bool {
    use sylph_protocol::{EnvironmentCommand, FileOperation};
    matches!(
        command,
        EnvironmentCommand::Health
            | EnvironmentCommand::Capabilities { .. }
            | EnvironmentCommand::DraftCapabilities { .. }
            | EnvironmentCommand::BrowseRoots
            | EnvironmentCommand::BrowseTree { .. }
            | EnvironmentCommand::Enumerate { .. }
            | EnvironmentCommand::Files {
                operation: FileOperation::Read { .. }
                    | FileOperation::List { .. }
                    | FileOperation::Stat { .. },
                ..
            }
            | EnvironmentCommand::DraftFiles {
                operation: FileOperation::Read { .. }
                    | FileOperation::List { .. }
                    | FileOperation::Stat { .. },
                ..
            }
    )
}
pub(crate) fn command_timeout(command: &sylph_protocol::EnvironmentCommand) -> std::time::Duration {
    use sylph_protocol::EnvironmentCommand;
    // Native snapshots are bounded by their data size, not an interactive handshake.
    // Their separate scheduler capacity keeps this longer deadline from blocking files.
    match command {
        EnvironmentCommand::Reread { .. }
        | EnvironmentCommand::Adopt { .. }
        | EnvironmentCommand::Watch { .. }
        | EnvironmentCommand::Enumerate { .. } => std::time::Duration::from_secs(300),
        _ => std::time::Duration::from_secs(30),
    }
}
fn command_name(command: &sylph_protocol::EnvironmentCommand) -> &'static str {
    use sylph_protocol::EnvironmentCommand;
    match command {
        EnvironmentCommand::Prepare { .. } => "prepare",
        EnvironmentCommand::Run { .. } => "run",
        EnvironmentCommand::Send { .. } => "send",
        EnvironmentCommand::Interrupt { .. } => "interrupt",
        EnvironmentCommand::Steer { .. } => "steer",
        EnvironmentCommand::Queue { .. } => "queue",
        EnvironmentCommand::QueueSteering { .. } => "queue-steering",
        EnvironmentCommand::SetConfig { .. } => "set-config",
        EnvironmentCommand::Resolve { .. } => "resolve",
        EnvironmentCommand::Watch { .. } => "watch",
        EnvironmentCommand::Adopt { .. } => "adopt",
        EnvironmentCommand::Reread { .. } => "reread",
        EnvironmentCommand::Files { .. } => "files",
        EnvironmentCommand::DraftCapabilities { .. } => "draft-capabilities",
        EnvironmentCommand::DraftSetConfig { .. } => "draft-set-config",
        EnvironmentCommand::DraftFiles { .. } => "draft-files",
        EnvironmentCommand::DraftClose { .. } => "draft-close",
        EnvironmentCommand::Health => "health",
        EnvironmentCommand::Capabilities { .. } => "capabilities",
        EnvironmentCommand::Enumerate { .. } => "enumerate",
        EnvironmentCommand::Close { .. } => "close",
        EnvironmentCommand::BrowseRoots => "browse-roots",
        EnvironmentCommand::BrowseTree { .. } => "browse-tree",
    }
}
fn remote_request_id(
    environment: &EnvironmentId,
    command: &sylph_protocol::EnvironmentCommand,
    _idempotency: IdempotencyPolicy,
) -> Result<sylph_protocol::CommandRequestId> {
    Ok(sylph_protocol::CommandRequestId::parse(format!(
        "remote-{environment}-{}-{}",
        command_name(command),
        uuid::Uuid::new_v4()
    ))?)
}
async fn take_command_waiters(
    waiters: &CommandWaiters,
    key: &(EnvironmentId, String),
) -> Vec<CommandWaiter> {
    waiters.lock().await.remove(key).unwrap_or_default()
}
async fn remove_command_waiter(waiters: &CommandWaiters, key: &(EnvironmentId, String), id: u64) {
    let mut guard = waiters.lock().await;
    if let Some(entries) = guard.get_mut(key) {
        entries.retain(|entry| entry.id != id);
        if entries.is_empty() {
            guard.remove(key);
        }
    }
}
fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
async fn mint_environment_id(store: &ControlStore) -> Result<EnvironmentId> {
    for _ in 0..16 {
        let raw = uuid::Uuid::new_v4().as_bytes().to_vec();
        let candidate = base32_lower(&raw);
        let id = EnvironmentId::parse(candidate)?;
        if store.environment_row(&id).await?.is_none() {
            return Ok(id);
        }
    }
    Err(anyhow!("could not allocate a unique environment id"))
}
fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut accumulator = 0_u16;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((accumulator >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((accumulator << (5 - bits)) & 31) as usize] as char);
    }
    out
}
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(anyhow!("invalid envagent frame length {length}"));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, payload: &[u8]) -> Result<()> {
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(anyhow!("invalid envagent frame length {}", payload.len()));
    }
    writer.write_u32(payload.len() as u32).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}
const MAX_FRAME_BYTES: usize = sylph_protocol::MAX_FRAME_BYTES;

fn capture_reply(batch: crate::trace::evidence::AdmissionBatch) -> sylph_protocol::CaptureReply {
    use sylph_protocol::{CaptureDisposition, CaptureReceipt, CaptureReply};
    CaptureReply::Admitted {
        receipts: batch
            .receipts
            .into_iter()
            .map(|receipt| CaptureReceipt {
                observation: receipt.observation_ref,
                disposition: match receipt.disposition {
                    crate::trace::evidence::ReceiptDisposition::Retained { admission_seq } => {
                        CaptureDisposition::Retained { admission_seq }
                    }
                    crate::trace::evidence::ReceiptDisposition::DiscardedByDeletion {
                        tombstone_id,
                    } => CaptureDisposition::DiscardedByDeletion { tombstone_id },
                },
            })
            .collect(),
        accepted_through: batch.accepted_through,
        evidence_cut: batch.evidence_cut,
    }
}
