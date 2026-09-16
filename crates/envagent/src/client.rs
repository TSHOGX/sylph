//! Outbound envagent connection wiring.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};

const MAX_INTERACTIVE_COMMANDS: usize = 8;
const MAX_HISTORY_COMMANDS: usize = 2;

use crate::runtime::{CaptureTransport, ChannelCaptureTransport};
use crate::transport::{ConnectionSupervisor, FramedConnection};

/// A live connection after hello/welcome negotiation.
pub struct ClientConnection {
    pub sink: Arc<ChannelCaptureTransport>,
    pub incoming: mpsc::Receiver<sylph_protocol::DecodedMessage>,
    pub welcome: sylph_protocol::Welcome,
    sender: mpsc::Sender<sylph_protocol::WireMessage>,
    heartbeat_stop: watch::Sender<()>,
}

impl ClientConnection {
    pub async fn send(&self, message: sylph_protocol::WireMessage) -> Result<()> {
        self.sender
            .send(message)
            .await
            .map_err(|_| anyhow::anyhow!("envagent writer task has stopped"))
    }
}

/// Connect to a server and keep the reader/writer halves independent so an
/// event delivery can wait for an ack while the reader continues to process it.
pub async fn connect(
    address: impl Into<String>,
    mut hello: sylph_protocol::Hello,
    credential: impl Into<String>,
) -> Result<ClientConnection> {
    let mut supervisor = ConnectionSupervisor::new(address).with_credential(credential);
    let (framed, welcome) = supervisor.connect_and_welcome(&hello).await?;
    hello.connection_epoch = welcome.connection_epoch;
    let stream = framed.into_inner();
    let (reader, writer) = tokio::io::split(stream);
    let (out_tx, out_rx) = mpsc::channel::<sylph_protocol::WireMessage>(64);
    let (in_tx, in_rx) = mpsc::channel::<sylph_protocol::DecodedMessage>(64);
    let sender_for_connection = out_tx.clone();
    let (heartbeat_stop, mut heartbeat_rx) = watch::channel(());
    let heartbeat_sender = out_tx.clone();
    let heartbeat_epoch = welcome.connection_epoch;
    let heartbeat_interval = Duration::from_millis(welcome.heartbeat_interval_ms.max(100));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_interval);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if heartbeat_sender.send(sylph_protocol::WireMessage::Heartbeat {
                        connection_epoch: heartbeat_epoch,
                        sent_at_ms: unix_now_ms(),
                    }).await.is_err() {
                        break;
                    }
                }
                changed = heartbeat_rx.changed() => {
                    if changed.is_err() { break; }
                    break;
                }
            }
        }
    });
    let sink = Arc::new(ChannelCaptureTransport::new(
        out_tx,
        welcome.connection_epoch,
    ));
    let sink_for_reader = sink.clone();
    let sink_for_writer = sink.clone();
    tokio::spawn(writer_loop(writer, out_rx, sink_for_writer));
    tokio::spawn(reader_loop(reader, sink_for_reader, in_tx));
    Ok(ClientConnection {
        sink,
        incoming: in_rx,
        welcome,
        sender: sender_for_connection,
        heartbeat_stop,
    })
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Run the command loop for one paired environment. Reconnect supervision is
/// deliberately outside this function so a caller can keep the same spool
/// and construct a new connection epoch after a drop.
pub async fn run_runtime(
    address: impl Into<String>,
    hello: sylph_protocol::Hello,
    credential: impl Into<String>,
    config: crate::runtime::EnvAgentConfig,
) -> Result<()> {
    let connection = connect(address, hello, credential).await?;
    let runtime = Arc::new(crate::runtime::EnvAgent::open(config, connection.sink.clone()).await?);
    run_connection(runtime, connection).await
}

/// Run an envagent over an already-connected duplex stream.
///
/// This is used by the server's local development deployment to exercise the
/// same hello/welcome, command, ack, and reconnect machinery without opening a
/// second TCP listener. Production agents use [`run_runtime`] with TCP; the
/// semantic path after framing is identical.
pub async fn run_runtime_stream<S>(
    stream: S,
    hello: sylph_protocol::Hello,
    credential: impl Into<String>,
    config: crate::runtime::EnvAgentConfig,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    run_runtime_stream_with_adapters(stream, hello, credential, config, crate::adapters::linked())
        .await
}

/// Run an envagent over an already-connected stream with an explicit adapter source.
///
/// This is the integration-test seam: the fixture adapter remains inside the envagent
/// process boundary, while production callers continue to use [`run_runtime_stream`].
pub async fn run_runtime_stream_with_adapters<S>(
    stream: S,
    mut hello: sylph_protocol::Hello,
    credential: impl Into<String>,
    config: crate::runtime::EnvAgentConfig,
    adapters: Arc<dyn crate::adapters::AdapterSource>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut framed = FramedConnection::new(stream);
    hello.connection_epoch = 1;
    framed
        .write_env_message(&sylph_protocol::WireMessage::from_hello(hello.clone()))
        .await?;
    framed.write_json(&credential.into()).await?;
    let welcome = match framed.read_env_message().await? {
        sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Welcome {
            protocol,
            version,
            environment_id,
            connection_epoch,
            server_epoch,

            heartbeat_interval_ms,
        }) => sylph_protocol::Welcome {
            protocol,
            version,
            environment_id,
            connection_epoch,
            server_epoch,

            heartbeat_interval_ms,
        },
        other => return Err(anyhow!("expected envagent welcome, got {other:?}")),
    };
    let stream = framed.into_inner();
    let (reader, writer) = tokio::io::split(stream);
    let (out_tx, out_rx) = mpsc::channel::<sylph_protocol::WireMessage>(64);
    let (in_tx, in_rx) = mpsc::channel::<sylph_protocol::DecodedMessage>(64);
    let (heartbeat_stop, mut heartbeat_rx) = watch::channel(());
    let heartbeat_sender = out_tx.clone();
    let heartbeat_epoch = welcome.connection_epoch;
    let heartbeat_interval = Duration::from_millis(welcome.heartbeat_interval_ms.max(100));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_interval);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if heartbeat_sender.send(sylph_protocol::WireMessage::Heartbeat {
                        connection_epoch: heartbeat_epoch,
                        sent_at_ms: unix_now_ms(),
                    }).await.is_err() { break; }
                }
                changed = heartbeat_rx.changed() => {
                    if changed.is_err() { break; }
                    break;
                }
            }
        }
    });
    let sink = Arc::new(ChannelCaptureTransport::new(
        out_tx.clone(),
        welcome.connection_epoch,
    ));
    tokio::spawn(writer_loop(writer, out_rx, sink.clone()));
    tokio::spawn(reader_loop(reader, sink.clone(), in_tx));
    let connection = ClientConnection {
        sink: sink.clone(),
        incoming: in_rx,
        welcome,
        sender: out_tx,
        heartbeat_stop,
    };
    let runtime =
        Arc::new(crate::runtime::EnvAgent::open_with_adapters(config, sink, adapters).await?);
    run_connection(runtime, connection).await
}

async fn run_connection(
    runtime: Arc<crate::runtime::EnvAgent>,
    connection: ClientConnection,
) -> Result<()> {
    runtime.set_sink(connection.sink.clone()).await;
    // Replay a fixed durable prefix alongside commands. Waiting here blocks Files
    // during reconnect, while a single batch strands the rest of an ended source.
    let mut replay_tasks = tokio::task::JoinSet::new();
    let replay_runtime = runtime.clone();
    replay_tasks.spawn(async move { replay_runtime.flush_snapshot().await });
    let sender = connection.sender.clone();
    let mut incoming = connection.incoming;
    // A native re-read can take minutes and must not hold the receive loop hostage:
    // Files, heartbeat, and control commands still need a response while it runs.
    // History has a separate bound: one shared pool still lets bulk import consume
    // every slot needed by Files and stop commands. Saturation refuses undispatched
    // work immediately instead of letting an invisible queue outlive the RPC deadline.
    let interactive_slots = Arc::new(Semaphore::new(MAX_INTERACTIVE_COMMANDS));
    let history_slots = Arc::new(Semaphore::new(MAX_HISTORY_COMMANDS));
    let mut command_tails = HashMap::new();
    let mut command_tasks = tokio::task::JoinSet::new();
    let mut inventory_tasks = tokio::task::JoinSet::new();
    let mut inventory_tick = tokio::time::interval(Duration::from_secs(5));
    inventory_tick.tick().await;
    let mut inventory = None;
    let mut replay_failure_reported = false;
    loop {
        tokio::select! {
            maybe = incoming.recv() => {
                let Some(message) = maybe else { break; };
                match message {
            sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Command {
                request_id,
                idempotency,
                command,
            }) => {
                let slots = if is_history_command(&command) {
                    &history_slots
                } else {
                    &interactive_slots
                };
                let permit = match slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        sender.send(sylph_protocol::WireMessage::CommandResult {
                            request_id,
                            outcome: sylph_protocol::CommandOutcome::Rejected {
                                code: "COMMAND_OVERLOADED".into(),
                                message: "envagent command capacity is full; this request was not executed".into(),
                            },
                            connection_epoch: connection.welcome.connection_epoch,
                            idempotency: Some(idempotency),
                        }).await?;
                        continue;
                    }
                };
                // Link predecessors before spawning: acquiring a mutex inside each
                // task would let executor scheduling reorder commands from the wire.
                let key = command_session_key(&command);
                let (completion, next) = oneshot::channel::<()>();
                let predecessor = key.as_ref().and_then(|key| {
                    command_tails
                        .insert(key.clone(), (request_id.clone(), next))
                        .map(|(_, previous)| previous)
                });
                let runtime = runtime.clone();
                let sender = sender.clone();
                let connection_epoch = connection.welcome.connection_epoch;
                let file_list=match &command {
                    sylph_protocol::EnvironmentCommand::Files{operation:sylph_protocol::FileOperation::List{relative},..}=>Some(relative.clone()),
                    _=>None,
                };
                let queued_at=std::time::Instant::now();
                command_tasks.spawn(async move {
                    let _permit = permit;
                    if let Some(predecessor) = predecessor {
                        let _ = predecessor.await;
                    }
                    let queue_us=queued_at.elapsed().as_micros() as u64;
                    let executing=std::time::Instant::now();
                    let outcome = runtime.execute_command(request_id.clone(),idempotency,command).await;
                    if let Some(relative)=file_list {
                        tracing::debug!(%request_id,%relative,queue_us,filesystem_us=executing.elapsed().as_micros() as u64,"file_list_timing");
                    }
                    let _ = sender
                        .send(sylph_protocol::WireMessage::CommandResult {
                            request_id: request_id.clone(),
                            outcome,
                            connection_epoch,
                            idempotency: Some(idempotency),
                        })
                        .await;
                    let _ = completion.send(());
                    (key, request_id)
                });
            }

            sylph_protocol::DecodedMessage::Unknown { kind, .. } => {
                tracing::debug!("ignoring unknown server frame {kind}");
            }
            sylph_protocol::DecodedMessage::Known(other) => {
                tracing::debug!("ignoring server frame {}", other.kind());
            }
                }
            }
            _ = inventory_tick.tick() => {
                if inventory_tasks.is_empty() {
                    let runtime = runtime.clone();
                    inventory_tasks.spawn(async move { runtime.inventory().await });
                }
            }
            Some(completed) = inventory_tasks.join_next(), if !inventory_tasks.is_empty() => {
                let current = completed?;
                if inventory.as_ref() != Some(&current) {
                    sender.send(sylph_protocol::WireMessage::Agents {
                        connection_epoch: connection.welcome.connection_epoch,
                        agents: current.clone(),
                    }).await?;
                    inventory = Some(current);
                }
            }
            Some(completed) = command_tasks.join_next(), if !command_tasks.is_empty() => {
                if let Ok((Some(key), request_id)) = completed {
                    if command_tails.get(&key).is_some_and(|(tail, _)| *tail == request_id) {
                        command_tails.remove(&key);
                    }
                }
            }
            Some(completed) = replay_tasks.join_next(), if !replay_tasks.is_empty() => {
                if let Err(error)=completed? {
                    if !replay_failure_reported { tracing::error!(%error,"retained delivery will retry while commands remain available"); replay_failure_reported=true; }
                    else { tracing::debug!(%error,"retained delivery retry remains blocked"); }
                    let retry=runtime.clone();
                    replay_tasks.spawn(async move {tokio::time::sleep(Duration::from_millis(500)).await;retry.flush_snapshot().await});
                } else { replay_failure_reported=false; }
            }
        }
    }
    connection.sink.fail_pending().await;
    command_tasks.abort_all();
    inventory_tasks.abort_all();
    replay_tasks.abort_all();
    while command_tasks.join_next().await.is_some() {}
    while inventory_tasks.join_next().await.is_some() {}
    while replay_tasks.join_next().await.is_some() {}
    let _ = connection.heartbeat_stop.send(());
    Err(anyhow!("envagent server connection closed"))
}

fn is_history_command(command: &sylph_protocol::EnvironmentCommand) -> bool {
    matches!(
        command,
        sylph_protocol::EnvironmentCommand::Adopt { .. }
            | sylph_protocol::EnvironmentCommand::Watch { .. }
            | sylph_protocol::EnvironmentCommand::Reread { .. }
            | sylph_protocol::EnvironmentCommand::Enumerate { .. }
    )
}

/// Return the serialization domain for commands that mutate a hosted source.
/// Observation commands deliberately have no key: a slow history parse or file
/// listing must not hold up an unrelated read, while sends/stops/adopts retain
/// their arrival order for one session.
fn command_session_key(command: &sylph_protocol::EnvironmentCommand) -> Option<String> {
    use sylph_protocol::EnvironmentCommand;
    match command {
        EnvironmentCommand::Prepare { draft_id, .. }
        | EnvironmentCommand::DraftSetConfig { draft_id, .. }
        | EnvironmentCommand::DraftClose { draft_id } => Some(format!("draft:{draft_id}")),
        EnvironmentCommand::Files {
            session_id,
            operation:
                sylph_protocol::FileOperation::Write { .. }
                | sylph_protocol::FileOperation::Mkdir { .. }
                | sylph_protocol::FileOperation::Remove { .. },
            ..
        } => Some(format!("session:{session_id}")),
        EnvironmentCommand::DraftFiles {
            draft_id,
            operation:
                sylph_protocol::FileOperation::Write { .. }
                | sylph_protocol::FileOperation::Mkdir { .. }
                | sylph_protocol::FileOperation::Remove { .. },
        } => Some(format!("draft:{draft_id}")),
        EnvironmentCommand::Files { .. }
        | EnvironmentCommand::Capabilities { .. }
        | EnvironmentCommand::BrowseRoots
        | EnvironmentCommand::BrowseTree { .. }
        | EnvironmentCommand::Enumerate { .. }
        | EnvironmentCommand::DraftCapabilities { .. }
        | EnvironmentCommand::DraftFiles { .. }
        | EnvironmentCommand::Health => None,
        EnvironmentCommand::Run {
            session_id,
            draft_id,
            ..
        } => session_id
            .as_ref()
            .map(|id| format!("session:{id}"))
            .or_else(|| draft_id.as_ref().map(|id| format!("draft:{id}"))),
        EnvironmentCommand::Send { session_id, .. }
        | EnvironmentCommand::Interrupt { session_id, .. }
        | EnvironmentCommand::Steer { session_id, .. }
        | EnvironmentCommand::QueueSteering { session_id, .. }
        | EnvironmentCommand::Queue { session_id, .. }
        | EnvironmentCommand::SetConfig { session_id, .. }
        | EnvironmentCommand::Resolve { session_id, .. }
        | EnvironmentCommand::Watch { session_id, .. }
        | EnvironmentCommand::Adopt { session_id, .. }
        | EnvironmentCommand::Close {
            session_id: Some(session_id),
        } => Some(format!("session:{session_id}")),
        EnvironmentCommand::Close { session_id: None } => None,
        EnvironmentCommand::Reread { .. } => None,
    }
}

/// Keep an envagent attached while the server or network is temporarily
/// unavailable. The spool is reopened at the same path on every iteration;
/// no event is dropped or re-numbered.
pub async fn run_runtime_reconnecting(
    address: String,
    mut hello: sylph_protocol::Hello,
    credential: String,
    config: crate::runtime::EnvAgentConfig,
) -> Result<()> {
    let mut backoff = crate::transport::Backoff::default();
    let runtime =
        Arc::new(crate::runtime::EnvAgent::open(config, Arc::new(DisconnectedSink)).await?);
    loop {
        match connect(address.clone(), hello.clone(), credential.clone()).await {
            Ok(connection) => match run_connection(runtime.clone(), connection).await {
                Ok(()) => {
                    backoff.reset();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    runtime.set_sink(Arc::new(DisconnectedSink)).await;
                    let delay = backoff.fail();
                    tracing::debug!("envagent reconnect after {delay:?}: {error:#}");
                    tokio::time::sleep(delay).await;
                }
            },
            Err(error) => {
                runtime.set_sink(Arc::new(DisconnectedSink)).await;
                let delay = backoff.fail();
                tracing::debug!("envagent reconnect after {delay:?}: {error:#}");
                tokio::time::sleep(delay).await;
            }
        }
        hello.connection_epoch = hello.connection_epoch.saturating_add(1);
    }
}

struct DisconnectedSink;

#[async_trait::async_trait]
impl CaptureTransport for DisconnectedSink {
    async fn upload(
        &self,
        _operation: sylph_protocol::CaptureUpload,
    ) -> Result<sylph_protocol::CaptureReply> {
        anyhow::bail!("envagent disconnected")
    }
    async fn notify(&self, _message: sylph_protocol::WireMessage) -> Result<()> {
        anyhow::bail!("envagent disconnected")
    }
}

async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut out_rx: mpsc::Receiver<sylph_protocol::WireMessage>,
    sink: Arc<ChannelCaptureTransport>,
) -> Result<()> {
    let mut framed = FramedConnection::new(&mut writer);
    let result = async {
        while let Some(message) = out_rx.recv().await {
            framed.write_env_message(&message).await?;
        }
        Ok::<(), crate::transport::FrameError>(())
    }
    .await;
    sink.fail_pending().await;
    result.map_err(Into::into)
}

async fn reader_loop<R: AsyncRead + Unpin>(
    reader: R,
    sink: Arc<ChannelCaptureTransport>,
    in_tx: mpsc::Sender<sylph_protocol::DecodedMessage>,
) -> Result<()> {
    let mut framed = FramedConnection::new(reader);
    let result = async {
        loop {
            let message = framed.read_env_message().await?;
            if let sylph_protocol::DecodedMessage::Known(
                sylph_protocol::WireMessage::CaptureReply {
                    request_id,
                    outcome,
                    connection_epoch,
                },
            ) = &message
            {
                sink.reply(request_id, outcome.clone(), *connection_epoch)
                    .await;
                continue;
            }
            if in_tx.send(message).await.is_err() {
                break;
            }
        }
        Ok::<(), crate::transport::FrameError>(())
    }
    .await;
    sink.fail_pending().await;
    result.map_err(Into::into)
}
