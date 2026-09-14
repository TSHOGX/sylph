use super::*;
use sylph_protocol::{
    CommandRequestId, EnvironmentCommand, FileOperation, NativeTransferOperation,
};

struct ReadFixture {
    bridge: EnvBridge,
    environment: EnvironmentId,
    incoming: mpsc::Receiver<WireMessage>,
    epoch: u64,
    _root: tempfile::TempDir,
}

impl ReadFixture {
    async fn new(capacity: usize) -> Self {
        let (bridge, root) = bridge().await;
        let environment = EnvironmentId::parse("env7fk2").unwrap();
        bridge
            .store
            .upsert_environment(
                &environment,
                "read-fixture",
                "fixture",
                "development",
                &hash_secret("token"),
                "[]",
            )
            .await
            .unwrap();
        let (sender, incoming) = mpsc::channel(capacity);
        let hello = sylph_protocol::Hello::current(
            "read-fixture",
            ProducerId::parse("producer").unwrap(),
            Some(environment.clone()),
            0,
        );
        let welcome = bridge
            .register_connection(&hello, "token", sender)
            .await
            .unwrap();
        Self {
            bridge,
            environment,
            incoming,
            epoch: welcome.connection_epoch,
            _root: root,
        }
    }

    fn request(
        &self,
        command: EnvironmentCommand,
        timeout: std::time::Duration,
    ) -> tokio::task::JoinHandle<Result<CommandOutcome>> {
        let bridge = self.bridge.clone();
        let environment = self.environment.clone();
        tokio::spawn(async move {
            bridge
                .dispatch_and_wait(
                    &environment,
                    Command::new(
                        CommandRequestId::parse("caller-read-id").unwrap(),
                        IdempotencyPolicy::AtMostOnce,
                        command,
                    ),
                    timeout,
                )
                .await
        })
    }

    async fn command(&mut self) -> (CommandRequestId, IdempotencyPolicy) {
        let message = tokio::time::timeout(std::time::Duration::from_secs(2), self.incoming.recv())
            .await
            .unwrap()
            .unwrap();
        let WireMessage::Command {
            request_id,
            idempotency,
            ..
        } = message
        else {
            panic!("expected a dispatched read, got {message:?}");
        };
        (request_id, idempotency)
    }

    async fn result(&self, request_id: CommandRequestId, outcome: CommandOutcome) {
        self.bridge
            .record_command_result(
                &self.environment,
                &sylph_protocol::CommandResultMessage {
                    request_id,
                    outcome,
                    connection_epoch: self.epoch,
                    idempotency: Some(IdempotencyPolicy::Replayable),
                },
            )
            .await
            .unwrap();
    }

    async fn ledger(&self) -> (i64, i64) {
        sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(length(outcome_json)),0) FROM env_command_results WHERE environment_id=?")
            .bind(self.environment.as_str()).fetch_one(self.bridge.store.raw_pool()).await.unwrap()
    }
}

fn transfer_read() -> EnvironmentCommand {
    EnvironmentCommand::NativeTransfer {
        session_id: session("native-read"),
        operation: NativeTransferOperation::Read {
            id: "transfer".into(),
            offset: 0,
        },
    }
}

#[tokio::test]
async fn native_chunk_bytes_reach_the_waiter_without_persistent_command_payloads() {
    let mut fixture = ReadFixture::new(4).await;
    let waiter = fixture.request(transfer_read(), std::time::Duration::from_secs(2));
    let (request_id, policy) = fixture.command().await;
    assert_eq!(policy, IdempotencyPolicy::Replayable);
    assert_eq!(
        fixture.ledger().await,
        (0, 0),
        "reads must not persist an unknown receipt either"
    );
    let bytes: Vec<u8> = (0..=255)
        .cycle()
        .take(sylph_protocol::NATIVE_TRANSFER_CHUNK_BYTES)
        .collect();
    let outcome = CommandOutcome::accepted_with_result(
        serde_json::to_value(sylph_protocol::NativeTransferChunk {
            next_offset: bytes.len() as u64,
            bytes: bytes.clone(),
        })
        .unwrap(),
    );
    fixture.result(request_id.clone(), outcome.clone()).await;
    let CommandOutcome::Accepted {
        result: Some(result),
    } = waiter.await.unwrap().unwrap()
    else {
        panic!("the read must deliver its actual bytes");
    };
    let chunk: sylph_protocol::NativeTransferChunk = serde_json::from_value(result).unwrap();
    assert_eq!(chunk.bytes, bytes);
    assert_eq!(chunk.next_offset, bytes.len() as u64);
    fixture.result(request_id, outcome).await;
    assert_eq!(fixture.ledger().await, (0, 0));
    assert!(fixture.bridge.read_waiters.lock().unwrap().is_empty());
}

#[tokio::test]
async fn catalogue_files_and_capability_reads_never_create_command_receipts() {
    let mut fixture = ReadFixture::new(4).await;
    let mut commands = vec![
        EnvironmentCommand::Health,
        EnvironmentCommand::Enumerate {
            agent: "claude".into(),
            workdir: None,
        },
        EnvironmentCommand::Capabilities {
            session_id: session("native-read"),
        },
        EnvironmentCommand::DraftCapabilities {
            draft_id: "draft".into(),
        },
        EnvironmentCommand::BrowseRoots,
        EnvironmentCommand::BrowseTree { path: None },
    ];
    for operation in [
        FileOperation::Read {
            relative: "file".into(),
            limit: None,
        },
        FileOperation::List {
            relative: String::new(),
        },
        FileOperation::Stat {
            relative: "file".into(),
        },
    ] {
        commands.push(EnvironmentCommand::Files {
            session_id: session("native-read"),
            workdir: "/tmp".into(),
            operation: operation.clone(),
        });
        commands.push(EnvironmentCommand::DraftFiles {
            draft_id: "draft".into(),
            operation,
        });
    }
    for command in commands {
        let waiter = fixture.request(command, std::time::Duration::from_secs(2));
        let (request_id, _) = fixture.command().await;
        let outcome =
            CommandOutcome::accepted_with_result(serde_json::json!({"read": "fresh value"}));
        fixture.result(request_id, outcome.clone()).await;
        assert_eq!(waiter.await.unwrap().unwrap(), outcome);
        assert_eq!(fixture.ledger().await, (0, 0));
    }
}

#[tokio::test]
async fn timed_out_read_replies_cannot_recreate_a_durable_result() {
    let mut fixture = ReadFixture::new(4).await;
    let waiter = fixture.request(transfer_read(), std::time::Duration::from_millis(20));
    let (request_id, _) = fixture.command().await;
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        CommandOutcome::Unknown { .. }
    ));
    assert!(fixture.bridge.read_waiters.lock().unwrap().is_empty());
    fixture
        .result(
            request_id,
            CommandOutcome::accepted_with_result(serde_json::json!({"late": "x".repeat(100_000)})),
        )
        .await;
    assert_eq!(fixture.ledger().await, (0, 0));
}

#[tokio::test]
async fn disconnect_releases_read_waiters_and_discards_late_replies() {
    let mut fixture = ReadFixture::new(4).await;
    let waiter = fixture.request(transfer_read(), std::time::Duration::from_secs(2));
    let (request_id, _) = fixture.command().await;
    fixture
        .bridge
        .disconnect(&fixture.environment, fixture.epoch)
        .await
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap().unwrap(),
        CommandOutcome::Unknown { .. }
    ));
    assert!(fixture.bridge.read_waiters.lock().unwrap().is_empty());
    fixture.result(request_id, CommandOutcome::accepted()).await;
    assert_eq!(fixture.ledger().await, (0, 0));
}

#[tokio::test]
async fn cancelling_a_read_future_releases_its_registration() {
    let mut fixture = ReadFixture::new(4).await;
    let waiter = fixture.request(transfer_read(), std::time::Duration::from_secs(2));
    let (request_id, _) = fixture.command().await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(fixture.bridge.read_waiters.lock().unwrap().is_empty());
    fixture.result(request_id, CommandOutcome::accepted()).await;
    assert_eq!(fixture.ledger().await, (0, 0));
}

#[tokio::test]
async fn the_read_deadline_includes_waiting_for_a_socket_queue_slot() {
    let mut fixture = ReadFixture::new(1).await;
    fixture
        .bridge
        .leases
        .read()
        .await
        .get(&fixture.environment)
        .unwrap()
        .sender
        .send(WireMessage::Heartbeat {
            connection_epoch: fixture.epoch,
            sent_at_ms: 1,
        })
        .await
        .unwrap();
    let waiter = fixture.request(transfer_read(), std::time::Duration::from_millis(20));
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("a blocked socket queue must not outlive the read deadline")
        .unwrap()
        .unwrap();
    assert!(matches!(result, CommandOutcome::Unknown { .. }));
    assert!(matches!(
        fixture.incoming.recv().await,
        Some(WireMessage::Heartbeat { .. })
    ));
    assert!(fixture.incoming.try_recv().is_err());
    assert!(fixture.bridge.read_waiters.lock().unwrap().is_empty());
    assert_eq!(fixture.ledger().await, (0, 0));
}

#[tokio::test]
async fn at_most_once_writes_keep_the_durable_ledger_and_cannot_use_read_identities() {
    let mut fixture = ReadFixture::new(4).await;
    let command = EnvironmentCommand::Files {
        session_id: session("native-read"),
        workdir: "/tmp".into(),
        operation: FileOperation::Write {
            relative: "file".into(),
            data: "Ynl0ZXM=".into(),
            overwrite: false,
        },
    };
    let bridge = fixture.bridge.clone();
    let environment = fixture.environment.clone();
    let pending_command = command.clone();
    let waiter = tokio::spawn(async move {
        bridge
            .dispatch_and_wait(
                &environment,
                Command::new(
                    CommandRequestId::parse("durable-write").unwrap(),
                    IdempotencyPolicy::AtMostOnce,
                    pending_command,
                ),
                std::time::Duration::from_secs(2),
            )
            .await
    });
    let (request_id, policy) = fixture.command().await;
    assert_eq!(policy, IdempotencyPolicy::AtMostOnce);
    assert!(matches!(
        fixture
            .bridge
            .store
            .command_outcome(&fixture.environment, request_id.as_str())
            .await
            .unwrap(),
        Some(CommandOutcome::Unknown { .. })
    ));
    let outcome = CommandOutcome::accepted_with_result(serde_json::json!({"written": 5}));
    fixture
        .bridge
        .record_command_result(
            &fixture.environment,
            &sylph_protocol::CommandResultMessage {
                request_id: request_id.clone(),
                outcome: outcome.clone(),
                connection_epoch: fixture.epoch,
                idempotency: Some(policy),
            },
        )
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), outcome);
    assert_eq!(
        fixture
            .bridge
            .store
            .command_outcome(&fixture.environment, request_id.as_str())
            .await
            .unwrap(),
        Some(outcome.clone())
    );
    assert_eq!(
        fixture
            .bridge
            .dispatch_and_wait(
                &fixture.environment,
                Command::new(request_id, policy, command.clone(),),
                std::time::Duration::from_secs(2)
            )
            .await
            .unwrap(),
        outcome
    );
    assert!(fixture.incoming.try_recv().is_err());
    assert!(fixture
        .bridge
        .dispatch_and_wait(
            &fixture.environment,
            Command::new(
                CommandRequestId::parse("read-reserved-for-stateless").unwrap(),
                policy,
                command,
            ),
            std::time::Duration::from_secs(2)
        )
        .await
        .is_err());
    assert_eq!(fixture.ledger().await.0, 1);
}
