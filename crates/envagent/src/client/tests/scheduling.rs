use super::*;
use crate::adapters::AdapterSource;
use crate::path::PathPolicy;
use crate::runtime::{EnvAgent, EnvAgentConfig};
use crate::spool::SpoolConfig;
use futures::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use sylph_protocol::{
    Adapter, AgentInfo, CommandOutcome, CommandRequestId, EnvironmentCommand, EventStream,
    Fidelity, FileOperation, IdempotencyPolicy, NativeId, NativeRecord, NativeSummary, RunSpec,
    Session, SessionId, SessionMessage, StoreFilter, TurnInputFact,
};

struct ActiveRead(Arc<AtomicUsize>);

impl ActiveRead {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self(active)
    }
}

impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct BlockingSource {
    workdir: String,
    started: mpsc::UnboundedSender<String>,
    release: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
}

impl AdapterSource for BlockingSource {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        (name == "fixture").then(|| Box::new(self.clone()) as Box<dyn Adapter>)
    }

    fn known_agents(&self) -> Vec<String> {
        vec!["fixture".into()]
    }
}

#[async_trait::async_trait]
impl Adapter for BlockingSource {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(AgentInfo {
            name: "fixture".into(),
            version: None,
            flags: Default::default(),
            supports: Vec::new(),
            dictates_native_id: false,
            native_fidelity: Fidelity::Blocks,
        })
    }

    async fn run(&self, _spec: RunSpec) -> Result<Box<dyn Session>> {
        anyhow::bail!("fixture exposes native history only")
    }

    async fn adopt(&self, native_id: &NativeId) -> Result<EventStream> {
        let source = self.clone();
        let native_id = native_id.to_string();
        let prefix = futures::stream::iter([Ok(SessionMessage::TurnInput(TurnInputFact::new(
            "record before the blocked native read",
        )))]);
        let blocked = futures::stream::once(async move {
            let _active = ActiveRead::new(source.active.clone());
            source.started.send(native_id).unwrap();
            source.release.acquire().await.unwrap().forget();
            Ok(SessionMessage::TurnInput(TurnInputFact::new(
                "record after the blocked native read",
            )))
        });
        Ok(Box::pin(prefix.chain(blocked)))
    }

    async fn reproject(&self, _records: Vec<NativeRecord>) -> Result<EventStream> {
        anyhow::bail!("fixture has no retained records")
    }

    async fn watch(&self, native_id: &NativeId) -> Result<EventStream> {
        let _active = ActiveRead::new(self.active.clone());
        self.started.send(native_id.to_string()).unwrap();
        self.release.acquire().await.unwrap().forget();
        Ok(Box::pin(futures::stream::pending()))
    }

    async fn enumerate(&self, _filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(["ready", "ordered"]
            .into_iter()
            .map(|native_id| NativeSummary {
                native_id: NativeId::new(native_id),
                workdir: Some(self.workdir.clone()),
                title: None,
                started_at_ms: None,
                ended_at_ms: None,
                event_count: None,
                source_revision: None,
                parent_native_id: None,
                parent_relation: None,
                fidelity: Fidelity::Blocks,
            })
            .collect())
    }
}

struct CommandHarness {
    incoming: Option<mpsc::Sender<sylph_protocol::DecodedMessage>>,
    outgoing: mpsc::Receiver<WireMessage>,
    started: mpsc::UnboundedReceiver<String>,
    source: BlockingSource,
    sink: Arc<ChannelEventSink>,
    runtime: Arc<EnvAgent>,
    task: tokio::task::JoinHandle<Result<()>>,
    _root: tempfile::TempDir,
}

fn fixture_session(native_id: &str) -> SessionId {
    SessionId::decode(&format!("env7fk2:fixture:{native_id}")).unwrap()
}

impl CommandHarness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workdir = root.path().join("work");
        std::fs::create_dir(&workdir).unwrap();
        std::fs::write(workdir.join("visible.txt"), "native workspace").unwrap();
        let (started_tx, started) = mpsc::unbounded_channel();
        let source = BlockingSource {
            workdir: workdir.display().to_string(),
            started: started_tx,
            release: Arc::new(Semaphore::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
        };
        let (sender, outgoing) = mpsc::channel(64);
        let sink = Arc::new(ChannelEventSink::new(
            sender.clone(),
            EnvironmentId::parse("env7fk2").unwrap(),
            ProducerId::parse("producer-a").unwrap(),
            1,
        ));
        let runtime = Arc::new(
            EnvAgent::open_with_adapters(
                EnvAgentConfig {
                    environment_id: "env7fk2".into(),
                    producer_id: "producer-a".into(),
                    spool: SpoolConfig::new(root.path().join("spool.db"), 1024 * 1024),
                    path_policy: PathPolicy::from_roots([workdir]).unwrap(),
                },
                sink.clone(),
                Arc::new(source.clone()),
            )
            .await
            .unwrap(),
        );
        let (incoming, receiver) = mpsc::channel(64);
        let (heartbeat_stop, _) = watch::channel(());
        let connection = ClientConnection {
            sink: sink.clone(),
            incoming: receiver,
            welcome: sylph_protocol::Welcome::current(
                EnvironmentId::parse("env7fk2").unwrap(),
                1,
                1,
            ),
            sender,
            heartbeat_stop,
        };
        let task = tokio::spawn(run_connection(runtime.clone(), connection));
        Self {
            incoming: Some(incoming),
            outgoing,
            started,
            source,
            sink,
            runtime,
            task,
            _root: root,
        }
    }

    async fn send(&self, request_id: &str, command: EnvironmentCommand) {
        self.incoming
            .as_ref()
            .unwrap()
            .send(sylph_protocol::DecodedMessage::Known(
                WireMessage::Command {
                    request_id: CommandRequestId::parse(request_id).unwrap(),
                    idempotency: IdempotencyPolicy::AtMostOnce,
                    command,
                },
            ))
            .await
            .unwrap();
    }

    async fn result(&mut self) -> (String, CommandOutcome) {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(2), self.outgoing.recv())
                .await
                .expect("command must return while native history remains blocked")
                .expect("command connection remains open");
            match message {
                WireMessage::CommandResult {
                    request_id,
                    outcome,
                    ..
                } => return (request_id.to_string(), outcome),
                WireMessage::Agents { .. } => {}
                other => panic!("unexpected command frame: {other:?}"),
            }
        }
    }

    async fn wait_blocked(&mut self) {
        tokio::time::timeout(Duration::from_secs(2), self.started.recv())
            .await
            .expect("native source must reach the blocked read")
            .unwrap();
    }

    fn files(&self, operation: FileOperation) -> EnvironmentCommand {
        EnvironmentCommand::Files {
            session_id: fixture_session("ready"),
            workdir: self.source.workdir.clone(),
            operation,
        }
    }

    async fn disconnect(mut self) {
        self.incoming.take();
        let result = tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .expect("disconnect must cancel blocked commands")
            .expect("connection task must not panic");
        assert!(result.is_err());
        assert_eq!(self.source.active.load(Ordering::SeqCst), 0);
        self.runtime
            .handle_command(EnvironmentCommand::Close {
                session_id: Some(fixture_session("ordered")),
            })
            .await;
    }
}

fn reread() -> EnvironmentCommand {
    EnvironmentCommand::Reread {
        session_id: fixture_session("ready"),
        agent: "fixture".into(),
        native_id: NativeId::new("ready"),
    }
}

#[tokio::test]
async fn blocked_native_history_keeps_files_and_health_responsive() {
    let mut harness = CommandHarness::new().await;
    harness.send("history", reread()).await;
    harness.wait_blocked().await;
    harness
        .send(
            "files",
            harness.files(FileOperation::Read {
                relative: "visible.txt".into(),
                limit: None,
            }),
        )
        .await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "files");
    let CommandOutcome::Accepted {
        result: Some(result),
    } = outcome
    else {
        panic!("workspace file must be readable while history is blocked: {outcome:?}")
    };
    assert_eq!(result["data"], "bmF0aXZlIHdvcmtzcGFjZQ==");
    harness.send("health", EnvironmentCommand::Health).await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "health");
    assert!(matches!(outcome, CommandOutcome::Accepted { .. }));
    assert_eq!(harness.source.active.load(Ordering::SeqCst), 1);
    harness.disconnect().await;
}

#[tokio::test]
async fn acknowledgement_bursts_do_not_fill_the_command_queue() {
    let (agent_stream, peer_stream) = tokio::io::duplex(8192);
    let (sender, _outgoing) = mpsc::channel(2);
    let sink = Arc::new(ChannelEventSink::new(
        sender,
        EnvironmentId::parse("env7fk2").unwrap(),
        ProducerId::parse("producer-a").unwrap(),
        1,
    ));
    let (incoming, mut commands) = mpsc::channel(2);
    let reader = tokio::spawn(reader_loop(agent_stream, sink, incoming));
    let mut peer = FramedConnection::new(peer_stream);
    for sequence in 1..=commands.max_capacity() * 3 {
        peer.write_env_message(&WireMessage::Ack {
            producer_id: ProducerId::parse("producer-a").unwrap(),
            accepted_through: sylph_protocol::ProducerSeq::new(sequence as u64),
            server_seq_through: None,
        })
        .await
        .unwrap();
    }
    peer.write_env_message(&WireMessage::Command {
        request_id: CommandRequestId::parse("health-after-acks").unwrap(),
        idempotency: IdempotencyPolicy::Replayable,
        command: EnvironmentCommand::Health,
    })
    .await
    .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), commands.recv())
        .await
        .unwrap();
    assert!(matches!(message,
        Some(sylph_protocol::DecodedMessage::Known(WireMessage::Command { request_id, .. }))
            if request_id.as_str() == "health-after-acks"));
    drop(peer);
    assert!(reader.await.unwrap().is_err());
}

#[tokio::test]
async fn saturated_history_rejects_excess_work_without_consuming_interactive_capacity() {
    let mut harness = CommandHarness::new().await;
    for index in 0..MAX_HISTORY_COMMANDS {
        harness.send(&format!("history-{index}"), reread()).await;
        harness.wait_blocked().await;
    }
    harness.send("excess-history", reread()).await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "excess-history");
    assert!(
        matches!(outcome, CommandOutcome::Rejected { code, .. } if code == "COMMAND_OVERLOADED")
    );
    assert_eq!(
        harness.source.active.load(Ordering::SeqCst),
        MAX_HISTORY_COMMANDS
    );
    harness
        .send(
            "files",
            harness.files(FileOperation::List {
                relative: String::new(),
            }),
        )
        .await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "files");
    assert!(matches!(outcome, CommandOutcome::Accepted { .. }));
    harness.send("health", EnvironmentCommand::Health).await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "health");
    assert!(matches!(outcome, CommandOutcome::Accepted { .. }));
    harness.disconnect().await;
}

#[tokio::test]
async fn session_mutations_keep_wire_order_across_a_blocked_watch() {
    let mut harness = CommandHarness::new().await;
    let session_id = fixture_session("ordered");
    harness
        .send(
            "watch",
            EnvironmentCommand::Watch {
                session_id: session_id.clone(),
                agent: "fixture".into(),
                native_id: NativeId::new("ordered"),
                turn_id: None,
            },
        )
        .await;
    harness.wait_blocked().await;
    harness
        .send(
            "close",
            EnvironmentCommand::Close {
                session_id: Some(session_id.clone()),
            },
        )
        .await;
    harness
        .send("activate", EnvironmentCommand::Activate { session_id })
        .await;
    harness.send("health", EnvironmentCommand::Health).await;
    let (request_id, outcome) = harness.result().await;
    assert_eq!(
        request_id, "health",
        "later mutations must wait for the blocked watch"
    );
    assert!(matches!(outcome, CommandOutcome::Accepted { .. }));
    harness.source.release.add_permits(1);
    for expected in ["watch", "close"] {
        let (request_id, outcome) = harness.result().await;
        assert_eq!(request_id, expected);
        assert!(matches!(outcome, CommandOutcome::Accepted { .. }));
    }
    let (request_id, outcome) = harness.result().await;
    assert_eq!(request_id, "activate");
    assert!(
        matches!(outcome, CommandOutcome::Rejected { code, .. } if code == "SESSION_NOT_HOSTED")
    );
    harness.disconnect().await;
}

#[tokio::test]
async fn disconnect_cancels_native_reads_and_releases_ack_waiters() {
    let mut harness = CommandHarness::new().await;
    harness.send("history", reread()).await;
    harness.wait_blocked().await;
    let sink = harness.sink.clone();
    let delivery = tokio::spawn(async move {
        sink.deliver(&[SpoolEvent {
            producer_id: "producer-a".into(),
            producer_seq: 1,
            kind: "event".into(),
            session_id: fixture_session("ready").encode(),
            arrival_turn_id: None,
            payload: serde_json::to_vec(&Event::new(EventKind::Status, None, 1)).unwrap(),
            source_cursor: None,
            recorded_at_ms: None,
        }])
        .await
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), harness.outgoing.recv())
            .await
            .unwrap(),
        Some(WireMessage::EventBatch { .. })
    ));
    harness.disconnect().await;
    assert!(tokio::time::timeout(Duration::from_secs(2), delivery)
        .await
        .expect("connection close must release the unacknowledged delivery")
        .unwrap()
        .is_err());
}
