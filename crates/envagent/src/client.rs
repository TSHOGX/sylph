//! Outbound envagent connection wiring.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use crate::runtime::{ChannelEventSink, EventSink};
use crate::transport::{ConnectionSupervisor, FramedConnection};

/// A live connection after hello/welcome negotiation.
pub struct ClientConnection {
    pub sink: Arc<ChannelEventSink>,
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
    let sink = Arc::new(ChannelEventSink::new(
        out_tx,
        welcome.environment_id.clone(),
        hello.producer_id.clone(),
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
    let runtime = crate::runtime::EnvAgent::open(config, connection.sink.clone()).await?;
    run_connection(&runtime, connection).await
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
    run_runtime_stream_with_adapters(
        stream,
        hello,
        credential,
        config,
        crate::adapters::linked(),
    )
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
            accepted_cursors,
            max_batch_events,
            heartbeat_interval_ms,
        }) => sylph_protocol::Welcome {
            protocol,
            version,
            environment_id,
            connection_epoch,
            server_epoch,
            accepted_cursors,
            max_batch_events,
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
    let sink = Arc::new(ChannelEventSink::new(
        out_tx.clone(),
        welcome.environment_id.clone(),
        hello.producer_id.clone(),
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
    let runtime = crate::runtime::EnvAgent::open_with_adapters(config, sink, adapters).await?;
    run_connection(&runtime, connection).await
}

async fn run_connection(
    runtime: &crate::runtime::EnvAgent,
    connection: ClientConnection,
) -> Result<()> {
    runtime.set_sink(connection.sink.clone()).await;
    // Reconnects resume from the first unacknowledged producer sequence.  A
    // failed flush must tear down this connection; otherwise the runtime can
    // continue accepting commands while its durable prefix is stuck behind a
    // dead sink.
    runtime.flush_pending(128).await?;
    let sender = connection.sender.clone();
    let mut incoming = connection.incoming;
    let mut inventory_tick = tokio::time::interval(Duration::from_secs(5));
    inventory_tick.tick().await;
    let mut inventory = runtime.inventory().await;
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
                let outcome = runtime.handle_command(command).await;
                sender
                    .send(sylph_protocol::WireMessage::CommandResult {
                        request_id,
                        outcome,
                        connection_epoch: connection.welcome.connection_epoch,
                        idempotency: Some(idempotency),
                    })
                    .await?;
            }
            sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Nack {
                expected_next,
                reason,
                ..
            }) => {
                tracing::warn!(
                    expected = expected_next.get(),
                    "server rejected an envagent batch: {reason}"
                );
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
                let current = runtime.inventory().await;
                if current != inventory {
                    sender.send(sylph_protocol::WireMessage::Agents {
                        connection_epoch: connection.welcome.connection_epoch,
                        agents: current.clone(),
                    }).await?;
                    inventory = current;
                }
            }
        }
    }
    let _ = connection.heartbeat_stop.send(());
    Err(anyhow!("envagent server connection closed"))
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
    let runtime = crate::runtime::EnvAgent::open(config, Arc::new(DisconnectedSink)).await?;
    loop {
        match connect(address.clone(), hello.clone(), credential.clone()).await {
            Ok(connection) => match run_connection(&runtime, connection).await {
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
impl EventSink for DisconnectedSink {
    async fn deliver(&self, _events: &[crate::spool::SpoolEvent]) -> Result<u64> {
        anyhow::bail!("envagent is disconnected")
    }
}

async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut out_rx: mpsc::Receiver<sylph_protocol::WireMessage>,
    sink: Arc<ChannelEventSink>,
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
    sink: Arc<ChannelEventSink>,
    in_tx: mpsc::Sender<sylph_protocol::DecodedMessage>,
) -> Result<()> {
    let mut framed = FramedConnection::new(reader);
    let result = async {
        loop {
            let message = framed.read_env_message().await?;
            if let sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Ack {
                producer_id,
                accepted_through,
                server_seq_through,
            }) = &message
            {
                let ack = sylph_protocol::Ack {
                    producer_id: producer_id.clone(),
                    accepted_through: *accepted_through,
                    server_seq_through: *server_seq_through,
                };
                sink.acknowledge(&ack).await;
            }
            if let sylph_protocol::DecodedMessage::Known(sylph_protocol::WireMessage::Nack {
                producer_id,
                expected_next,
                ..
            }) = &message
            {
                if sink.is_for_producer(producer_id) {
                    sink.reject_gap(*expected_next).await;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::EventSink;
    use crate::spool::SpoolEvent;
    use sylph_protocol::{EnvironmentId, Event, EventKind, ProducerId, WireMessage};

    #[tokio::test]
    async fn reader_eof_releases_a_delivery_waiting_for_ack() {
        let (agent_stream, peer_stream) = tokio::io::duplex(1024);
        let (sender, mut outgoing) = mpsc::channel(2);
        let sink = Arc::new(ChannelEventSink::new(
            sender,
            EnvironmentId::parse("env7fk2").unwrap(),
            ProducerId::parse("producer-a").unwrap(),
            1,
        ));
        let (in_tx, _in_rx) = mpsc::channel(2);
        let reader_sink = sink.clone();
        let reader = tokio::spawn(reader_loop(agent_stream, reader_sink, in_tx));

        let row = SpoolEvent {
            producer_id: "producer-a".into(),
            producer_seq: 1,
            kind: "event".into(),
            session_id: "env7fk2:fixture:n".into(),
            arrival_turn_id: None,
            payload: serde_json::to_vec(&Event::new(EventKind::Status, None, 1)).unwrap(),
            source_cursor: None,
            recorded_at_ms: None,
        };
        let deliver_sink = sink.clone();
        let delivery = tokio::spawn(async move { deliver_sink.deliver(&[row]).await });
        assert!(matches!(
            outgoing.recv().await,
            Some(WireMessage::EventBatch { .. })
        ));

        drop(peer_stream);
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), delivery)
            .await
            .expect("reader EOF must release the delivery waiter")
            .expect("delivery task must not panic");
        assert!(result.is_err());
        reader
            .await
            .expect("reader task must not panic")
            .unwrap_err();
    }
}
