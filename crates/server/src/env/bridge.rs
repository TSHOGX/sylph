//! Inbound envagent connection manager and canonical event ingest.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use sylph_protocol::{
    negotiate, Ack, Command, CommandOutcome, DecodedMessage, EnvironmentId, EventBatch,
    IdempotencyPolicy, ProducerId, ProducerSeq, ServerSeq, WireMessage, ENV_PROTOCOL,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_rustls::TlsAcceptor;

use crate::db::{EnvIngestResult, JournalStore};
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::models::now_ms;
use crate::supervisor::{Outcome, TrajectoryOutcome, TrajectoryReconciler, TurnCloser};

use super::registry::{EnvironmentRegistry, EnvironmentSnapshot, PairingGrant, PairingOffer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Sent,
    AlreadyAccepted,
    AlreadyRejected,
    UnknownNotRetried,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    Ack(Ack),
    Nack {
        producer_id: ProducerId,
        expected_next: ProducerSeq,
        reason: String,
    },
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
    producer_id: ProducerId,
}

/// Metadata retained for a prepared link.
///
/// The envagent allocates the native identity before the server can expose a durable
/// session, because identity is `(environment, agent, native_id)` and only the vendor
/// mints the third part (`docs/architecture.md` § Identity).  So this record already
/// carries a session id while the first turn is still waiting in the browser — the id
/// exists before any row does, which is the opposite of a server-minted key and the
/// reason a draft needs its own type at all.
#[derive(Debug, Clone)]
pub struct RemoteDraft {
    /// Stable command scope for this draft. Retrying the first-turn request
    /// must address the same envagent command rather than minting a second
    /// non-idempotent run.
    pub draft_id: String,
    pub environment: EnvironmentId,
    pub session_id: Option<sylph_protocol::SessionId>,
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    pub inject_identity: bool,
    pub flags: std::collections::HashMap<String, String>,
    pub identity_preamble: Option<String>,
    pub permission_mode: Option<String>,
    pub promoting: bool,
    pub first_turn_id: Option<String>,
}

/// Result of a successful remote draft promotion.  Drafts are intentionally
/// process-local, but an idempotent HTTP retry can arrive immediately after a
/// promotion removed the live draft.  Keeping this small response cache lets
/// that retry return the original identity instead of attempting a second
/// non-idempotent `Run` command.
#[derive(Debug, Clone)]
pub struct RemotePromotionResult {
    pub turn_id: String,
    pub session_id: sylph_protocol::SessionId,
}

#[derive(Debug, Clone)]
pub enum RemoteDraftClaim {
    Missing,
    Busy,
    Conflict {
        turn_id: String,
    },
    Claimed {
        draft: Box<RemoteDraft>,
        turn_id: String,
    },
}

struct CommandWaiter {
    id: u64,
    sender: tokio::sync::oneshot::Sender<CommandOutcome>,
}

type CommandWaiters = Arc<Mutex<HashMap<(EnvironmentId, String), Vec<CommandWaiter>>>>;

/// Owns paired-environment state and the one ingress serialization point.
#[derive(Clone)]
pub struct EnvBridge {
    pub registry: EnvironmentRegistry,
    store: JournalStore,
    history: crate::storage::HistoryStore,
    seq: SeqAllocator,
    hub: EventHub,
    leases: Arc<RwLock<HashMap<EnvironmentId, Lease>>>,
    command_policies: Arc<RwLock<HashMap<(EnvironmentId, String), IdempotencyPolicy>>>,
    command_waiters: CommandWaiters,
    next_waiter_id: Arc<AtomicU64>,
    ingest_gate: Arc<Mutex<()>>,
    remote_drafts: Arc<Mutex<HashMap<String, RemoteDraft>>>,
    remote_promotion_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    remote_promotion_results: Arc<Mutex<HashMap<String, RemotePromotionResult>>>,
    session_command_locks: Arc<Mutex<HashMap<sylph_protocol::SessionId, Arc<Mutex<()>>>>>,
    remote_source_turns: Arc<Mutex<HashMap<sylph_protocol::SessionId, String>>>,
    /// Shared terminal settlement path.  The bridge is constructed before the
    /// supervisor (which owns the consolidator), so this is installed by
    /// `AppState::build` once both sides exist.
    turn_closer: Arc<RwLock<Option<TurnCloser>>>,
    /// Applies late delegated outcomes to the turn that opened the trajectory.
    /// Remote event batches bypass the live link drain, so they need the same
    /// reconciler wired explicitly at startup.
    trajectory_reconciler: Arc<RwLock<Option<TrajectoryReconciler>>>,
    server_epoch: u64,
}

impl EnvBridge {
    pub fn new(
        store: JournalStore,
        history: crate::storage::HistoryStore,
        seq: SeqAllocator,
        hub: EventHub,
        registry: EnvironmentRegistry,
    ) -> Self {
        Self {
            registry,
            store,
            history,
            seq,
            hub,
            leases: Arc::new(RwLock::new(HashMap::new())),
            command_policies: Arc::new(RwLock::new(HashMap::new())),
            command_waiters: Arc::new(Mutex::new(HashMap::new())),
            next_waiter_id: Arc::new(AtomicU64::new(1)),
            ingest_gate: Arc::new(Mutex::new(())),
            remote_drafts: Arc::new(Mutex::new(HashMap::new())),
            remote_promotion_locks: Arc::new(Mutex::new(HashMap::new())),
            remote_promotion_results: Arc::new(Mutex::new(HashMap::new())),
            session_command_locks: Arc::new(Mutex::new(HashMap::new())),
            remote_source_turns: Arc::new(Mutex::new(HashMap::new())),
            turn_closer: Arc::new(RwLock::new(None)),
            trajectory_reconciler: Arc::new(RwLock::new(None)),
            server_epoch: now_ms() as u64,
        }
    }

    /// Install the canonical turn settlement path after supervisor construction.
    pub async fn set_turn_closer(&self, closer: TurnCloser) {
        *self.turn_closer.write().await = Some(closer);
    }

    pub async fn interrupt_turn(
        &self,
        session_id: &sylph_protocol::SessionId,
        turn_id: &str,
    ) -> bool {
        let Some(closer) = self.turn_closer.read().await.clone() else {
            return false;
        };
        closer
            .close(session_id, turn_id, Outcome::Interrupted)
            .await
    }

    /// Journal and publish a server-authored event for a turn.
    ///
    /// Server-authored, so `source` is `server` rather than `agent`: a status the server
    /// decided is not something the vendor said, and a reader that could not tell them
    /// apart would attribute the server's own reasoning to the agent.
    pub async fn emit_server_event(
        &self,
        session_id: &sylph_protocol::SessionId,
        turn_id: &str,
        kind: &str,
        text: String,
        data: Option<serde_json::Value>,
    ) -> Result<()> {
        let event = crate::events::StreamEvent {
            seq: self.seq.next(session_id).await?,
            arrival_turn_id: Some(turn_id.to_string()),
            kind: kind.to_string(),
            phase: None,
            stream_id: None,
            span: None,
            interior_state: None,
            name: None,
            text,
            data,
            is_error: false,
            stability: "stable".into(),
            source: "server".into(),
            arrival_at_ms: now_ms(),
            recorded_at_ms: None,
            created_at_ms: now_ms(),
            started_at_ms: None,
        };
        self.store
            .append_event(&event.to_row(session_id, turn_id))
            .await?;
        self.hub.publish(turn_id, event.clone()).await;
        self.hub.publish_trace(&session_id.encode(), event).await;
        Ok(())
    }

    /// Install the shared trajectory reconciliation path used by remote ingest.
    pub async fn set_trajectory_reconciler(&self, reconciler: TrajectoryReconciler) {
        *self.trajectory_reconciler.write().await = Some(reconciler);
    }

    pub fn spawn_health_reaper(&self) {
        let bridge = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                ticker.tick().await;
                let cutoff = now_ms() - 45_000;
                let Ok(stale) = bridge.store.disconnect_stale_environments(cutoff).await else {
                    continue;
                };
                if !stale.is_empty() {
                    let mut leases = bridge.leases.write().await;
                    for environment in stale {
                        leases.remove(&environment);
                    }
                }
            }
        });
    }

    /// Serve envagent connections over the local development transport.
    ///
    /// The production deployment may wrap the accepted stream in TLS; the
    /// message and acknowledgement semantics remain identical. This listener
    /// is opt-in so existing loopback server tests do not bind an extra port.
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

    /// Serve envagents over TLS. The framed protocol is identical to the
    /// loopback listener; only the accepted byte stream is wrapped before the
    /// bridge performs its credential and hello handshake.
    pub async fn serve_tls(&self, listener: TcpListener, acceptor: TlsAcceptor) -> Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
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

    async fn serve_connection(&self, stream: TcpStream) -> Result<()> {
        let (reader, writer) = tokio::io::split(stream);
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
                spool_cursors,
                agents,
                health,
            }) => sylph_protocol::Hello {
                protocol,
                versions,
                environment_id,
                installation_id,
                producer_id,
                connection_epoch,
                spool_cursors,
                agents,
                health,
            },
            DecodedMessage::Unknown { kind, .. } => {
                return Err(anyhow!("first envagent frame must be hello, got {kind}"))
            }
            DecodedMessage::Known(other) => {
                return Err(anyhow!(
                    "first envagent frame must be hello, got {}",
                    other.kind()
                ))
            }
        };
        // The credential is carried in a small preface for the TCP fixture:
        // a real TLS deployment obtains it from the authenticated channel.
        let credential_frame = read_frame(&mut reader).await?;
        let credential: String = serde_json::from_slice(&credential_frame)
            .context("decoding envagent credential preface")?;
        let (tx, mut rx) = mpsc::channel::<WireMessage>(64);
        let welcome = self
            .register_connection(&hello, &credential, tx.clone())
            .await?;
        write_frame(
            &mut writer,
            &serde_json::to_vec(&WireMessage::Welcome {
                protocol: welcome.protocol.clone(),
                version: welcome.version,
                environment_id: welcome.environment_id.clone(),
                connection_epoch: welcome.connection_epoch,
                server_epoch: welcome.server_epoch,
                accepted_cursors: welcome.accepted_cursors.clone(),
                max_batch_events: welcome.max_batch_events,
                heartbeat_interval_ms: welcome.heartbeat_interval_ms,
            })?,
        )
        .await?;

        let writer_task = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                let bytes = serde_json::to_vec(&message)?;
                write_frame(&mut writer, &bytes).await?;
            }
            Ok::<(), anyhow::Error>(())
        });
        let environment = welcome.environment_id.clone();
        let epoch = welcome.connection_epoch;
        let result: Result<()> = async {
            loop {
                let bytes = match read_frame(&mut reader).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let _ = self.disconnect(&environment, epoch).await;
                        writer_task.abort();
                        return Err(error);
                    }
                };
                match sylph_protocol::decode_message(&bytes)? {
                    DecodedMessage::Known(WireMessage::EventBatch {
                        environment_id,
                        producer_id,
                        connection_epoch,
                        events,
                    }) => {
                        if environment_id != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        let result = self
                            .ingest_batch(EventBatch {
                                environment_id,
                                producer_id,
                                connection_epoch,
                                events,
                            })
                            .await?;
                        let response = match result {
                            IngestResult::Ack(ack) => WireMessage::Ack {
                                producer_id: ack.producer_id,
                                accepted_through: ack.accepted_through,
                                server_seq_through: ack.server_seq_through,
                            },
                            IngestResult::Nack {
                                producer_id,
                                expected_next,
                                reason,
                            } => WireMessage::Nack {
                                producer_id,
                                expected_next,
                                reason,
                            },
                        };
                        tx.send(response)
                            .await
                            .map_err(|_| anyhow!("envagent writer closed"))?;
                    }
                    DecodedMessage::Known(WireMessage::CommandResult {
                        request_id,
                        outcome,
                        connection_epoch: result_epoch,
                        idempotency,
                    }) => {
                        if result_epoch != epoch {
                            return Err(BridgeError::StaleEpoch.into());
                        }
                        self.record_command_result(
                            &environment,
                            &sylph_protocol::CommandResultMessage {
                                request_id,
                                outcome,
                                connection_epoch: result_epoch,
                                idempotency,
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::Heartbeat {
                        connection_epoch, ..
                    }) => {
                        if connection_epoch != epoch {
                            return Err(BridgeError::StaleEpoch.into());
                        }
                        if let Some(row) = self.store.environment_row(&environment).await? {
                            let _ = self
                                .store
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
                        let _ = self.registry.touch(&environment, epoch).await;
                    }
                    DecodedMessage::Known(WireMessage::Agents {
                        connection_epoch,
                        agents,
                    }) => {
                        if connection_epoch != epoch {
                            return Err(BridgeError::StaleEpoch.into());
                        }
                        let agents_json = serde_json::to_string(&agents)?;
                        if let Some(row) = self.store.environment_row(&environment).await? {
                            let _ = self
                                .store
                                .set_environment_connection(
                                    &environment,
                                    &row.token_hash,
                                    epoch,
                                    true,
                                    row.reachable_via.as_deref(),
                                    &agents_json,
                                )
                                .await?;
                        }
                        let _ = self
                            .registry
                            .update_agents(&environment, epoch, agents)
                            .await;
                    }
                    DecodedMessage::Known(WireMessage::TurnInput {
                        session_id,
                        turn_id,
                        turn,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        if let Some(cursor) = turn.source_cursor.as_ref() {
                            tracing::debug!(
                                session = %session_id,
                                source = %cursor.source,
                                offset = cursor.offset,
                                "received adopted turn input"
                            );
                        }
                        // A native input fact is a container update, not a
                        // timeline event. The turn id is carried by the source
                        // cursor when an adapter has one; absent that, retain the
                        // text only in a diagnostic rather than inventing a turn.
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "sessionId": session_id,
                            "turnId": turn_id,
                            "turn": turn,
                        }))?);
                        let store = self.store.clone();
                        let bridge = self.clone();
                        let session_for_effect = session_id.clone();
                        let turn_id_for_effect = turn_id.clone();
                        let text = turn.text.clone();
                        let recorded_at_ms = turn.recorded_at_ms.unwrap_or_else(now_ms);
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            move || async move {
                                let target_turn = match turn_id_for_effect {
                                    Some(turn_id) => turn_id,
                                    None => {
                                        bridge
                                            .ensure_remote_source_turn(
                                                &session_for_effect,
                                                recorded_at_ms,
                                            )
                                            .await?
                                    }
                                };
                                let _ = store
                                    .set_turn_request_text(&session_for_effect, &target_turn, &text)
                                    .await?;
                                Ok(())
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::Edge {
                        edge,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if edge.parent.environment != environment
                            || edge.child.environment != environment
                        {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "edge": edge,
                        }))?);
                        let store = self.store.clone();
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            move || async move {
                                store.insert_session_edge(&edge).await?;
                                Ok(())
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::NativeRecord {
                        session_id,
                        record,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "sessionId": session_id,
                            "record": record,
                        }))?);
                        let history = self.history.clone();
                        let session_key = session_id.encode();
                        let record_source = record.source.clone();
                        let record_version = record.version;
                        let result = self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            move || async move {
                                history.put_native_record(&session_key, &record).await?;
                                Ok(())
                            },
                        )
                        .await;
                        if let Err(error) = result {
                            // A native record is the source of truth for any future
                            // re-projection.  If retaining it fails, stop the affected
                            // turn before tearing down the connection so callers observe
                            // a terminal LINK_LOST outcome instead of a turn that hangs
                            // until their polling deadline.
                            if let Some(turn_id) = self.store.running_turn_id(&session_id).await? {
                                if let Some(closer) = self.turn_closer.read().await.clone() {
                                    closer
                                        .close(
                                            &session_id,
                                            &turn_id,
                                            Outcome::LinkLost {
                                                detail: Some(format!(
                                                    "could not retain native record {}:{}: {error:#}",
                                                    record_source, record_version
                                                )),
                                            },
                                        )
                                        .await;
                                }
                            }
                            return Err(error);
                        }
                    }
                    DecodedMessage::Known(WireMessage::Request {
                        session_id,
                        request,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        let payload = serde_json::to_value(&request)?;
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "sessionId": session_id,
                            "request": payload.clone(),
                        }))?);
                        let hub = self.hub.clone();
                        let session_key = session_id.encode();
                        let store = self.store.clone();
                        let request_for_store = request.clone();
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            move || async move {
                                let turn_id = store
                                    .running_turn_id(&session_id)
                                    .await?
                                    .unwrap_or_default();
                                let pending = crate::models::PendingRequest {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    session_id: session_id.clone(),
                                    turn_id,
                                    adapter_request_id: request_for_store.id.clone(),
                                    kind: request_for_store.kind.to_string(),
                                    tool: request_for_store.tool.clone(),
                                    tool_use_id: request_for_store.tool_use_id.clone(),
                                    title: request_for_store.title.clone(),
                                    detail_json: request_for_store
                                        .detail
                                        .as_ref()
                                        .map(|value| value.to_string())
                                        .unwrap_or_else(|| "{}".into()),
                                    options_json: serde_json::to_string(&request_for_store.options)
                                        .unwrap_or_else(|_| "[]".into()),
                                    form_json: request_for_store
                                        .form
                                        .as_ref()
                                        .and_then(|value| serde_json::to_string(value).ok()),
                                    default_option: request_for_store.default.clone(),
                                    risk: request_for_store.risk.map(|value| value.to_string()),
                                    allow_persistent: request_for_store.allow_persistent,
                                    expires_at_ms: request_for_store.expires_at_ms,
                                    status: "pending".into(),
                                    chosen_option: None,
                                    resolved_input_json: None,
                                    resolved_by: None,
                                    created_at_ms: now_ms(),
                                    resolved_at_ms: None,
                                };
                                let stored = store.create_pending_request(&pending).await?;
                                hub.publish_session(
                                    &session_key,
                                    StreamEvent {
                                        seq: 0,
                                        arrival_turn_id: None,
                                        kind: "request".into(),
                                        phase: None,
                                        stream_id: None,
                                        span: None,
                                        interior_state: None,
                                        name: Some(stored.title.clone()),
                                        text: String::new(),
                                        data: Some(payload),
                                        is_error: false,
                                        stability: "stable".into(),
                                        source: "agent".into(),
                                        arrival_at_ms: now_ms(),
                                        recorded_at_ms: None,
                                        created_at_ms: now_ms(),
                                        started_at_ms: None,
                                    },
                                )
                                .await;
                                Ok(())
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::CapabilitySnapshot {
                        session_id,
                        facets,
                        link,
                        capabilities,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        // Capability snapshots are current state, not history.
                        // Publish the typed payload to attached clients; the
                        // link registry remains the source of truth for commands.
                        let payload = serde_json::json!({
                            "facets": facets,
                            "link": link,
                            "capabilities": capabilities,
                        });
                        let payload_hash = hash_bytes(&serde_json::to_vec(&payload)?);
                        let hub = self.hub.clone();
                        let session_key = session_id.encode();
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            move || async move {
                                hub.publish_session(
                                    &session_key,
                                    StreamEvent {
                                        seq: 0,
                                        arrival_turn_id: None,
                                        kind: "capabilities".into(),
                                        phase: None,
                                        stream_id: None,
                                        span: None,
                                        interior_state: None,
                                        name: None,
                                        text: String::new(),
                                        data: Some(payload),
                                        is_error: false,
                                        stability: "ephemeral".into(),
                                        source: "agent".into(),
                                        arrival_at_ms: now_ms(),
                                        recorded_at_ms: None,
                                        created_at_ms: now_ms(),
                                        started_at_ms: None,
                                    },
                                )
                                .await;
                                Ok(())
                            },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::SourceState {
                        session_id,
                        cursor,
                        state,
                        detail,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id.environment != environment {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        tracing::debug!(
                            session = %session_id,
                            source = %cursor.source,
                            generation = cursor.generation,
                            state = %state,
                            detail = ?detail,
                            "envagent source state"
                        );
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "sessionId": session_id,
                            "cursor": cursor,
                            "state": state,
                            "detail": detail,
                        }))?);
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            || async { Ok(()) },
                        )
                        .await?;
                    }
                    DecodedMessage::Known(WireMessage::Diagnostic {
                        code,
                        message,
                        producer_id,
                        producer_seq,
                        connection_epoch: frame_epoch,
                        session_id,
                        details,
                        ..
                    }) => {
                        self.validate_producer_frame(
                            &environment,
                            epoch,
                            frame_epoch,
                            &producer_id,
                        )
                        .await?;
                        if session_id
                            .as_ref()
                            .is_some_and(|id| id.environment != environment)
                        {
                            return Err(BridgeError::ProducerMismatch.into());
                        }
                        // Diagnostics are non-history messages, but their
                        // delivery still participates in the producer spool
                        // prefix so a later event cannot be acknowledged across
                        // an unprocessed diagnostic.
                        let payload_hash = hash_bytes(&serde_json::to_vec(&serde_json::json!({
                            "code": code,
                            "message": message,
                            "sessionId": session_id,
                            "details": details,
                        }))?);
                        self.process_message(
                            &environment,
                            epoch,
                            &producer_id,
                            producer_seq,
                            &payload_hash,
                            &tx,
                            || async { Ok(()) },
                        )
                        .await?;
                        if code == "source_closed" || code == "spooled_source_closed" {
                            if let Some(session_id) = session_id {
                                if let Some(turn_id) =
                                    self.remote_source_turns.lock().await.remove(&session_id)
                                {
                                    if let Some(closer) = self.turn_closer.read().await.clone() {
                                        closer
                                            .close(
                                                &session_id,
                                                &turn_id,
                                                Outcome::Completed { stop_reason: None },
                                            )
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    DecodedMessage::Known(other) => {
                        tracing::debug!("ignoring envagent frame {}", other.kind());
                    }
                    DecodedMessage::Unknown { kind, .. } => {
                        tracing::debug!("ignoring unknown envagent frame {kind}");
                    }
                }
            }
        }
        .await;
        let _ = self.disconnect(&environment, epoch).await;
        writer_task.abort();
        result
    }

    pub async fn create_pairing(&self) -> Result<PairingOffer> {
        let offer = self.registry.create_pairing().await;
        self.store
            .put_pairing(&hash_secret(&offer.code), offer.expires_at_ms)
            .await?;
        Ok(offer)
    }

    /// Issue a one-shot capability invite for an already paired installation.
    /// The installation credential is accepted only for its own environment;
    /// this is the local-machine authority proof and is deliberately separate
    /// from the browser Subject cookie.
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

    /// Consume a code and persist a durable installation credential.
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
        // The journal keeps the last known connection for restart diagnostics, but it is
        // not the current link. A server restart cannot inherit an envagent socket, and a
        // stale `connected = 1` row otherwise makes health/settings claim reachability
        // while every command correctly fails against the empty lease map. The lease is
        // the only live fact; persistence supplies identity and the last inventory.
        let leases = self.leases.read().await;
        let mut rows = Vec::new();
        for row in self.store.list_environment_rows().await? {
            let connected = leases.contains_key(&row.id);
            let agents = serde_json::from_str(&row.agents_json)
                .map_err(|error| anyhow!("invalid agent inventory in journal: {error}"))?;
            let facts = serde_json::from_str(&row.facts_json)
                .map_err(|error| anyhow!("invalid environment facts in journal: {error}"))?;
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

    /// Serialize commands that affect one long-lived remote session.  In
    /// particular, a `Send` and a turn-scoped stop must not cross on the
    /// connection: otherwise the stop can win the database race while the
    /// envagent still starts the cancelled input.
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
        self.remote_drafts
            .lock()
            .await
            .get(draft_id)
            .filter(|draft| !draft.promoting)
            .cloned()
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

    /// Return the per-draft promotion mutex.  The guard is held by the HTTP
    /// promotion handler across the envagent command and canonical row writes;
    /// this is the server-side serialization point for concurrent first-turn
    /// requests.
    pub async fn remote_promotion_lock(&self, draft_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.remote_promotion_locks.lock().await;
        locks
            .entry(draft_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn remote_promotion_result(&self, draft_id: &str) -> Option<RemotePromotionResult> {
        self.remote_promotion_results
            .lock()
            .await
            .get(draft_id)
            .cloned()
    }

    pub async fn remember_remote_promotion(&self, draft_id: &str, result: RemotePromotionResult) {
        self.remote_promotion_results
            .lock()
            .await
            .insert(draft_id.to_string(), result);
    }

    /// Claim a draft for promotion so concurrent first-turn requests cannot
    /// dispatch the same non-idempotent vendor input twice.
    pub async fn claim_remote_draft(
        &self,
        draft_id: &str,
        proposed_turn_id: String,
    ) -> RemoteDraftClaim {
        let mut drafts = self.remote_drafts.lock().await;
        let Some(draft) = drafts.get_mut(draft_id) else {
            return RemoteDraftClaim::Missing;
        };
        if draft.promoting {
            // A caller that did not take the per-draft lock must never be
            // allowed to dispatch the same first turn twice.  The normal HTTP
            // path waits on that lock and then reads the cached result.
            return RemoteDraftClaim::Busy;
        }
        if let Some(existing) = draft.first_turn_id.as_ref() {
            if existing != &proposed_turn_id {
                return RemoteDraftClaim::Conflict {
                    turn_id: existing.clone(),
                };
            }
        }
        let turn_id = draft.first_turn_id.clone().unwrap_or(proposed_turn_id);
        draft.promoting = true;
        draft.first_turn_id = Some(turn_id.clone());
        RemoteDraftClaim::Claimed {
            draft: Box::new(draft.clone()),
            turn_id,
        }
    }

    pub async fn release_remote_draft_claim(&self, draft_id: &str) {
        if let Some(draft) = self.remote_drafts.lock().await.get_mut(draft_id) {
            draft.promoting = false;
            // No canonical session exists when a promotion is released.  The
            // next attempt must be able to choose its own idempotency scope;
            // retaining this provisional id would turn every later retry into
            // a permanent conflict after a transient envagent/DB failure.
            draft.first_turn_id = None;
        }
    }

    pub async fn remove_remote_draft(&self, draft_id: &str) -> Option<RemoteDraft> {
        let draft = self.remote_drafts.lock().await.remove(draft_id);
        if draft.is_some() {
            // Cleanup is best effort: the server must forget the draft even when
            // the envagent went offline, while a connected agent should also
            // release the process it owns.
            if let Some(draft) = draft.as_ref() {
                if self.environment_connected(&draft.environment).await {
                    if let Ok(request_id) =
                        sylph_protocol::CommandRequestId::parse(format!("close-draft-{draft_id}"))
                    {
                        let _ = self
                            .dispatch(
                                &draft.environment,
                                Command::new(
                                    request_id,
                                    IdempotencyPolicy::Replayable,
                                    sylph_protocol::EnvironmentCommand::DraftClose {
                                        draft_id: draft_id.to_string(),
                                    },
                                ),
                            )
                            .await;
                    }
                }
            }
        }
        // The result cache is retained for idempotent retries, but the lock
        // itself is no longer needed once the draft has been consumed.  A
        // caller holding the lock is necessarily in this method's caller and
        // therefore cannot race this removal.
        self.remote_promotion_locks.lock().await.remove(draft_id);
        draft
    }

    /// Forget server-side draft metadata without issuing a remote close. Used
    /// only after promotion and activation have succeeded; the envagent then
    /// owns a canonical session and must not receive a late DraftClose.
    pub async fn forget_remote_draft(&self, draft_id: &str) -> Option<RemoteDraft> {
        let draft = self.remote_drafts.lock().await.remove(draft_id);
        self.remote_promotion_locks.lock().await.remove(draft_id);
        draft
    }

    /// Discard an unpromoted remote draft and ask the envagent to release its
    /// prepared process. This is the destructive draft path used by DELETE and
    /// compensation after a failed first-turn request.
    pub async fn discard_remote_draft(&self, draft_id: &str) -> Option<RemoteDraft> {
        self.remove_remote_draft(draft_id).await
    }

    pub async fn activate_remote(&self, session_id: &sylph_protocol::SessionId) -> Result<()> {
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "activate-{}",
            uuid::Uuid::new_v4().simple()
        ))?;
        let outcome = self
            .dispatch_and_wait(
                &session_id.environment,
                Command::new(
                    request_id,
                    IdempotencyPolicy::Replayable,
                    sylph_protocol::EnvironmentCommand::Activate {
                        session_id: session_id.clone(),
                    },
                ),
                std::time::Duration::from_secs(10),
            )
            .await?;
        match outcome {
            CommandOutcome::Accepted { .. } => Ok(()),
            CommandOutcome::Rejected { code, message } => {
                Err(RemoteCommandError::rejected(code, message).into())
            }
            CommandOutcome::Unknown { message } => Err(anyhow!(
                "remote activation outcome is unknown: {}",
                message.unwrap_or_default()
            )),
        }
    }

    async fn dispatch_remote_operation(
        &self,
        environment: &EnvironmentId,
        command: sylph_protocol::EnvironmentCommand,
        idempotency: IdempotencyPolicy,
    ) -> Result<CommandOutcome> {
        let request_id = remote_request_id(environment, &command, idempotency)?;
        self.dispatch_and_wait(
            environment,
            Command::new(request_id, idempotency, command),
            std::time::Duration::from_secs(30),
        )
        .await
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
            None => sylph_protocol::EnvironmentCommand::Files {
                session_id: session_id.ok_or_else(|| anyhow!("missing remote session id"))?,
                operation,
            },
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

    /// Browse an environment's machine-local roots or one directory level.
    /// The envagent owns path policy and filesystem access; the server only
    /// dispatches the typed command and returns its JSON result.
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

    /// Dispatch a session-scoped command and normalize its result to a JSON
    /// value for the HTTP layer.  The bridge remains the only place that knows
    /// how a command crosses an environment lease.
    pub async fn remote_session_command(
        &self,
        session_id: &sylph_protocol::SessionId,
        command: sylph_protocol::EnvironmentCommand,
        idempotency: IdempotencyPolicy,
    ) -> Result<CommandOutcome> {
        // Every command routed here wants a fresh request id, so this asks no
        // classification question and must not grow one. The reads it carries
        // (`Capabilities`, `Close`) are already classified as repeatable by
        // `command_needs_a_fresh_request_id`; the writes
        // (`Interrupt`, `Steer`, `Queue`, `SetConfig`, `Resolve`) are each a
        // distinct user action, so hashing their payload would make sending one
        // steer text twice — or setting a config back to a value it already held —
        // collide with the earlier call and replay its stored result instead of
        // reaching the envagent. That is the frozen-surface defect again, and
        // `repeating_one_steer_text_dispatches_a_second_command` pins it.
        //
        // At-most-once protection for these does not come from the id: a retry
        // that arrives while the original is in flight joins its waiter in
        // `dispatch_and_wait`, and `remote_promotion_locks` serializes promotion.
        let request_id = sylph_protocol::CommandRequestId::parse(format!(
            "remote-{}-{}-{}",
            session_id.environment,
            command_name(&command),
            uuid::Uuid::new_v4().simple()
        ))?;
        self.dispatch_and_wait(
            &session_id.environment,
            Command::new(request_id, idempotency, command),
            std::time::Duration::from_secs(30),
        )
        .await
    }

    /// Re-establish a durable remote session after a server or envagent
    /// restart.  The envagent applies path policy to the workdir it receives;
    /// the server never canonicalizes a path belonging to another machine.
    pub async fn remote_attach(&self, session: &crate::models::Session) -> Result<CommandOutcome> {
        let flags = serde_json::from_str::<serde_json::Value>(&session.flags_json)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        self.remote_session_command(
            &session.id,
            sylph_protocol::EnvironmentCommand::Run {
                session_id: Some(session.id.clone()),
                draft_id: None,
                agent: session.id.agent.to_string(),
                workdir: session
                    .workdir
                    .clone()
                    .ok_or_else(|| anyhow!("session has no recorded working directory"))?,
                flags,
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

    /// Start a remote agent session and retain the server-side draft metadata.
    /// The envagent validates the workdir on its own machine before spawning.
    #[allow(clippy::too_many_arguments)]
    pub async fn prepare_remote(
        &self,
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
        let request_id = sylph_protocol::CommandRequestId::parse(format!("prepare-{draft_id}"))?;
        let outcome = self
            .dispatch_and_wait(
                environment,
                Command::new(
                    request_id,
                    IdempotencyPolicy::AtMostOnce,
                    sylph_protocol::EnvironmentCommand::Prepare {
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
                )))
            }
            CommandOutcome::Unknown { message } => {
                return Err(anyhow::Error::new(RemoteCommandError::Unknown {
                    message: message.unwrap_or_default(),
                }))
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
                first_turn_id: None,
            },
        );
        Ok((draft_id, contained_workdir))
    }

    pub async fn run_remote_initial(
        &self,
        draft: &RemoteDraft,
        turn_id: &str,
        input: sylph_protocol::WireTurnInput,
    ) -> Result<(sylph_protocol::SessionId, String)> {
        let request_id =
            sylph_protocol::CommandRequestId::parse(format!("remote-run-{}", draft.draft_id))?;
        let outcome = self
            .dispatch_and_wait(
                &draft.environment,
                Command::new(
                    request_id,
                    IdempotencyPolicy::AtMostOnce,
                    sylph_protocol::EnvironmentCommand::Run {
                        session_id: None,
                        draft_id: Some(draft.draft_id.clone()),
                        agent: draft.agent.clone(),
                        workdir: draft.workdir.clone(),
                        flags: draft.flags.clone().into_iter().collect(),
                        inject_identity: draft.inject_identity,
                        identity_preamble: draft.identity_preamble.clone(),
                        permission_mode: draft.permission_mode.clone(),
                        initial_input: Some(input),
                        initial_turn_id: Some(turn_id.to_string()),
                    },
                ),
                std::time::Duration::from_secs(30),
            )
            .await?;
        let result = match outcome {
            CommandOutcome::Accepted {
                result: Some(result),
            } => result,
            CommandOutcome::Accepted { result: None } => {
                return Err(anyhow!("remote run returned no metadata"))
            }
            CommandOutcome::Rejected { code, message } => {
                return Err(anyhow!("remote run rejected ({code}): {message}"))
            }
            CommandOutcome::Unknown { message } => {
                return Err(anyhow!(
                    "remote run outcome is unknown: {}",
                    message.unwrap_or_default()
                ))
            }
        };
        let session_id: sylph_protocol::SessionId = serde_json::from_value(
            result
                .get("sessionId")
                .cloned()
                .ok_or_else(|| anyhow!("remote run returned no session id"))?,
        )?;
        if session_id.environment != draft.environment {
            return Err(BridgeError::ProducerMismatch.into());
        }
        let workdir = result
            .get("workdir")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("remote run returned no contained workdir"))?
            .to_string();
        Ok((session_id, workdir))
    }

    /// Authenticate and lease one active connection for an installation.
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
        self.leases.write().await.insert(
            environment.clone(),
            Lease {
                epoch,
                sender,
                producer_id: hello.producer_id.clone(),
            },
        );
        let mut welcome = sylph_protocol::Welcome::current(environment, epoch, self.server_epoch);
        welcome.version = version;
        let accepted = self
            .store
            .env_accepted_through(&welcome.environment_id, &hello.producer_id)
            .await?;
        welcome
            .accepted_cursors
            .insert(hello.producer_id.to_string(), ProducerSeq::new(accepted));
        Ok(welcome)
    }

    pub async fn disconnect(&self, environment: &EnvironmentId, epoch: u64) -> Result<()> {
        let actual_epoch = {
            let mut leases = self.leases.write().await;
            match leases.get(environment) {
                Some(lease) if lease.epoch == epoch => {
                    leases.remove(environment);
                    Some(epoch)
                }
                Some(_) => None,
                None => Some(epoch),
            }
        };
        let Some(actual_epoch) = actual_epoch else {
            return Err(BridgeError::StaleEpoch.into());
        };
        if let Some(row) = self.store.environment_row(environment).await? {
            let _ = self
                .store
                .clear_environment_connection_if_epoch(environment, &row.token_hash, actual_epoch)
                .await?;
        }
        let pending: Vec<(String, IdempotencyPolicy)> = self
            .command_policies
            .read()
            .await
            .iter()
            .filter(|((env, _), _)| env == environment)
            .map(|((_, request_id), policy)| (request_id.clone(), *policy))
            .collect();
        for (request_id, policy) in pending {
            let unknown = CommandOutcome::Unknown {
                message: Some("connection closed before command result".into()),
            };
            let _ = self
                .store
                .record_command_outcome(environment, &request_id, policy, &unknown)
                .await;
            if let Some(waiters) = self
                .command_waiters
                .lock()
                .await
                .remove(&(environment.clone(), request_id.clone()))
            {
                for waiter in waiters {
                    let _ = waiter.sender.send(unknown.clone());
                }
            }
        }
        // Requests are server resources; announce every row whose agent died so
        // attached clients retire stale approval panels immediately.
        let sessions = self
            .store
            .list_sessions(&crate::db::store::SessionFilter {
                environment: Some(environment.to_string()),
                limit: 10000,
                ..Default::default()
            })
            .await
            .unwrap_or_default();
        for session in sessions {
            if let Ok(rows) = self.store.abandon_open_requests(&session.id).await {
                for row in rows {
                    self.hub
                        .publish_session(
                            &session.id.encode(),
                            crate::events::StreamEvent {
                                seq: 0,
                                arrival_turn_id: None,
                                kind: "request".into(),
                                phase: None,
                                stream_id: None,
                                span: None,
                                interior_state: None,
                                name: None,
                                text: String::new(),
                                data: Some(serde_json::to_value(row).unwrap_or_default()),
                                is_error: false,
                                stability: "stable".into(),
                                source: "server".into(),
                                arrival_at_ms: now_ms(),
                                recorded_at_ms: None,
                                created_at_ms: now_ms(),
                                started_at_ms: None,
                            },
                        )
                        .await;
                }
            }
        }
        let _ = self.registry.disconnect(environment, epoch).await;
        Ok(())
    }

    pub async fn unpair(&self, environment: &EnvironmentId) -> Result<bool> {
        self.leases.write().await.remove(environment);
        let removed = self.store.delete_environment(environment).await?;
        let _ = self.registry.unpair(environment).await;
        Ok(removed)
    }

    /// Ingest a bounded batch.  Receipt insertion and event insertion share
    /// one database transaction, so a receipt can never claim an event that was not
    /// written; the ingest gate serializes sequence allocation across every producer
    /// touching one session, which is what keeps `UNIQUE(session, seq)` from refusing a
    /// write when a live turn and an adoption are both appending.
    pub async fn ingest_batch(&self, batch: EventBatch) -> Result<IngestResult> {
        batch
            .validate()
            .map_err(|error| BridgeError::InvalidBatch(error.to_string()))?;
        let environment = batch.environment_id.clone();
        let lease = self
            .leases
            .read()
            .await
            .get(&environment)
            .map(|lease| (lease.epoch, lease.producer_id.clone()))
            .ok_or(BridgeError::NotConnected)?;
        if lease.0 != batch.connection_epoch {
            return Err(BridgeError::StaleEpoch.into());
        }
        if lease.1 != batch.producer_id {
            return Err(BridgeError::ProducerMismatch.into());
        }
        let _gate = self.ingest_gate.lock().await;
        // Remote events derive their canonical sequence from SQLite. Hold the
        // allocator gate through that transaction and cursor update so a
        // concurrent server-authored frame cannot reserve the same number.
        let seq_gate = self.seq.gate();
        let _seq_gate = seq_gate.lock().await;
        let mut terminal_items: Vec<(sylph_protocol::SessionId, String, sylph_protocol::Event)> =
            Vec::new();
        let mut history_next_seq: std::collections::HashMap<String, i32> =
            std::collections::HashMap::new();
        let mut accepted_through = self
            .store
            .env_accepted_through(&environment, &batch.producer_id)
            .await?;
        if batch.first_seq().get() > accepted_through.saturating_add(1) {
            return Ok(IngestResult::Nack {
                producer_id: batch.producer_id,
                expected_next: ProducerSeq::new(accepted_through.saturating_add(1)),
                reason: "producer sequence gap".into(),
            });
        }
        let mut last_server = None;
        for item in &batch.events {
            let seq = item.producer_seq.get();
            let payload_hash = hash_bytes(&serde_json::to_vec(item)?);
            if seq > accepted_through.saturating_add(1) {
                return Ok(IngestResult::Nack {
                    producer_id: batch.producer_id,
                    expected_next: ProducerSeq::new(accepted_through.saturating_add(1)),
                    reason: "producer sequence gap".into(),
                });
            }
            if seq <= accepted_through {
                // A replayed item is accepted only if the receipt exists.
                let Some(receipt) = self
                    .store
                    .env_receipt_detail(&environment, &batch.producer_id, item.producer_seq)
                    .await?
                else {
                    return Ok(IngestResult::Nack {
                        producer_id: batch.producer_id.clone(),
                        expected_next: ProducerSeq::new(accepted_through),
                        reason: "duplicate sequence has no durable receipt".into(),
                    });
                };
                if receipt.payload_hash != payload_hash {
                    return Ok(IngestResult::Nack {
                        producer_id: batch.producer_id.clone(),
                        expected_next: ProducerSeq::new(seq),
                        reason: "duplicate sequence carries different payload".into(),
                    });
                }
                last_server = Some(ServerSeq::new(receipt.server_seq));
                let replay_turn_id = match item.arrival_turn_id.clone() {
                    Some(turn_id) => Some(turn_id),
                    None => self
                        .remote_source_turns
                        .lock()
                        .await
                        .get(&item.session_id)
                        .cloned(),
                };
                if let Some(turn_id) = replay_turn_id.as_deref() {
                    if item.event.kind == sylph_protocol::EventKind::Turn
                        && item.event.phase == Some(sylph_protocol::Phase::Completed)
                    {
                        terminal_items.push((
                            item.session_id.clone(),
                            turn_id.to_string(),
                            item.event.clone(),
                        ));
                    }
                }
                continue;
            }

            let mut row = StreamEvent::from_protocol(&item.event).to_row(&item.session_id, "");
            // An envagent supplies arrival provenance, but the server still owns
            // the turn namespace.  Never let a malformed or cross-session turn id
            // settle (or appear to belong to) another session.
            let arrival_turn_id = match item.arrival_turn_id.as_deref() {
                Some(turn_id) => match self.store.get_turn(turn_id).await? {
                    Some(turn) if turn.session_id == item.session_id => Some(turn_id.to_string()),
                    _ => None,
                },
                None => self
                    .remote_source_turns
                    .lock()
                    .await
                    .get(&item.session_id)
                    .cloned(),
            };
            let arrival_turn_id = if let Some(turn_id) = arrival_turn_id {
                Some(turn_id)
            } else {
                Some(
                    self.ensure_remote_source_turn(&item.session_id, row.created_at_ms)
                        .await?,
                )
            };
            row.turn_id = arrival_turn_id.clone();
            let session_key = item.session_id.encode();
            let min_next_seq = if let Some(next) = history_next_seq.get(&session_key).copied() {
                next
            } else {
                let next = self
                    .history
                    .next_seq(&session_key)
                    .await
                    .map_err(|error| anyhow!(error))?;
                history_next_seq.insert(session_key, next);
                next
            };
            let ingest = self
                .store
                .ingest_env_event_auto(
                    &environment,
                    &batch.producer_id,
                    item.producer_seq,
                    &mut row,
                    &payload_hash,
                    min_next_seq,
                )
                .await;
            let ingest = match ingest {
                Ok(value) => value,
                Err(error) => {
                    self.seq.reset_session(&item.session_id).await;
                    return Err(error.into());
                }
            };
            match ingest.result {
                EnvIngestResult::Inserted => {
                    // Remote ingest allocates the canonical sequence in the same
                    // transaction as the event row. Keep server-authored producers'
                    // in-memory allocator above that value, or the next status/error
                    // frame can reuse the sequence and hit the journal's uniqueness
                    // constraint.
                    if let Ok(next_seq) = i32::try_from(ingest.server_seq.get().saturating_add(1)) {
                        self.seq.observe_inner(&item.session_id, next_seq).await;
                        history_next_seq.insert(item.session_id.encode(), next_seq);
                    }
                    let published = StreamEvent::from_row(&row);
                    if let Some(turn_id) = arrival_turn_id.as_deref() {
                        self.hub.publish(turn_id, published.clone()).await;
                    }
                    self.hub
                        .publish_trace(&row.session_id.encode(), published)
                        .await;
                    // Remote envagent ingest owns its own drain, so
                    // mirror the live path's late trajectory reconciliation here.
                    // The event is still retained on the arriving turn; the
                    // reconciler amends the earlier provisional record in place.
                    if item.event.kind == sylph_protocol::EventKind::Subagent
                        && item.event.phase == Some(sylph_protocol::Phase::Completed)
                    {
                        if let Some(stream_id) = item.event.stream_id.as_deref() {
                            if let Some(reconciler) =
                                self.trajectory_reconciler.read().await.clone()
                            {
                                let outcome = TrajectoryOutcome {
                                    text: &item.event.text,
                                    is_error: item.event.is_error,
                                    reported_on_turn_id: arrival_turn_id
                                        .as_deref()
                                        .unwrap_or_default(),
                                };
                                let _ = reconciler
                                    .record(
                                        &item.session_id,
                                        stream_id,
                                        arrival_turn_id.as_deref().unwrap_or_default(),
                                        &outcome,
                                    )
                                    .await;
                            }
                        }
                    }
                    if let Some(turn_id) = arrival_turn_id.as_deref() {
                        if item.event.kind == sylph_protocol::EventKind::Turn
                            && item.event.phase == Some(sylph_protocol::Phase::Completed)
                        {
                            terminal_items.push((
                                item.session_id.clone(),
                                turn_id.to_string(),
                                item.event.clone(),
                            ));
                            self.remote_source_turns
                                .lock()
                                .await
                                .remove(&item.session_id);
                        }
                    }
                    last_server = Some(ingest.server_seq);
                }
                EnvIngestResult::Duplicate { .. } => {
                    last_server = Some(ingest.server_seq);
                }
            }
            accepted_through = seq;
        }
        drop(_gate);
        drop(_seq_gate);
        if let Some(closer) = self.turn_closer.read().await.clone() {
            for (session_id, turn_id, event) in terminal_items {
                let stop_reason = event
                    .data
                    .as_ref()
                    .and_then(|data| data.get("stopReason"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let outcome = if event.is_error {
                    Outcome::Failed {
                        message: if event.text.is_empty() {
                            "The agent reported a failure".to_string()
                        } else {
                            event.text.clone()
                        },
                    }
                } else if stop_reason.as_deref() == Some("interrupted")
                    || stop_reason.as_deref() == Some("aborted")
                {
                    Outcome::Interrupted
                } else {
                    Outcome::Completed { stop_reason }
                };
                closer.close(&session_id, &turn_id, outcome).await;
            }
        }
        Ok(IngestResult::Ack(Ack {
            producer_id: batch.producer_id,
            accepted_through: ProducerSeq::new(accepted_through),
            server_seq_through: last_server,
        }))
    }

    async fn ensure_remote_source_turn(
        &self,
        session_id: &sylph_protocol::SessionId,
        at_ms: i64,
    ) -> Result<String> {
        if let Some(id) = self
            .remote_source_turns
            .lock()
            .await
            .get(session_id)
            .cloned()
        {
            return Ok(id);
        }
        if let Some(id) = self.store.running_turn_id(session_id).await? {
            self.remote_source_turns
                .lock()
                .await
                .insert(session_id.clone(), id.clone());
            return Ok(id);
        }
        let turn = crate::models::Turn {
            id: format!("remote-source-turn-{}", uuid::Uuid::new_v4().simple()),
            session_id: session_id.clone(),
            status: "running".into(),
            // No HTTP request opened this turn: it is the remote agent speaking
            // after the prior turn settled, so preserve that provenance in the
            // durable turn row just as the live link path does.
            origin: "agent".into(),
            origin_subject: None,
            request_text: String::new(),
            command_invocation_id: None,
            idempotency_key: None,
            error_code: None,
            error_message: None,
            close_reason: None,
            created_at_ms: at_ms,
            started_at_ms: Some(at_ms),
            completed_at_ms: None,
            stopped_at_ms: None,
        };
        let id = self.store.create_turn(&turn).await?.id;
        self.remote_source_turns
            .lock()
            .await
            .insert(session_id.clone(), id.clone());
        let _ = self
            .store
            .update_session_status(session_id, "running")
            .await;
        Ok(id)
    }

    async fn validate_producer_frame(
        &self,
        environment: &EnvironmentId,
        expected_epoch: u64,
        frame_epoch: u64,
        producer_id: &ProducerId,
    ) -> Result<()> {
        if frame_epoch != expected_epoch {
            return Err(BridgeError::StaleEpoch.into());
        }
        let lease = self
            .leases
            .read()
            .await
            .get(environment)
            .map(|lease| (lease.epoch, lease.producer_id.clone()))
            .ok_or(BridgeError::NotConnected)?;
        if lease.0 != expected_epoch {
            return Err(BridgeError::StaleEpoch.into());
        }
        if &lease.1 != producer_id {
            return Err(BridgeError::ProducerMismatch.into());
        }
        Ok(())
    }

    /// Run one sequenced non-event message through the same contiguous-prefix
    /// gate as event batches.  The receipt/hash check happens before the
    /// callback, so an exact replay cannot publish or mutate state twice.  The
    /// gate remains held while the callback and receipt insertion run; this is
    /// intentionally serialized because these messages often update state in a
    /// different table than the receipt table and SQLite cannot make that pair
    /// one transaction for every sink (notably the warm history store).
    #[allow(clippy::too_many_arguments)]
    async fn process_message<F, Fut>(
        &self,
        environment: &EnvironmentId,
        epoch: u64,
        producer_id: &ProducerId,
        producer_seq: ProducerSeq,
        payload_hash: &str,
        tx: &mpsc::Sender<WireMessage>,
        effect: F,
    ) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        self.validate_producer_frame(environment, epoch, epoch, producer_id)
            .await?;
        let _gate = self.ingest_gate.lock().await;
        let accepted_before = self
            .store
            .env_accepted_through(environment, producer_id)
            .await?;
        if producer_seq.get() > accepted_before.saturating_add(1) {
            tx.send(WireMessage::Nack {
                producer_id: producer_id.clone(),
                expected_next: ProducerSeq::new(accepted_before.saturating_add(1)),
                reason: "producer sequence gap".into(),
            })
            .await
            .map_err(|_| anyhow!("envagent writer closed"))?;
            return Ok(());
        }
        if let Some(existing) = self
            .store
            .env_receipt_detail(environment, producer_id, producer_seq)
            .await?
        {
            if existing.payload_hash != payload_hash {
                return Err(BridgeError::InvalidBatch(
                    "producer sequence was replayed with different payload".into(),
                )
                .into());
            }
            tx.send(WireMessage::Ack {
                producer_id: producer_id.clone(),
                accepted_through: ProducerSeq::new(accepted_before),
                server_seq_through: None,
            })
            .await
            .map_err(|_| anyhow!("envagent writer closed"))?;
            return Ok(());
        }

        effect().await?;
        self.store
            .insert_env_message_receipt(environment, producer_id, producer_seq, payload_hash)
            .await?;
        let accepted_through = self
            .store
            .env_accepted_through(environment, producer_id)
            .await?;
        tx.send(WireMessage::Ack {
            producer_id: producer_id.clone(),
            accepted_through: ProducerSeq::new(accepted_through),
            server_seq_through: None,
        })
        .await
        .map_err(|_| anyhow!("envagent writer closed"))?;
        Ok(())
    }

    /// Dispatch a command while respecting the durable ambiguity record.
    pub async fn dispatch(
        &self,
        environment: &EnvironmentId,
        command: Command,
    ) -> Result<DispatchResult> {
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

    pub async fn record_command_result(
        &self,
        environment: &EnvironmentId,
        result: &sylph_protocol::CommandResultMessage,
    ) -> Result<()> {
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
}

fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
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

async fn take_command_waiters(
    waiters: &CommandWaiters,
    key: &(EnvironmentId, String),
) -> Vec<CommandWaiter> {
    waiters.lock().await.remove(key).unwrap_or_default()
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
        EnvironmentCommand::Activate { .. } => "activate",
        EnvironmentCommand::Close { .. } => "close",
        EnvironmentCommand::BrowseRoots => "browse-roots",
        EnvironmentCommand::BrowseTree { .. } => "browse-tree",
    }
}

/// Whether each call of this command is a *new* operation, so it must not
/// address one frozen durable result in `env_command_results`.
///
/// This is deliberately **not** [`IdempotencyPolicy::Replayable`], and the two
/// must not be derived from each other even though they read as synonyms.
/// `Replayable` answers a wire question that travels with the command — may the
/// envagent be sent this same request id again after an ambiguous crash. This
/// answers a server question — does a second call want a second answer.
/// `Replayable` is the strict superset, and `Run` is the witness: `remote_attach`
/// is replayable precisely *because* a retry must address the original command
/// rather than start a second native session, which is the opposite of what a
/// read wants. Collapsing the two would make one of those cases wrong.
///
/// The trap is that the naming invites an omission rather than a mismatch: a new
/// read-only command reaches this file, gets `Replayable` at its call site, and
/// nothing fails if it is never listed here — it just answers every later caller
/// with its first answer. `DraftCapabilities` and `DraftFiles` both shipped that
/// way, each carrying the same omission as its durable-session counterpart.
fn command_needs_a_fresh_request_id(command: &sylph_protocol::EnvironmentCommand) -> bool {
    use sylph_protocol::{EnvironmentCommand, FileOperation};
    match command {
        // A capability surface is a live reading of an agent's state, so a read
        // after a config change must observe it. Omitting the draft form here is
        // what made picking a model or a thinking level appear to do nothing: a
        // draft's first read happens before any config is set, and that empty
        // snapshot then answered every later read for the draft's whole life.
        EnvironmentCommand::Capabilities { .. } | EnvironmentCommand::DraftCapabilities { .. } => {
            true
        }
        // A listing after mkdir must observe the new entry. Mutating file
        // operations are excluded: they carry their payload in the request id so
        // an at-most-once retry addresses the original write.
        EnvironmentCommand::Files { operation, .. }
        | EnvironmentCommand::DraftFiles { operation, .. } => matches!(
            operation,
            FileOperation::List { .. } | FileOperation::Read { .. } | FileOperation::Stat { .. }
        ),
        // Not a read, but repeatable for the same reason: a session can be
        // closed, reattached, and closed again, and the payload is only its id —
        // so a payload-derived id would replay the first close's result and the
        // second close would never reach the envagent.
        EnvironmentCommand::Close { .. } => true,
        // **The case that looks safest and is not.** A re-read touches a file and
        // writes no vendor state, so it reads as a question — but its whole point
        // is to observe a store that has changed since it was last asked, and its
        // payload is only the session id, identical on every call. A
        // payload-derived id would make the second click address the first pass's
        // result and report counts taken before the bytes it was clicked to
        // collect existed: a "refresh" answering from cache, which is the very
        // defect the control exists to fix, one layer down.
        EnvironmentCommand::Reread { .. } => true,
        _ => false,
    }
}

fn remote_request_id(
    environment: &EnvironmentId,
    command: &sylph_protocol::EnvironmentCommand,
    _idempotency: IdempotencyPolicy,
) -> Result<sylph_protocol::CommandRequestId> {
    // The command payload itself is the idempotency scope.  Hashing it keeps
    // request ids bounded even for large file writes, while making retries of
    // the same operation address the original durable result and changing a
    // config value or file payload address a new operation.
    // A command whose every call is a new operation must not reuse one cached
    // result forever, so give it a fresh request id; its retry safety comes from
    // the operation itself. Mutating commands retain payload-derived ids for
    // at-most-once semantics.
    if command_needs_a_fresh_request_id(command) {
        return sylph_protocol::CommandRequestId::parse(format!(
            "remote-{}-{}-{}",
            environment,
            command_name(command),
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|error| anyhow!(error));
    }
    let payload = serde_json::to_vec(command).map_err(|error| anyhow!(error))?;
    let suffix = hash_bytes(&payload);
    sylph_protocol::CommandRequestId::parse(format!(
        "remote-{}-{}-{}",
        environment,
        command_name(command),
        suffix
    ))
    .map_err(|error| anyhow!(error))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
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

async fn mint_environment_id(store: &JournalStore) -> Result<EnvironmentId> {
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

const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Session;
    use sylph_protocol::{AgentName, EnvironmentId, Event, EventKind, Phase, SessionId};

    async fn bridge() -> (EnvBridge, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .unwrap();
        let hub = EventHub::new();
        let seq = SeqAllocator::new(store.clone());
        let history = crate::storage::HistoryStore::open(
            dir.path().join("history.db").to_str().unwrap(),
            crate::storage::cas::BlobStore::new(dir.path().join("blobs")),
        )
        .await
        .unwrap();
        let bridge = EnvBridge::new(store, history, seq, hub, EnvironmentRegistry::new());
        (bridge, dir)
    }

    fn session(native: &str) -> SessionId {
        SessionId::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            AgentName::parse("claude").unwrap(),
            sylph_protocol::NativeId::new(native),
        )
    }

    #[tokio::test]
    async fn pairing_is_persisted_and_a_code_can_be_consumed_once() {
        let (bridge, _dir) = bridge().await;
        let offer = bridge.create_pairing().await.unwrap();
        let grant = bridge
            .claim_pairing(&offer.code, "test", "development", Vec::new())
            .await
            .unwrap();
        assert!(grant.environment_id.as_str().len() > 10);
        assert!(bridge
            .claim_pairing(&offer.code, "again", "development", Vec::new())
            .await
            .is_err());
        let rows = bridge.list_environments().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].connected);
    }

    #[tokio::test]
    async fn ingest_assigns_server_sequences_and_deduplicates_replays() {
        let (bridge, _dir) = bridge().await;
        let id = session("n1");
        bridge
            .store
            .create_session(&Session {
                id: id.clone(),
                workdir: Some("/tmp".into()),
                title: None,
                inject_identity: false,
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".into(),
                status: "idle".into(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(4);
        let hello = sylph_protocol::Hello::current(
            "install",
            ProducerId::parse("producer").unwrap(),
            Some(EnvironmentId::parse("env7fk2").unwrap()),
            0,
        );
        // Pairing persistence is bypassed here by inserting a fixture installation.
        bridge
            .store
            .upsert_environment(
                &EnvironmentId::parse("env7fk2").unwrap(),
                "install",
                "fixture",
                "development",
                &hash_secret("token"),
                "[]",
            )
            .await
            .unwrap();
        let welcome = bridge
            .register_connection(&hello, "token", tx)
            .await
            .unwrap();
        let event = Event::new(EventKind::Assistant, Some(Phase::Completed), 10);
        let item = sylph_protocol::SequencedEvent::pending(
            ProducerId::parse("producer").unwrap(),
            ProducerSeq::new(1),
            id.clone(),
            event,
            None,
        );
        let batch = EventBatch::new(
            EnvironmentId::parse("env7fk2").unwrap(),
            ProducerId::parse("producer").unwrap(),
            welcome.connection_epoch,
            vec![item.clone()],
        )
        .unwrap();
        assert!(matches!(
            bridge.ingest_batch(batch.clone()).await.unwrap(),
            IngestResult::Ack(_)
        ));
        assert!(matches!(
            bridge.ingest_batch(batch).await.unwrap(),
            IngestResult::Ack(_)
        ));
        assert_eq!(bridge.store.max_event_seq(&id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_gap_is_nacked_before_any_event_is_written() {
        let (bridge, _dir) = bridge().await;
        bridge
            .store
            .upsert_environment(
                &EnvironmentId::parse("env7fk2").unwrap(),
                "install",
                "fixture",
                "development",
                &hash_secret("token"),
                "[]",
            )
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(4);
        let hello = sylph_protocol::Hello::current(
            "install",
            ProducerId::parse("producer").unwrap(),
            Some(EnvironmentId::parse("env7fk2").unwrap()),
            0,
        );
        let welcome = bridge
            .register_connection(&hello, "token", tx)
            .await
            .unwrap();
        let item = sylph_protocol::SequencedEvent::pending(
            ProducerId::parse("producer").unwrap(),
            ProducerSeq::new(3),
            session("n1"),
            Event::new(EventKind::Status, None, 1),
            None,
        );
        let result = bridge
            .ingest_batch(
                EventBatch::new(
                    EnvironmentId::parse("env7fk2").unwrap(),
                    ProducerId::parse("producer").unwrap(),
                    welcome.connection_epoch,
                    vec![item],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            matches!(result, IngestResult::Nack { expected_next, .. } if expected_next.get() == 1)
        );
    }

    #[tokio::test]
    async fn dispatch_and_wait_returns_the_persisted_command_result() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        bridge
            .store
            .upsert_environment(
                &environment,
                "install",
                "fixture",
                "development",
                &hash_secret("token"),
                "[]",
            )
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let hello = sylph_protocol::Hello::current(
            "install",
            ProducerId::parse("producer").unwrap(),
            Some(environment.clone()),
            0,
        );
        bridge
            .register_connection(&hello, "token", tx)
            .await
            .unwrap();

        let command = Command::new(
            sylph_protocol::CommandRequestId::parse("request-1").unwrap(),
            IdempotencyPolicy::Replayable,
            sylph_protocol::EnvironmentCommand::Health,
        );
        let waiter = {
            let bridge = bridge.clone();
            let environment = environment.clone();
            tokio::spawn(async move {
                bridge
                    .dispatch_and_wait(&environment, command, std::time::Duration::from_secs(1))
                    .await
            })
        };
        let WireMessage::Command {
            request_id,
            idempotency,
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("dispatch must send a command frame");
        };
        bridge
            .record_command_result(
                &environment,
                &sylph_protocol::CommandResultMessage {
                    request_id,
                    outcome: CommandOutcome::accepted(),
                    connection_epoch: 1,
                    idempotency: Some(idempotency),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            waiter.await.unwrap().unwrap(),
            CommandOutcome::Accepted { .. }
        ));
    }

    #[tokio::test]
    async fn concurrent_retries_share_one_command_result_and_one_wire_dispatch() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        bridge
            .store
            .upsert_environment(
                &environment,
                "install-concurrent",
                "fixture",
                "development",
                &hash_secret("concurrent-token"),
                "[]",
            )
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let hello = sylph_protocol::Hello::current(
            "install-concurrent",
            ProducerId::parse("producer-concurrent").unwrap(),
            Some(environment.clone()),
            0,
        );
        let welcome = bridge
            .register_connection(&hello, "concurrent-token", tx)
            .await
            .unwrap();
        let command = Command::new(
            sylph_protocol::CommandRequestId::parse("same-request").unwrap(),
            IdempotencyPolicy::AtMostOnce,
            sylph_protocol::EnvironmentCommand::Health,
        );
        let first = {
            let bridge = bridge.clone();
            let environment = environment.clone();
            let command = command.clone();
            tokio::spawn(async move {
                bridge
                    .dispatch_and_wait(&environment, command, std::time::Duration::from_secs(1))
                    .await
            })
        };
        let second = {
            let bridge = bridge.clone();
            let environment = environment.clone();
            tokio::spawn(async move {
                bridge
                    .dispatch_and_wait(&environment, command, std::time::Duration::from_secs(1))
                    .await
            })
        };
        let WireMessage::Command { request_id, .. } = rx.recv().await.unwrap() else {
            panic!("one command frame expected");
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
                .await
                .is_err(),
            "a concurrent retry must not dispatch a second frame"
        );
        bridge
            .record_command_result(
                &environment,
                &sylph_protocol::CommandResultMessage {
                    request_id,
                    outcome: CommandOutcome::accepted_with_result(serde_json::json!({
                        "ok": true
                    })),
                    connection_epoch: welcome.connection_epoch,
                    idempotency: Some(IdempotencyPolicy::AtMostOnce),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            first.await.unwrap().unwrap(),
            CommandOutcome::Accepted { .. }
        ));
        assert!(matches!(
            second.await.unwrap().unwrap(),
            CommandOutcome::Accepted { .. }
        ));
    }

    #[tokio::test]
    async fn a_message_ack_never_skips_an_unreceived_prefix() {
        // Drives `process_message` through the real wire (`serve_io`), the only path a
        // producer sequence actually reaches. A prior version of this test called a
        // second, test-only `ack_message` that had already diverged from production: on
        // this exact gap, `process_message` sends a `Nack`, not the `Ack` the dead copy
        // sent. Asserting on `serve_io`'s reply is asserting on what an envagent gets.
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let producer = ProducerId::parse("producer-message").unwrap();
        bridge
            .store
            .upsert_environment(
                &environment,
                "install-message",
                "fixture",
                "development",
                &hash_secret("message-token"),
                "[]",
            )
            .await
            .unwrap();

        let (mut client, server) = tokio::io::duplex(32 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server);
        let bridge_task = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.serve_io(server_reader, server_writer).await }
        });
        let hello = sylph_protocol::Hello::current(
            "install-message",
            producer.clone(),
            Some(environment.clone()),
            0,
        );
        write_frame(
            &mut client,
            &serde_json::to_vec(&WireMessage::from_hello(hello)).unwrap(),
        )
        .await
        .unwrap();
        write_frame(&mut client, &serde_json::to_vec("message-token").unwrap())
            .await
            .unwrap();
        let welcome =
            sylph_protocol::decode_message(&read_frame(&mut client).await.unwrap()).unwrap();
        let epoch = match welcome {
            DecodedMessage::Known(WireMessage::Welcome {
                connection_epoch, ..
            }) => connection_epoch,
            other => panic!("expected welcome, got {other:?}"),
        };

        // Producer sequence 3 with nothing accepted yet is a gap, not the next message.
        write_frame(
            &mut client,
            &serde_json::to_vec(&WireMessage::Diagnostic {
                level: sylph_protocol::DiagnosticLevel::Info,
                code: "probe".into(),
                message: "gap probe".into(),
                session_id: None,
                details: Default::default(),
                producer_id: producer.clone(),
                producer_seq: ProducerSeq::new(3),
                connection_epoch: epoch,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let reply =
            sylph_protocol::decode_message(&read_frame(&mut client).await.unwrap()).unwrap();
        assert!(
            matches!(
                reply,
                DecodedMessage::Known(WireMessage::Nack { expected_next, .. })
                    if expected_next.get() == 1
            ),
            "a gap must be nacked to the next unreceived sequence, not acked past it: {reply:?}"
        );

        drop(client);
        let _ = bridge_task.await;
    }

    #[tokio::test]
    async fn remote_terminal_event_settles_its_arrival_turn_and_replayed_terminal_is_idempotent() {
        let (bridge, dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let producer = ProducerId::parse("producer-terminal").unwrap();
        let session_id = session("remote-terminal");
        bridge
            .store
            .upsert_environment(
                &environment,
                "install-terminal",
                "fixture",
                "development",
                &hash_secret("terminal-token"),
                "[]",
            )
            .await
            .unwrap();
        bridge
            .store
            .create_session(&Session {
                id: session_id.clone(),
                workdir: Some(dir.path().display().to_string()),
                title: Some("remote".into()),
                inject_identity: false,
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".into(),
                status: "running".into(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        let turn_id = "remote-turn-1".to_string();
        bridge
            .store
            .create_turn(&crate::models::Turn {
                id: turn_id.clone(),
                session_id: session_id.clone(),
                status: "running".into(),
                origin: "user".into(),
                origin_subject: None,
                request_text: "hello".into(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                close_reason: None,
                created_at_ms: 1,
                started_at_ms: Some(1),
                completed_at_ms: None,
                stopped_at_ms: None,
            })
            .await
            .unwrap();

        let (tx, _rx) = mpsc::channel(8);
        let hello = sylph_protocol::Hello::current(
            "install-terminal",
            producer.clone(),
            Some(environment.clone()),
            0,
        );
        let welcome = bridge
            .register_connection(&hello, "terminal-token", tx)
            .await
            .unwrap();
        let mut terminal = Event::new(EventKind::Turn, Some(Phase::Completed), 2);
        terminal.data = Some(serde_json::json!({"stopReason": "completed"}));
        let mut item = sylph_protocol::SequencedEvent::pending(
            producer.clone(),
            ProducerSeq::new(1),
            session_id.clone(),
            terminal,
            None,
        );
        item.arrival_turn_id = Some(turn_id.clone());
        let batch =
            EventBatch::new(environment, producer, welcome.connection_epoch, vec![item]).unwrap();
        bridge.ingest_batch(batch.clone()).await.unwrap();
        let first = bridge.store.get_turn(&turn_id).await.unwrap().unwrap();
        assert_eq!(first.status, "running");

        let writer = crate::db::JournalWriter::new(bridge.store.clone());
        let consolidator =
            crate::storage::Consolidator::new(bridge.store.clone(), bridge.history.clone());
        bridge
            .set_turn_closer(crate::supervisor::TurnCloser::new(
                bridge.store.clone(),
                bridge.hub.clone(),
                bridge.seq.clone(),
                consolidator,
                writer,
            ))
            .await;
        bridge.ingest_batch(batch.clone()).await.unwrap();
        let settled = bridge.store.get_turn(&turn_id).await.unwrap().unwrap();
        assert_eq!(settled.status, "completed");
        assert_eq!(
            bridge.store.session_status(&session_id).await.as_deref(),
            Some("idle")
        );

        // A reconnect can replay the already committed terminal item. The
        // receipt/hash path must accept it without reopening or duplicating the
        // terminal transition.
        bridge.ingest_batch(batch).await.unwrap();
        let replayed = bridge.store.get_turn(&turn_id).await.unwrap().unwrap();
        assert_eq!(replayed.status, "completed");
    }

    #[tokio::test]
    async fn framed_envagent_connection_ingests_and_acknowledges_a_terminal_batch() {
        let (bridge, dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let producer = ProducerId::parse("producer-wire").unwrap();
        let session_id = session("wire-session");
        bridge
            .store
            .upsert_environment(
                &environment,
                "install-wire",
                "fixture",
                "development",
                &hash_secret("wire-token"),
                "[]",
            )
            .await
            .unwrap();
        bridge
            .store
            .create_session(&Session {
                id: session_id.clone(),
                workdir: Some(dir.path().display().to_string()),
                title: Some("wire".into()),
                inject_identity: false,
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".into(),
                status: "running".into(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        let turn_id = "wire-turn".to_string();
        bridge
            .store
            .create_turn(&crate::models::Turn {
                id: turn_id.clone(),
                session_id: session_id.clone(),
                status: "running".into(),
                origin: "user".into(),
                origin_subject: None,
                request_text: "wire".into(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                close_reason: None,
                created_at_ms: 1,
                started_at_ms: Some(1),
                completed_at_ms: None,
                stopped_at_ms: None,
            })
            .await
            .unwrap();
        let writer = crate::db::JournalWriter::new(bridge.store.clone());
        let consolidator =
            crate::storage::Consolidator::new(bridge.store.clone(), bridge.history.clone());
        bridge
            .set_turn_closer(crate::supervisor::TurnCloser::new(
                bridge.store.clone(),
                bridge.hub.clone(),
                bridge.seq.clone(),
                consolidator,
                writer,
            ))
            .await;

        let (mut client, server) = tokio::io::duplex(32 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server);
        let bridge_task = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.serve_io(server_reader, server_writer).await }
        });
        let hello = sylph_protocol::Hello::current(
            "install-wire",
            producer.clone(),
            Some(environment.clone()),
            0,
        );
        write_frame(
            &mut client,
            &serde_json::to_vec(&WireMessage::from_hello(hello)).unwrap(),
        )
        .await
        .unwrap();
        write_frame(&mut client, &serde_json::to_vec("wire-token").unwrap())
            .await
            .unwrap();
        let welcome =
            sylph_protocol::decode_message(&read_frame(&mut client).await.unwrap()).unwrap();
        let epoch = match welcome {
            DecodedMessage::Known(WireMessage::Welcome {
                connection_epoch, ..
            }) => connection_epoch,
            other => panic!("expected welcome, got {other:?}"),
        };
        let mut event = Event::new(EventKind::Turn, Some(Phase::Completed), 2);
        event.data = Some(serde_json::json!({"stopReason": "completed"}));
        let mut item = sylph_protocol::SequencedEvent::pending(
            producer.clone(),
            ProducerSeq::new(1),
            session_id.clone(),
            event,
            None,
        );
        item.arrival_turn_id = Some(turn_id.clone());
        let batch = EventBatch::new(environment, producer, epoch, vec![item]).unwrap();
        write_frame(
            &mut client,
            &serde_json::to_vec(&WireMessage::from_event_batch(batch)).unwrap(),
        )
        .await
        .unwrap();
        let response =
            sylph_protocol::decode_message(&read_frame(&mut client).await.unwrap()).unwrap();
        assert!(matches!(
            response,
            DecodedMessage::Known(WireMessage::Ack { accepted_through, .. })
                if accepted_through.get() == 1
        ));
        assert_eq!(
            bridge
                .store
                .get_turn(&turn_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "completed"
        );
        drop(client);
        let _ = bridge_task.await;
    }

    #[tokio::test]
    async fn an_unattributed_remote_terminal_opens_one_reachable_turn_container() {
        let (bridge, dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let producer = ProducerId::parse("producer-source").unwrap();
        let session_id = session("remote-source");
        bridge
            .store
            .upsert_environment(
                &environment,
                "install-source",
                "fixture",
                "development",
                &hash_secret("source-token"),
                "[]",
            )
            .await
            .unwrap();
        bridge
            .store
            .create_session(&Session {
                id: session_id.clone(),
                workdir: Some(dir.path().display().to_string()),
                title: Some("source".into()),
                inject_identity: false,
                fidelity: sylph_protocol::Fidelity::Blocks,
                flags_json: "{}".into(),
                status: "idle".into(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let hello = sylph_protocol::Hello::current(
            "install-source",
            producer.clone(),
            Some(environment.clone()),
            0,
        );
        let welcome = bridge
            .register_connection(&hello, "source-token", tx)
            .await
            .unwrap();
        let writer = crate::db::JournalWriter::new(bridge.store.clone());
        let consolidator =
            crate::storage::Consolidator::new(bridge.store.clone(), bridge.history.clone());
        bridge
            .set_turn_closer(crate::supervisor::TurnCloser::new(
                bridge.store.clone(),
                bridge.hub.clone(),
                bridge.seq.clone(),
                consolidator,
                writer,
            ))
            .await;
        let terminal = Event::new(EventKind::Turn, Some(Phase::Completed), 42);
        let item = sylph_protocol::SequencedEvent::pending(
            producer.clone(),
            ProducerSeq::new(1),
            session_id.clone(),
            terminal,
            Some(sylph_protocol::SourceCursor::new("native.jsonl", 1, 42).unwrap()),
        );
        bridge
            .ingest_batch(
                EventBatch::new(
                    environment.clone(),
                    producer.clone(),
                    welcome.connection_epoch,
                    vec![item.clone()],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let turns = bridge.store.list_turns(&session_id, 10).await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, "completed");
        let events = bridge
            .store
            .turn_events_after(&turns[0].id, -1)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "turn"));
        assert!(events
            .iter()
            .all(|event| event.turn_id.as_deref() == Some(turns[0].id.as_str())));
        bridge
            .ingest_batch(
                EventBatch::new(environment, producer, welcome.connection_epoch, vec![item])
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            bridge
                .store
                .list_turns(&session_id, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_remote_draft_cannot_be_claimed_twice_while_promoting() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        bridge.remote_drafts.lock().await.insert(
            "remote-draft-test".into(),
            RemoteDraft {
                draft_id: "remote-draft-test".into(),
                environment,
                session_id: None,
                agent: "claude".into(),
                workdir: "/tmp".into(),
                title: None,
                inject_identity: false,
                flags: Default::default(),
                identity_preamble: None,
                permission_mode: None,
                promoting: false,
                first_turn_id: None,
            },
        );
        assert!(matches!(
            bridge
                .claim_remote_draft("remote-draft-test", "turn-1".into())
                .await,
            RemoteDraftClaim::Claimed { .. }
        ));
        assert!(matches!(
            bridge
                .claim_remote_draft("remote-draft-test", "turn-1".into())
                .await,
            RemoteDraftClaim::Busy
        ));
    }

    /// Register a connected environment and return the frames the bridge writes
    /// to it, so a test can answer a command the way an envagent would.
    async fn connected_environment(
        bridge: &EnvBridge,
        environment: &EnvironmentId,
        install: &str,
    ) -> mpsc::Receiver<WireMessage> {
        bridge
            .store
            .upsert_environment(
                environment,
                install,
                "fixture",
                "development",
                &hash_secret("token"),
                "[]",
            )
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(8);
        let hello = sylph_protocol::Hello::current(
            install,
            ProducerId::parse(install).unwrap(),
            Some(environment.clone()),
            0,
        );
        bridge
            .register_connection(&hello, "token", tx)
            .await
            .unwrap();
        rx
    }

    /// Answer the next command frame the bridge sends with `result`, and return
    /// the request id it addressed.
    async fn answer_next_command(
        bridge: &EnvBridge,
        environment: &EnvironmentId,
        rx: &mut mpsc::Receiver<WireMessage>,
        result: serde_json::Value,
    ) -> sylph_protocol::CommandRequestId {
        let WireMessage::Command {
            request_id,
            idempotency,
            ..
        } = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("the bridge should write a command frame")
            .expect("the command channel should stay open")
        else {
            panic!("expected a command frame");
        };
        bridge
            .record_command_result(
                environment,
                &sylph_protocol::CommandResultMessage {
                    request_id: request_id.clone(),
                    outcome: CommandOutcome::accepted_with_result(result),
                    connection_epoch: 1,
                    idempotency: Some(idempotency),
                },
            )
            .await
            .unwrap();
        request_id
    }

    fn draft_capability_surface(model: &str) -> serde_json::Value {
        serde_json::json!({"capabilities": {"commands": [], "config": [
            {"id": "model", "value": model}
        ]}})
    }

    /// A draft's capability surface is read once before any config is set, so a
    /// frozen answer is indistinguishable from a real one: it names the agent's
    /// own defaults. Two reads with a config change between them are the only way
    /// to observe the difference, which is why one call could not.
    #[tokio::test]
    async fn a_second_draft_capability_read_observes_a_config_change() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let mut rx = connected_environment(&bridge, &environment, "install-draft-caps").await;
        let draft = RemoteDraft {
            draft_id: "remote-draft-caps".into(),
            environment: environment.clone(),
            session_id: None,
            agent: "claude".into(),
            workdir: "/tmp".into(),
            title: None,
            inject_identity: false,
            flags: Default::default(),
            identity_preamble: None,
            permission_mode: None,
            promoting: false,
            first_turn_id: None,
        };
        bridge
            .remote_drafts
            .lock()
            .await
            .insert(draft.draft_id.clone(), draft.clone());

        // The first read is the one a client makes right after creating a draft,
        // before any model has been picked.
        let first = {
            let bridge = bridge.clone();
            let draft = draft.clone();
            tokio::spawn(async move { bridge.remote_draft_capabilities(&draft).await })
        };
        let first_request = answer_next_command(
            &bridge,
            &environment,
            &mut rx,
            draft_capability_surface("default"),
        )
        .await;
        assert_eq!(
            first.await.unwrap().unwrap()["capabilities"]["config"][0]["value"],
            "default"
        );

        // The user picks a model. The envagent now holds a different surface.
        let second = {
            let bridge = bridge.clone();
            let draft = draft.clone();
            tokio::spawn(async move { bridge.remote_draft_capabilities(&draft).await })
        };
        let second_request = answer_next_command(
            &bridge,
            &environment,
            &mut rx,
            draft_capability_surface("sonnet"),
        )
        .await;
        assert_eq!(
            second.await.unwrap().unwrap()["capabilities"]["config"][0]["value"],
            "sonnet",
            "the second read must report what the envagent holds now, not the \
             empty surface frozen before any config was set"
        );
        assert_ne!(
            first_request.as_str(),
            second_request.as_str(),
            "two reads of one draft must address two commands, or the second \
             never reaches the envagent at all"
        );
    }

    /// Same defect, same draft-shaped command: a listing frozen at its first
    /// answer cannot show a file written afterwards.
    #[tokio::test]
    async fn a_second_draft_file_listing_observes_a_new_entry() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let mut rx = connected_environment(&bridge, &environment, "install-draft-files").await;
        let list = || sylph_protocol::FileOperation::List {
            relative: ".".into(),
        };

        let first = {
            let bridge = bridge.clone();
            let environment = environment.clone();
            tokio::spawn(async move {
                bridge
                    .remote_files(
                        &environment,
                        Some("remote-draft-files".into()),
                        None,
                        list(),
                    )
                    .await
            })
        };
        answer_next_command(
            &bridge,
            &environment,
            &mut rx,
            serde_json::json!({"entries": ["a.txt"]}),
        )
        .await;
        assert_eq!(first.await.unwrap().unwrap()["entries"][0], "a.txt");

        let second = {
            let bridge = bridge.clone();
            let environment = environment.clone();
            tokio::spawn(async move {
                bridge
                    .remote_files(
                        &environment,
                        Some("remote-draft-files".into()),
                        None,
                        list(),
                    )
                    .await
            })
        };
        answer_next_command(
            &bridge,
            &environment,
            &mut rx,
            serde_json::json!({"entries": ["a.txt", "b.txt"]}),
        )
        .await;
        let listing = second.await.unwrap().unwrap();
        assert_eq!(
            listing["entries"].as_array().map(Vec::len),
            Some(2),
            "a draft listing after a write must observe the new entry"
        );
    }

    /// A steer is a user action, not a query: sending the same text twice must
    /// reach the envagent twice. A payload-derived request id would make the
    /// second one address the first one's stored result and silently vanish.
    #[tokio::test]
    async fn repeating_one_steer_text_dispatches_a_second_command() {
        let (bridge, _dir) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        let mut rx = connected_environment(&bridge, &environment, "install-steer").await;
        let session_id = SessionId::new(
            environment.clone(),
            AgentName::parse("claude").unwrap(),
            sylph_protocol::NativeId::new("steer-session"),
        );
        let steer = || sylph_protocol::EnvironmentCommand::Steer {
            session_id: session_id.clone(),
            text: "focus on the parser".into(),
            expected_turn_id: None,
        };

        let mut request_ids = Vec::new();
        for _ in 0..2 {
            let task = {
                let bridge = bridge.clone();
                let session_id = session_id.clone();
                let command = steer();
                tokio::spawn(async move {
                    bridge
                        .remote_session_command(&session_id, command, IdempotencyPolicy::AtMostOnce)
                        .await
                })
            };
            request_ids.push(
                answer_next_command(&bridge, &environment, &mut rx, serde_json::json!({})).await,
            );
            assert!(matches!(
                task.await.unwrap().unwrap(),
                CommandOutcome::Accepted { .. }
            ));
        }
        assert_ne!(
            request_ids[0].as_str(),
            request_ids[1].as_str(),
            "the same steer text sent twice must be two commands"
        );
    }

    /// Two re-reads of one session are two commands, not one replayed answer.
    ///
    /// A re-read reads a file and writes no vendor state, so it looks like the kind of
    /// question that may safely address its own earlier result — and if it were treated that
    /// way, its request id would be hashed from a payload that is *identical on every call*.
    /// The second click would then resolve to the first pass's durable outcome and report
    /// counts taken before the bytes it was clicked to collect existed: a refresh returning a
    /// cached answer, which is the defect the endpoint was added to remove.
    ///
    /// Asserted on the ids rather than on an observed dispatch, because the id is the whole
    /// mechanism: equal ids *are* the replay.
    #[test]
    fn re_reading_twice_mints_two_request_ids_rather_than_replaying_one() {
        let session = session("native-1");
        let command = sylph_protocol::EnvironmentCommand::Reread {
            session_id: session.clone(),
            agent: "claude".to_string(),
            native_id: session.native_id.clone(),
        };

        assert!(
            command_needs_a_fresh_request_id(&command),
            "a re-read whose payload is its idempotency scope replays the previous pass"
        );

        // The id the bridge actually mints, twice. These must differ, and that is the
        // whole property: equal ids *are* the replay, because the second call would
        // resolve to the first pass's durable outcome in `env_command_results`.
        let minted_once = remote_request_id(
            &session.environment,
            &command,
            IdempotencyPolicy::AtMostOnce,
        )
        .unwrap();
        let minted_again = remote_request_id(
            &session.environment,
            &command,
            IdempotencyPolicy::AtMostOnce,
        )
        .unwrap();
        assert_ne!(
            minted_once.as_str(),
            minted_again.as_str(),
            "a re-read's payload is identical on every call, so a payload-derived id \
             would report the counts of a pass that ran before the bytes it was \
             clicked to collect existed"
        );
    }
}
