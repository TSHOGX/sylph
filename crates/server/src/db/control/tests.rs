use super::*;
use sylph_protocol::SessionId;

#[tokio::test]
async fn first_run_authority_is_not_limited_to_registered_environment_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir.path().join("control.db")).await;
    assert!(store
        .has_capability("bootstrap", "unlisted-retained-environment", "observe")
        .await
        .unwrap());
    assert_eq!(
        store.capable_environments("bootstrap", "observe").await.unwrap(),
        crate::db::metadata::CapabilityPartition::Unrestricted
    );
    store
        .upsert_subject("registered", "Registered reader", "opaque-token-hash", false)
        .await
        .unwrap();
    assert!(!store
        .has_capability("bootstrap", "unlisted-retained-environment", "observe")
        .await
        .unwrap());
    assert_eq!(
        store.capable_environments("bootstrap", "observe").await.unwrap(),
        crate::db::metadata::CapabilityPartition::Environments(Default::default())
    );
}

#[tokio::test]
async fn queued_edits_keep_original_bytes_and_dispatch_the_exact_selected_immutable_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = ControlStore::open(
        &dir.path().join("control.db"),
        Issuer::Server("test".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap();
    let mut input = acceptance("queued-edit");
    input.kind = CommandKind::Queue;
    let original = input.input.clone();
    store.accept(input).await.unwrap();
    let updated = SubmittedRevision {
        revision: "revision-two".into(),
        media_type: "text/plain".into(),
        bytes: b"edited queued input".to_vec(),
    };
    let edited = store
        .revise_queued("queued-edit", 0, updated.clone(), authorization())
        .await
        .unwrap();
    assert_eq!(edited.current_revision, 1);
    assert_eq!(edited.acceptance.input, original);
    assert_eq!(edited.current_input, updated);
    assert!(store
        .revise_queued("queued-edit", 0, original.clone(), authorization())
        .await
        .is_err());
    let permit = store
        .claim_dispatch("queued-edit", authorization(), 12)
        .await
        .unwrap();
    assert_eq!(permit.command.input, updated);
    assert!(store
        .record_delivery(
            "queued-edit",
            "wrong-consumption",
            DeliveryFact::Consumed {
                revision: original.revision.clone(),
                receipt: b"wrong revision".to_vec(),
            },
            13
        )
        .await
        .is_err());
    store
        .record_delivery(
            "queued-edit",
            "current-consumption",
            DeliveryFact::Consumed {
                revision: updated.revision.clone(),
                receipt: b"current revision".to_vec(),
            },
            14,
        )
        .await
        .unwrap();
    assert_eq!(
        store.command("queued-edit").await.unwrap().unwrap().state,
        CommandState::Consumed
    );
    assert!(store
        .revise_queued("queued-edit", 1, original.clone(), authorization())
        .await
        .is_err());
    assert!(store
        .cancel_queued("queued-edit", 1, "subject-a")
        .await
        .is_err());
    let original_fact = store
        .pending()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.capture.observation_ref.producer_seq == 1)
        .unwrap();
    let OperationalFact::Accepted { command } =
        serde_json::from_slice(&original_fact.payload).unwrap()
    else {
        panic!()
    };
    assert_eq!(command.input, original);
    let facts: Vec<OperationalFact> = store
        .pending()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| serde_json::from_slice(&entry.payload).unwrap())
        .collect();
    assert!(facts
        .iter()
        .any(|fact| matches!(fact,OperationalFact::Revised{previous_revision,input,..}if input==&updated && previous_revision==&original.revision)));
    assert!(facts.iter().any(|fact|matches!(fact,OperationalFact::Dispatched{input_revision,..}if input_revision=="revision-two")));
}

#[tokio::test]
async fn queued_cancellation_requires_the_current_revision_and_prevents_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = ControlStore::open(
        &dir.path().join("control.db"),
        Issuer::Server("test".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap();
    let mut input = acceptance("queued-cancel");
    input.kind = CommandKind::Queue;
    store.accept(input).await.unwrap();
    assert!(store
        .cancel_queued("queued-cancel", 1, "subject-a")
        .await
        .is_err());
    assert!(store
        .cancel_queued("queued-cancel", 0, "another-subject")
        .await
        .is_err());
    let result = store
        .cancel_queued("queued-cancel", 0, "subject-a")
        .await
        .unwrap();
    assert_eq!(result.state, CommandState::Cancelled);
    assert!(store
        .claim_dispatch("queued-cancel", authorization(), 13)
        .await
        .is_err());
    assert!(store.queued_commands(0, 32).await.unwrap().is_empty());
}

#[tokio::test]
async fn authorized_input_deletion_waits_for_export_and_never_erases_a_later_readoption() {
    let dir = tempfile::tempdir().unwrap();
    let store = ControlStore::open(
        &dir.path().join("control.db"),
        Issuer::Server("test".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap();
    let first = store.accept(acceptance("delete-me")).await.unwrap();
    let agent = first.acceptance.target.agent().unwrap().clone();
    assert_eq!(store.erase_agent_input(&agent).await.unwrap(), 0);
    assert!(store.command("delete-me").await.unwrap().is_some());
    let second = store
        .accept(acceptance("after-deletion-cut"))
        .await
        .unwrap();
    store
        .mark_exported(
            &first.accepted_observation,
            ExportDisposition::DiscardedByDeletion,
        )
        .await
        .unwrap();
    store
        .mark_exported(
            &second.accepted_observation,
            ExportDisposition::Retained { admission_cut: 2 },
        )
        .await
        .unwrap();
    store.erase_pending_inputs().await.unwrap();
    assert!(store.command("delete-me").await.unwrap().is_none());
    assert_eq!(
        store
            .command("after-deletion-cut")
            .await
            .unwrap()
            .unwrap()
            .acceptance
            .input
            .bytes,
        second.acceptance.input.bytes
    );
    assert!(store.accept(acceptance("delete-me")).await.is_err());
    let bodies: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_outbox WHERE command_id='delete-me'")
            .fetch_one(&store.readers)
            .await
            .unwrap();
    assert_eq!(bodies, 0);
}

fn authorization() -> AuthorizationReceipt {
    AuthorizationReceipt {
        subject: "subject-a".into(),
        decision_ref: "decision-a".into(),
        lease_ref: Some("lease-a".into()),
        evaluated_at_ms: 10,
    }
}

#[tokio::test]
async fn control_writer_discards_an_acknowledged_begin_without_a_transaction_guard() {
    let dir=tempfile::tempdir().unwrap();
    let store=store(&dir.path().join("control.db")).await;
    crate::db::sqlite::assert_abandoned_begin_is_discarded(&store.pool).await;
}

fn acceptance(id: &str) -> CommandAcceptance {
    CommandAcceptance {
        command_id: id.into(),
        target: CommandTarget::Agent {
            agent: AgentRef::Session(SessionId::decode("env:codex:Root:Case").unwrap()),
        },
        kind: CommandKind::Send,
        input: SubmittedRevision {
            revision: "input-v1".into(),
            media_type: "application/octet-stream".into(),
            bytes: b"Human input\x00\xff\xe4\xbd\xa0\xe5\xa5\xbd\r\n".to_vec(),
        },
        authorization: authorization(),
        ingestion_authorization_epoch: IngestionEpoch::new("admission-original").unwrap(),
        retry_policy: RetryPolicy::AtMostOnce,
        idempotency_key: Some(format!("request:{id}")),
        accepted_at_ms: 20,
    }
}

async fn store(path: &Path) -> ControlStore {
    ControlStore::open(
        path,
        Issuer::Server("server-a".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn accepted_bytes_authorization_and_occurrence_survive_reopen_under_full_sync() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&control.pool)
            .await
            .unwrap(),
        2
    );
    let accepted = control.accept(acceptance("command-a")).await.unwrap();
    let pending = control.pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].capture.observation_ref.producer_seq, 1);
    control.close().await;
    let control = store(&path).await;
    let actual = control.command("command-a").await.unwrap().unwrap();
    assert_eq!(actual, accepted);
    assert_eq!(
        actual.acceptance.input.bytes,
        b"Human input\x00\xff\xe4\xbd\xa0\xe5\xa5\xbd\r\n"
    );
    assert_eq!(
        actual.acceptance.authorization.lease_ref.as_deref(),
        Some("lease-a")
    );
    assert_eq!(control.pending().await.unwrap(), pending);
    let fact: OperationalFact = serde_json::from_slice(&pending[0].payload).unwrap();
    assert!(matches!(fact, OperationalFact::Accepted { command } if *command == actual.acceptance));
}

#[tokio::test]
async fn failed_acceptance_commits_neither_input_nor_observation_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    sqlx::raw_sql("CREATE TRIGGER reject_outbox BEFORE INSERT ON control_outbox BEGIN SELECT RAISE(ABORT, 'acceptance fault'); END;")
        .execute(&control.pool).await.unwrap();
    assert!(control.accept(acceptance("command-a")).await.is_err());
    assert!(control.command("command-a").await.unwrap().is_none());
    assert!(control.pending().await.unwrap().is_empty());
    assert_eq!(control.progress(30).await.unwrap().pending_count, 0);
    sqlx::query("DROP TRIGGER reject_outbox")
        .execute(&control.pool)
        .await
        .unwrap();
    assert_eq!(
        control
            .accept(acceptance("command-a"))
            .await
            .unwrap()
            .accepted_observation
            .producer_seq,
        1
    );
}

#[tokio::test]
async fn idempotent_retry_retains_first_acceptance_and_rejects_changed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let command = acceptance("command-a");
    let first = control.accept(command.clone()).await.unwrap();
    let mut retry = command;
    retry.command_id = "newly-generated-command-id".into();
    retry.accepted_at_ms = 300;
    retry.authorization.lease_ref = Some("lease-new".into());
    assert_eq!(control.accept(retry.clone()).await.unwrap(), first);
    retry.input.bytes.push(b'!');
    assert!(matches!(
        control.accept(retry).await,
        Err(OutboxError::IdentityConflict)
    ));
    assert_eq!(control.pending().await.unwrap().len(), 1);
}

#[tokio::test]
async fn equal_submissions_have_distinct_command_and_source_occurrences() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let first = control.accept(acceptance("command-a")).await.unwrap();
    let second = control.accept(acceptance("command-b")).await.unwrap();
    assert_eq!(first.acceptance.input, second.acceptance.input);
    let pending = control.pending().await.unwrap();
    assert_eq!(pending.len(), 2);
    assert_ne!(
        pending[0].capture.source_position,
        pending[1].capture.source_position
    );
    assert_ne!(first.accepted_observation, second.accepted_observation);
}

#[tokio::test]
async fn lost_at_most_once_dispatch_stays_unknown_and_cannot_execute_again() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    control.accept(acceptance("command-a")).await.unwrap();
    let permit = control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    assert_eq!(permit.attempt, 1);
    control.close().await;
    let control = store(&path).await;
    assert_eq!(control.recover_inflight(40).await.unwrap(), 1);
    assert_eq!(control.recover_inflight(41).await.unwrap(), 0);
    let recovered = control.command("command-a").await.unwrap().unwrap();
    assert_eq!(recovered.state, CommandState::Unknown);
    assert_eq!(recovered.dispatch_attempts, 1);
    assert!(matches!(
        control
            .claim_dispatch("command-a", authorization(), 50)
            .await,
        Err(OutboxError::InvalidTransition)
    ));
    assert_eq!(control.pending().await.unwrap().len(), 3);
}

#[tokio::test]
async fn native_idempotency_retry_reuses_the_same_key_and_exact_input() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let mut command = acceptance("command-a");
    command.retry_policy = RetryPolicy::VendorIdempotent {
        key: "native-idempotency-key".into(),
    };
    control.accept(command.clone()).await.unwrap();
    control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    control.recover_inflight(40).await.unwrap();
    let permit = control
        .claim_dispatch("command-a", authorization(), 50)
        .await
        .unwrap();
    assert_eq!(permit.command, command);
    assert_eq!(permit.attempt, 2);
}

#[tokio::test]
async fn simultaneous_dispatch_claims_yield_one_durable_vendor_permit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    let other_handle = store(&path).await;
    control.accept(acceptance("command-a")).await.unwrap();
    let (a, b) = tokio::join!(
        control.claim_dispatch("command-a", authorization(), 30),
        other_handle.claim_dispatch("command-a", authorization(), 30)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        control
            .command("command-a")
            .await
            .unwrap()
            .unwrap()
            .dispatch_attempts,
        1
    );
    assert_eq!(control.pending().await.unwrap().len(), 2);
}

#[tokio::test]
async fn fresh_authorization_cannot_dispatch_another_subjects_accepted_input() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control.accept(acceptance("command-a")).await.unwrap();
    let mut another_subject = authorization();
    another_subject.subject = "subject-b".into();
    assert!(matches!(
        control
            .claim_dispatch("command-a", another_subject, 30)
            .await,
        Err(OutboxError::Invalid(_))
    ));
    assert_eq!(
        control.command("command-a").await.unwrap().unwrap().state,
        CommandState::Accepted
    );
    assert!(sqlx::query("UPDATE trace_commands SET state = 'cancelled'")
        .execute(&control.readers)
        .await
        .is_err());
}

/// Invoked in an isolated test process. The parent kills it with both pools open,
/// at an explicit committed boundary, so recovery is measured from WAL bytes.
#[test]
fn process_kill_fixture() {
    let Ok(path) = std::env::var("SYLPH_OUTBOX_KILL_FIXTURE") else {
        return;
    };
    let stage = std::env::var("SYLPH_OUTBOX_KILL_STAGE").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        use std::io::Write;
        let control = store(Path::new(&path)).await;
        control.accept(acceptance("command-a")).await.unwrap();
        if stage == "dispatch" {
            control
                .claim_dispatch("command-a", authorization(), 30)
                .await
                .unwrap();
        }
        println!("CONTROL_COMMITTED");
        std::io::stdout().flush().unwrap();
        std::future::pending::<()>().await;
    });
}

#[tokio::test]
async fn process_kill_preserves_accepted_bytes_and_dispatch_ambiguity() {
    use tokio::io::AsyncBufReadExt;
    for stage in ["acceptance", "dispatch"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("control.db");
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::control::tests::process_kill_fixture",
                "--nocapture",
            ])
            .env("SYLPH_OUTBOX_KILL_FIXTURE", &path)
            .env("SYLPH_OUTBOX_KILL_STAGE", stage)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("child exited before its committed boundary");
                if line.contains("CONTROL_COMMITTED") {
                    break;
                }
            }
        })
        .await
        .expect("child did not commit within the test deadline");
        child.kill().await.unwrap();
        let control = store(&path).await;
        let recovered = control.command("command-a").await.unwrap().unwrap();
        assert_eq!(
            recovered.acceptance.input.bytes,
            b"Human input\x00\xff\xe4\xbd\xa0\xe5\xa5\xbd\r\n"
        );
        if stage == "dispatch" {
            assert_eq!(control.recover_inflight(40).await.unwrap(), 1);
            assert!(matches!(
                control
                    .claim_dispatch("command-a", authorization(), 50)
                    .await,
                Err(OutboxError::InvalidTransition)
            ));
            assert_eq!(
                control.command("command-a").await.unwrap().unwrap().state,
                CommandState::Unknown
            );
        } else {
            assert_eq!(control.recover_inflight(40).await.unwrap(), 0);
            assert_eq!(recovered.state, CommandState::Accepted);
            assert_eq!(
                control.pending().await.unwrap()[0]
                    .capture
                    .observation_ref
                    .producer_seq,
                1
            );
        }
    }
}

#[tokio::test]
async fn failed_dispatch_record_produces_no_permit_and_does_not_consume_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control.accept(acceptance("command-a")).await.unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_dispatch BEFORE INSERT ON control_outbox WHEN NEW.event_key LIKE 'dispatch:%' BEGIN SELECT RAISE(ABORT, 'dispatch fault'); END;")
        .execute(&control.pool).await.unwrap();
    assert!(control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .is_err());
    let actual = control.command("command-a").await.unwrap().unwrap();
    assert_eq!(actual.state, CommandState::Accepted);
    assert_eq!(actual.dispatch_attempts, 0);
    assert_eq!(control.pending().await.unwrap().len(), 1);
}

#[tokio::test]
async fn export_retries_fixed_observation_without_changing_dispatch_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    control.accept(acceptance("command-a")).await.unwrap();
    control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    let first = control.pending().await.unwrap();
    // Models admission committed in evidence.db followed by loss of the process
    // before control.db could mark export. The producer retries these exact bytes.
    control.close().await;
    let control = store(&path).await;
    let retry = control.pending().await.unwrap();
    assert_eq!(retry, first);
    for (index, occurrence) in retry.iter().enumerate() {
        let disposition = ExportDisposition::Retained {
            admission_cut: index as u64 + 1,
        };
        control
            .mark_exported(&occurrence.capture.observation_ref, disposition)
            .await
            .unwrap();
        control
            .mark_exported(&occurrence.capture.observation_ref, disposition)
            .await
            .unwrap();
    }
    assert!(control.pending().await.unwrap().is_empty());
    let state = control.command("command-a").await.unwrap().unwrap();
    assert_eq!(state.state, CommandState::Dispatching);
    assert_eq!(state.dispatch_attempts, 1);
    assert_eq!(
        control.progress(90).await.unwrap(),
        OutboxProgress {
            pending_count: 0,
            pending_bytes: 0,
            oldest_pending_age_ms: None
        }
    );
}

#[tokio::test]
async fn first_export_after_readoption_keeps_acceptance_epoch_and_deletion_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    let accepted = control.accept(acceptance("command-a")).await.unwrap();
    control.close().await;
    let control = store(&path).await;
    let pending = control.pending().await.unwrap();
    assert_eq!(
        pending[0].capture.ingestion_authorization_epoch.as_str(),
        "admission-original"
    );
    let mut readopted_retry = acceptance("command-a");
    readopted_retry.ingestion_authorization_epoch = IngestionEpoch::new("readopted-new").unwrap();
    assert!(matches!(
        control.accept(readopted_retry).await,
        Err(OutboxError::IdentityConflict)
    ));
    control
        .mark_exported(
            &accepted.accepted_observation,
            ExportDisposition::DiscardedByDeletion,
        )
        .await
        .unwrap();
    assert_eq!(control.progress(100).await.unwrap().pending_count, 0);
    assert_eq!(
        control
            .command("command-a")
            .await
            .unwrap()
            .unwrap()
            .dispatch_attempts,
        0
    );
    assert!(matches!(
        control
            .mark_exported(
                &accepted.accepted_observation,
                ExportDisposition::Retained { admission_cut: 99 }
            )
            .await,
        Err(OutboxError::ReceiptMismatch)
    ));
}

#[tokio::test]
async fn consumption_names_the_immutable_dispatched_revision_and_keeps_native_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control.accept(acceptance("command-a")).await.unwrap();
    control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    let receipt = b"{\"duplicate\":1,\"duplicate\":2}\r\n".to_vec();
    let fact = DeliveryFact::Consumed {
        revision: "input-v1".into(),
        receipt: receipt.clone(),
    };
    let observed = control
        .record_delivery("command-a", "native-consumed-1", fact.clone(), 40)
        .await
        .unwrap();
    assert_eq!(
        control
            .record_delivery("command-a", "native-consumed-1", fact, 50)
            .await
            .unwrap(),
        observed
    );
    assert_eq!(
        control.command("command-a").await.unwrap().unwrap().state,
        CommandState::Consumed
    );
    let records = control.pending().await.unwrap();
    let fact: OperationalFact = serde_json::from_slice(&records[2].payload).unwrap();
    assert!(
        matches!(fact, OperationalFact::Delivery { fact: DeliveryFact::Consumed { receipt: actual, .. }, .. } if actual == receipt)
    );
    assert!(matches!(
        control
            .record_delivery(
                "command-a",
                "native-consumed-1",
                DeliveryFact::Consumed {
                    revision: "input-v1".into(),
                    receipt: b"changed".to_vec()
                },
                60
            )
            .await,
        Err(OutboxError::IdentityConflict)
    ));
    assert!(matches!(
        control
            .record_delivery(
                "command-a",
                "different-revision",
                DeliveryFact::Consumed {
                    revision: "input-v2".into(),
                    receipt: vec![]
                },
                60
            )
            .await,
        Err(OutboxError::InvalidTransition)
    ));
}

#[tokio::test]
async fn accepted_revision_and_unexported_evidence_cannot_be_mutated_or_removed() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control.accept(acceptance("command-a")).await.unwrap();
    for mutation in [
        "UPDATE trace_commands SET acceptance_json = '{}'",
        "UPDATE control_outbox SET payload = 'changed'",
        "UPDATE control_outbox SET capture_json = '{}'",
        "DELETE FROM control_outbox",
        "DELETE FROM trace_commands",
    ] {
        assert!(
            sqlx::query(mutation).execute(&control.pool).await.is_err(),
            "{mutation}"
        );
    }
    assert_eq!(control.pending().await.unwrap().len(), 1);
}

#[tokio::test]
async fn contradictory_vendor_receipts_are_retained_and_cannot_authorize_retry() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let mut command = acceptance("command-a");
    command.retry_policy = RetryPolicy::VendorIdempotent {
        key: "native-key".into(),
    };
    control.accept(command).await.unwrap();
    control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    control
        .record_delivery(
            "command-a",
            "accepted",
            DeliveryFact::ReceiverAccepted {
                receipt: b"accepted".to_vec(),
            },
            40,
        )
        .await
        .unwrap();
    control
        .record_delivery(
            "command-a",
            "rejected",
            DeliveryFact::Rejected {
                receipt: b"rejected".to_vec(),
            },
            50,
        )
        .await
        .unwrap();
    control
        .record_delivery(
            "command-a",
            "unconfirmed",
            DeliveryFact::OutcomeUnknown {
                reason: "connection closed".into(),
            },
            60,
        )
        .await
        .unwrap();
    assert_eq!(
        control.command("command-a").await.unwrap().unwrap().state,
        CommandState::Conflicting
    );
    assert_eq!(control.pending().await.unwrap().len(), 5);
    assert!(matches!(
        control
            .claim_dispatch("command-a", authorization(), 70)
            .await,
        Err(OutboxError::InvalidTransition)
    ));
}

#[tokio::test]
async fn outbox_capacity_is_atomic_and_export_releases_only_its_own_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let limits = OutboxLimits {
        max_pending_facts: 2,
        max_export_items: 1,
        ..OutboxLimits::default()
    };
    let control = ControlStore::open(
        &temp.path().join("control.db"),
        Issuer::Server("server-a".into()),
        limits,
    )
    .await
    .unwrap();
    control.accept(acceptance("command-a")).await.unwrap();
    control.accept(acceptance("command-b")).await.unwrap();
    let progress = control.progress(90).await.unwrap();
    assert_eq!(progress.pending_count, control.limits().max_pending_facts);
    assert_eq!(progress.oldest_pending_age_ms, Some(70));
    assert!(matches!(
        control.accept(acceptance("command-c")).await,
        Err(OutboxError::Capacity)
    ));
    assert!(control.command("command-c").await.unwrap().is_none());
    assert_eq!(control.progress(90).await.unwrap(), progress);
    let pending = control.pending().await.unwrap();
    assert_eq!(pending.len(), control.limits().max_export_items);
    control
        .mark_exported(
            &pending[0].capture.observation_ref,
            ExportDisposition::Retained { admission_cut: 1 },
        )
        .await
        .unwrap();
    assert!(control.progress(90).await.unwrap().pending_bytes < progress.pending_bytes);
    let next = control.accept(acceptance("command-c")).await.unwrap();
    assert_eq!(next.accepted_observation.producer_seq, 3);
    assert_eq!(control.pending().await.unwrap()[0].command_id, "command-b");
}

#[tokio::test]
async fn oversized_input_and_forged_export_receipt_do_not_change_durable_state() {
    let temp = tempfile::tempdir().unwrap();
    let limits = OutboxLimits {
        max_input_bytes: 64,
        ..OutboxLimits::default()
    };
    let control = ControlStore::open(
        &temp.path().join("control.db"),
        Issuer::Server("server-a".into()),
        limits,
    )
    .await
    .unwrap();
    let mut large = acceptance("command-large");
    large.input.bytes = vec![b'x'; control.limits().max_input_bytes + 1];
    assert!(matches!(
        control.accept(large).await,
        Err(OutboxError::TooLarge)
    ));
    let accepted = control.accept(acceptance("command-a")).await.unwrap();
    let mut forged = accepted.accepted_observation;
    forged.producer_epoch = "another-runtime".into();
    assert!(matches!(
        control
            .mark_exported(&forged, ExportDisposition::Retained { admission_cut: 1 })
            .await,
        Err(OutboxError::ReceiptMismatch)
    ));
    assert_eq!(control.progress(90).await.unwrap().pending_count, 1);
}

#[tokio::test]
async fn evidence_commit_before_export_mark_replays_without_new_admission_or_dispatch() {
    use crate::{
        trace::cas::BlobStore,
        trace::evidence::{AdmissionLimits, EvidenceStore, ReceiptDisposition},
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let evidence = EvidenceStore::open(
        temp.path().join("evidence.db"),
        BlobStore::new(temp.path().join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap();
    let control = store(&path).await;
    let mut command = acceptance("command-a");
    command.ingestion_authorization_epoch = evidence
        .authorize_scope(
            &command.target.ingestion_scope(),
            &command.target.declared_subjects(),
        )
        .await
        .unwrap();
    control.accept(command).await.unwrap();
    control
        .claim_dispatch("command-a", authorization(), 30)
        .await
        .unwrap();
    let pending = control.pending().await.unwrap();
    for occurrence in &pending {
        assert_eq!(
            evidence.stage_payload(&occurrence.payload).await.unwrap(),
            occurrence.capture.payload_ref
        );
    }
    let captures: Vec<_> = pending
        .iter()
        .map(|occurrence| occurrence.capture.clone())
        .collect();
    let admitted = evidence
        .admit_batch(&Issuer::Server("server-a".into()), &captures)
        .await
        .unwrap();
    assert_eq!(admitted.evidence_cut, 2);
    control.close().await;
    let control = store(&path).await;
    assert_eq!(control.pending().await.unwrap(), pending);
    let replayed = evidence
        .admit_batch(&Issuer::Server("server-a".into()), &captures)
        .await
        .unwrap();
    assert_eq!(replayed.evidence_cut, admitted.evidence_cut);
    assert!(replayed
        .receipts
        .iter()
        .all(|receipt| !receipt.newly_recorded));
    for receipt in replayed.receipts {
        let ReceiptDisposition::Retained { admission_seq } = receipt.disposition else {
            panic!("retained command was discarded");
        };
        control
            .mark_exported(
                &receipt.observation_ref,
                ExportDisposition::Retained {
                    admission_cut: admission_seq,
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(control.progress(60).await.unwrap().pending_count, 0);
    let command = control.command("command-a").await.unwrap().unwrap();
    assert_eq!(command.state, CommandState::Dispatching);
    assert_eq!(command.dispatch_attempts, 1);
    assert!(matches!(
        control
            .claim_dispatch("command-a", authorization(), 60)
            .await,
        Err(OutboxError::InvalidTransition)
    ));
}

#[tokio::test]
async fn deletion_discards_never_exported_old_epoch_while_unrelated_command_advances() {
    use crate::{
        trace::cas::BlobStore,
        trace::evidence::{AdmissionLimits, EvidenceStore, ReceiptDisposition},
    };
    let temp = tempfile::tempdir().unwrap();
    let evidence = EvidenceStore::open(
        temp.path().join("evidence.db"),
        BlobStore::new(temp.path().join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let mut old = acceptance("command-a");
    old.ingestion_authorization_epoch = evidence
        .authorize_scope(
            &old.target.ingestion_scope(),
            &old.target.declared_subjects(),
        )
        .await
        .unwrap();
    control.accept(old.clone()).await.unwrap();
    evidence
        .delete_scope(&old.target.ingestion_scope())
        .await
        .unwrap();
    let readopted = evidence
        .readopt_scope(
            &old.target.ingestion_scope(),
            &old.target.declared_subjects(),
        )
        .await
        .unwrap();
    assert_ne!(readopted, old.ingestion_authorization_epoch);
    let mut next = acceptance("command-b");
    next.target = CommandTarget::Agent {
        agent: AgentRef::Session(SessionId::decode("env:codex:OtherSession").unwrap()),
    };
    next.ingestion_authorization_epoch = evidence
        .authorize_scope(
            &next.target.ingestion_scope(),
            &next.target.declared_subjects(),
        )
        .await
        .unwrap();
    control.accept(next).await.unwrap();
    let pending = control.pending().await.unwrap();
    for occurrence in &pending {
        evidence.stage_payload(&occurrence.payload).await.unwrap();
    }
    let captures: Vec<_> = pending
        .iter()
        .map(|occurrence| occurrence.capture.clone())
        .collect();
    let admitted = evidence
        .admit_batch(&Issuer::Server("server-a".into()), &captures)
        .await
        .unwrap();
    assert_eq!(admitted.accepted_through, 2);
    assert_eq!(admitted.evidence_cut, 1);
    assert!(matches!(
        admitted.receipts[0].disposition,
        ReceiptDisposition::DiscardedByDeletion { .. }
    ));
    assert!(matches!(
        admitted.receipts[1].disposition,
        ReceiptDisposition::Retained { admission_seq: 1 }
    ));
    for receipt in admitted.receipts {
        let disposition = match receipt.disposition {
            ReceiptDisposition::Retained { admission_seq } => ExportDisposition::Retained {
                admission_cut: admission_seq,
            },
            ReceiptDisposition::DiscardedByDeletion { .. } => {
                ExportDisposition::DiscardedByDeletion
            }
        };
        control
            .mark_exported(&receipt.observation_ref, disposition)
            .await
            .unwrap();
    }
    assert_eq!(control.progress(90).await.unwrap().pending_count, 0);
    let archive = evidence.admissions_after(0, 10).await.unwrap();
    assert_eq!(archive.captures.len(), 1);
    assert_eq!(archive.captures[0].capture.observation_ref.producer_seq, 2);
}

#[tokio::test]
async fn large_accepted_input_keeps_fixed_manifest_across_restart_and_chunk_configuration_change() {
    use crate::{
        trace::cas::BlobStore,
        trace::evidence::{AdmissionLimits, EvidenceStore},
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let evidence = EvidenceStore::open(
        temp.path().join("evidence.db"),
        BlobStore::new(temp.path().join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap();
    let control = store(&path).await;
    let mut command = acceptance("command-large");
    command.input.bytes = vec![b'x'; control.limits().payload_chunk_bytes + 10_000];
    command.input.bytes[..5].copy_from_slice(b"\xff\0A\r\n");
    command.ingestion_authorization_epoch = evidence
        .authorize_scope(
            &command.target.ingestion_scope(),
            &command.target.declared_subjects(),
        )
        .await
        .unwrap();
    control.accept(command.clone()).await.unwrap();
    let original = control.pending().await.unwrap();
    let manifest = original[0]
        .manifest
        .as_ref()
        .expect("large control facts use a persisted manifest");
    assert!(manifest.chunks.len() > 1);
    assert!(manifest
        .chunks
        .iter()
        .all(|chunk| chunk.length <= control.limits().payload_chunk_bytes as u64));
    control.close().await;
    let control = ControlStore::open(
        &path,
        Issuer::Server("server-a".into()),
        OutboxLimits {
            payload_chunk_bytes: 8192,
            ..OutboxLimits::default()
        },
    )
    .await
    .unwrap();
    let pending = control.pending().await.unwrap();
    assert_eq!(pending, original);
    for chunk in &manifest.chunks {
        let start = chunk.payload_offset as usize;
        let end = start + chunk.length as usize;
        assert_eq!(
            evidence
                .stage_payload(&pending[0].payload[start..end])
                .await
                .unwrap(),
            chunk.digest
        );
    }
    assert_eq!(
        evidence.stage_manifest(manifest).await.unwrap(),
        pending[0].capture.payload_ref
    );
    evidence
        .admit_batch(
            &Issuer::Server("server-a".into()),
            std::slice::from_ref(&pending[0].capture),
        )
        .await
        .unwrap();
    let bytes = evidence
        .payload_bytes(&pending[0].capture.payload_ref, pending[0].payload.len())
        .await
        .unwrap();
    assert_eq!(bytes, original[0].payload);
    let fact: OperationalFact = serde_json::from_slice(&bytes).unwrap();
    assert!(
        matches!(fact, OperationalFact::Accepted { command: actual } if actual.input.bytes == command.input.bytes)
    );
}

#[tokio::test]
async fn excessive_chunk_expansion_is_rejected_before_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let control = ControlStore::open(
        &temp.path().join("control.db"),
        Issuer::Server("server-a".into()),
        OutboxLimits {
            payload_chunk_bytes: 128,
            max_payload_chunks: 2,
            ..OutboxLimits::default()
        },
    )
    .await
    .unwrap();
    let mut command = acceptance("command-large");
    command.input.bytes =
        vec![b'x'; control.limits().payload_chunk_bytes * control.limits().max_payload_chunks + 1];
    assert!(matches!(
        control.accept(command).await,
        Err(OutboxError::TooLarge)
    ));
    assert!(control.command("command-large").await.unwrap().is_none());
    assert_eq!(control.progress(40).await.unwrap().pending_count, 0);
}

#[tokio::test]
async fn manifest_metadata_limit_is_enforced_before_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let control = ControlStore::open(
        &temp.path().join("control.db"),
        Issuer::Server("server-a".into()),
        OutboxLimits {
            payload_chunk_bytes: 128,
            max_manifest_bytes: 128,
            ..OutboxLimits::default()
        },
    )
    .await
    .unwrap();
    let command = acceptance("command-a");
    assert!(command.input.bytes.len() < control.limits().max_input_bytes);
    assert!(matches!(
        control.accept(command).await,
        Err(OutboxError::TooLarge)
    ));
    assert!(control.command("command-a").await.unwrap().is_none());
    assert_eq!(control.progress(40).await.unwrap().pending_count, 0);
}

#[tokio::test]
async fn capture_envelope_limit_is_enforced_before_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let control = ControlStore::open(
        &temp.path().join("control.db"),
        Issuer::Server("server-a".into()),
        OutboxLimits {
            max_capture_envelope_bytes: 128,
            ..OutboxLimits::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        control.accept(acceptance("command-a")).await,
        Err(OutboxError::TooLarge)
    ));
    assert!(control.command("command-a").await.unwrap().is_none());
    assert_eq!(control.progress(40).await.unwrap().pending_count, 0);
}

#[tokio::test]
async fn lowered_export_budget_reports_retained_work_instead_of_an_empty_batch() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    let mut command = acceptance("command-a");
    command.input.bytes = vec![b'x'; 4096];
    control.accept(command).await.unwrap();
    let original = control.pending().await.unwrap();
    control.close().await;
    let control = ControlStore::open(
        &path,
        Issuer::Server("server-a".into()),
        OutboxLimits {
            max_fact_bytes: 2048,
            max_export_bytes: 2048,
            ..OutboxLimits::default()
        },
    )
    .await
    .unwrap();
    assert!(original[0].payload.len() > control.limits().max_export_bytes);
    assert!(matches!(
        control.pending().await,
        Err(OutboxError::TooLarge)
    ));
    assert_eq!(control.progress(40).await.unwrap().pending_count, 1);
    assert_eq!(
        control.command("command-a").await.unwrap().unwrap().state,
        CommandState::Accepted
    );
}

fn launch_acceptance(id: &str) -> CommandAcceptance {
    let mut command = acceptance(id);
    command.target = CommandTarget::Launch {
        inventory: sylph_protocol::trace::InventoryRef {
            environment: sylph_protocol::EnvironmentId::parse("env").unwrap(),
            vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
            instance: "launch-inventory".into(),
        },
    };
    command
}
fn identity_evidence() -> Vec<ObservationRef> {
    vec![ObservationRef {
        issuer: Issuer::Environment(sylph_protocol::EnvironmentId::parse("env").unwrap()),
        producer_id: "vendor-capture".into(),
        producer_epoch: "process-one".into(),
        producer_seq: 1,
    }]
}

#[tokio::test]
async fn native_binding_preserves_launch_target_input_epoch_and_dispatch_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("control.db");
    let control = store(&path).await;
    let command = launch_acceptance("launch-one");
    let accepted = control.accept(command.clone()).await.unwrap();
    assert_eq!(accepted.agent(), None);
    assert!(control.pending().await.unwrap()[0]
        .capture
        .declared_subject_refs
        .is_empty());
    control
        .claim_dispatch("launch-one", authorization(), 30)
        .await
        .unwrap();
    let agent = AgentRef::Session(SessionId::decode("env:codex:vendor-native").unwrap());
    let binding = control
        .bind_identity("launch-one", agent.clone(), identity_evidence(), 40)
        .await
        .unwrap();
    assert_eq!(
        control
            .bind_identity("launch-one", agent.clone(), identity_evidence(), 50)
            .await
            .unwrap(),
        binding
    );
    let current = control.command("launch-one").await.unwrap().unwrap();
    assert_eq!(current.acceptance, command);
    assert_eq!(current.agent(), Some(&agent));
    assert_eq!(current.dispatch_attempts, 1);
    assert_eq!(current.state, CommandState::Dispatching);
    let queued = control.pending().await.unwrap();
    assert_eq!(queued.len(), 3);
    let fact: OperationalFact = serde_json::from_slice(&queued[2].payload).unwrap();
    assert!(
        matches!(fact,OperationalFact::IdentityBound{agent_ref,source_evidence,..} if agent_ref==agent&&source_evidence==identity_evidence())
    );
    assert_eq!(
        queued[2].capture.ingestion_authorization_epoch,
        command.ingestion_authorization_epoch
    );
    assert!(control
        .claim_dispatch("launch-one", authorization(), 60)
        .await
        .is_err());
    control.close().await;
    let reopened = store(&path).await;
    assert_eq!(
        reopened.command("launch-one").await.unwrap().unwrap(),
        current
    );
}

#[tokio::test]
async fn binding_refuses_foreign_vendor_phantom_children_and_unlaunched_commands() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control
        .accept(launch_acceptance("launch-one"))
        .await
        .unwrap();
    let agent = AgentRef::Session(SessionId::decode("env:codex:vendor-native").unwrap());
    assert!(control
        .bind_identity("launch-one", agent.clone(), identity_evidence(), 30)
        .await
        .is_err());
    control
        .claim_dispatch("launch-one", authorization(), 30)
        .await
        .unwrap();
    let pi = AgentRef::Session(SessionId::decode("env:pi:vendor-native").unwrap());
    assert!(control
        .bind_identity("launch-one", pi, identity_evidence(), 40)
        .await
        .is_err());
    let child = AgentRef::Embedded {
        anchor_session: agent.anchor_session().clone(),
        namespace: "codex.child".into(),
        native_child_id: "child".into(),
    };
    assert!(control
        .bind_identity("launch-one", child, identity_evidence(), 40)
        .await
        .is_err());
    assert!(control
        .bind_identity("launch-one", agent.clone(), vec![], 40)
        .await
        .is_err());
    control
        .bind_identity("launch-one", agent.clone(), identity_evidence(), 40)
        .await
        .unwrap();
    let other = AgentRef::Session(SessionId::decode("env:codex:another-native").unwrap());
    assert!(matches!(
        control
            .bind_identity("launch-one", other, identity_evidence(), 50)
            .await,
        Err(OutboxError::IdentityConflict)
    ));
    assert_eq!(
        control
            .command("launch-one")
            .await
            .unwrap()
            .unwrap()
            .agent(),
        Some(&agent)
    );
}

#[tokio::test]
async fn failed_binding_transaction_retains_an_unbound_command_and_no_identity_fact() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    control
        .accept(launch_acceptance("launch-one"))
        .await
        .unwrap();
    control
        .claim_dispatch("launch-one", authorization(), 30)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TRIGGER refuse_binding BEFORE UPDATE OF bound_agent_json ON trace_commands BEGIN SELECT RAISE(ABORT,'binding failure'); END;").execute(&control.pool).await.unwrap();
    let agent = AgentRef::Session(SessionId::decode("env:codex:vendor-native").unwrap());
    assert!(control
        .bind_identity("launch-one", agent.clone(), identity_evidence(), 40)
        .await
        .is_err());
    assert_eq!(
        control
            .command("launch-one")
            .await
            .unwrap()
            .unwrap()
            .agent(),
        None
    );
    assert_eq!(control.pending().await.unwrap().len(), 2);
    sqlx::query("DROP TRIGGER refuse_binding")
        .execute(&control.pool)
        .await
        .unwrap();
    assert_eq!(
        control
            .bind_identity("launch-one", agent, identity_evidence(), 50)
            .await
            .unwrap()
            .producer_seq,
        3
    );
}

#[tokio::test]
async fn resolved_launch_identity_never_reauthorizes_a_fresh_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let control = store(&temp.path().join("control.db")).await;
    let mut command = launch_acceptance("launch-one");
    command.retry_policy = RetryPolicy::VendorIdempotent {
        key: "native-retry".into(),
    };
    control.accept(command).await.unwrap();
    control
        .claim_dispatch("launch-one", authorization(), 30)
        .await
        .unwrap();
    let agent = AgentRef::Session(SessionId::decode("env:codex:vendor-native").unwrap());
    control
        .bind_identity("launch-one", agent, identity_evidence(), 40)
        .await
        .unwrap();
    control
        .record_delivery(
            "launch-one",
            "response-lost",
            DeliveryFact::OutcomeUnknown {
                reason: "transport lost".into(),
            },
            50,
        )
        .await
        .unwrap();
    assert!(control
        .claim_dispatch("launch-one", authorization(), 60)
        .await
        .is_err());
    assert_eq!(
        control
            .command("launch-one")
            .await
            .unwrap()
            .unwrap()
            .dispatch_attempts,
        1
    );
}

#[tokio::test]
async fn every_queued_edit_names_the_preceding_immutable_revision() {
    let dir = tempfile::tempdir().unwrap();
    let store = ControlStore::open(
        &dir.path().join("control.db"),
        Issuer::Server("test".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap();
    let mut accepted = acceptance("revision-chain");
    accepted.kind = CommandKind::Queue;
    let original = accepted.input.revision.clone();
    store.accept(accepted).await.unwrap();
    for (expected, name) in ["revision-two", "revision-three"].into_iter().enumerate() {
        store
            .revise_queued(
                "revision-chain",
                expected as u64,
                SubmittedRevision {
                    revision: name.into(),
                    media_type: "text/plain".into(),
                    bytes: name.as_bytes().to_vec(),
                },
                authorization(),
            )
            .await
            .unwrap();
    }
    let chain: Vec<_> = store
        .pending()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|entry| {
            match serde_json::from_slice::<OperationalFact>(&entry.payload).unwrap() {
                OperationalFact::Revised {
                    previous_revision,
                    input,
                    ..
                } => Some((previous_revision, input.revision)),
                _ => None,
            }
        })
        .collect();
    assert_eq!(
        chain,
        vec![
            (original, "revision-two".into()),
            ("revision-two".into(), "revision-three".into())
        ]
    );
}

#[tokio::test]
async fn bootstrap_cookie_rotation_invalidates_the_previous_process_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir.path().join("control.db")).await;
    store
        .upsert_subject("bootstrap", "Bootstrap", "previous-process-token", true)
        .await
        .unwrap();
    store
        .upsert_subject("bootstrap", "Bootstrap", "current-process-token", true)
        .await
        .unwrap();
    assert_eq!(
        store
            .subject_id_for_token_hash("previous-process-token")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .subject_id_for_token_hash("current-process-token")
            .await
            .unwrap()
            .as_deref(),
        Some("bootstrap")
    );
}

fn pending_approval() -> sylph_protocol::Request {
    serde_json::from_value(serde_json::json!({"id":"scoped-request", "kind":"approval", "options":[{"id":"allow","label":"Allow"}]})).unwrap()
}

#[tokio::test]
async fn concurrent_approval_dispatch_claims_reserve_one_request_and_survive_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir.path().join("control.db")).await;
    let request = pending_approval();
    let mut first = acceptance("resolver-a");
    first.kind = CommandKind::Approval;
    let mut second = acceptance("resolver-b");
    second.kind = CommandKind::Approval;
    let session = first.target.agent().unwrap().anchor_session().clone();
    store
        .set_current_request(&session, &request, 1)
        .await
        .unwrap();
    store.accept(first).await.unwrap();
    store.accept(second).await.unwrap();
    let (left, right) = tokio::join!(
        store.claim_request_dispatch("resolver-a", authorization(), 30, &session, &request),
        store.claim_request_dispatch("resolver-b", authorization(), 30, &session, &request),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let winner = if left.is_ok() {
        "resolver-a"
    } else {
        "resolver-b"
    };
    let loser = if left.is_ok() {
        "resolver-b"
    } else {
        "resolver-a"
    };
    assert_eq!(
        store
            .command(loser)
            .await
            .unwrap()
            .unwrap()
            .dispatch_attempts,
        0
    );
    store
        .record_delivery(
            winner,
            "lost",
            DeliveryFact::OutcomeUnknown {
                reason: "response lost".into(),
            },
            31,
        )
        .await
        .unwrap();
    store
        .clear_environment_runtime(&session.environment)
        .await
        .unwrap();
    store.reset_runtime_connections().await.unwrap();
    store
        .set_current_request(&session, &request, 2)
        .await
        .unwrap();
    assert!(store
        .current_request(&session, &request.id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .claim_request_dispatch(loser, authorization(), 32, &session, &request)
        .await
        .is_err());
    store
        .record_delivery(
            winner,
            "accepted",
            DeliveryFact::ReceiverAccepted {
                receipt: b"accepted".to_vec(),
            },
            33,
        )
        .await
        .unwrap();
    store
        .set_current_request(&session, &request, 3)
        .await
        .unwrap();
    assert!(store
        .current_request(&session, &request.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn changed_approval_notice_blocks_a_stale_answer_and_explicit_refusal_restores_choice() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir.path().join("control.db")).await;
    let request = pending_approval();
    let mut accepted = acceptance("approval-change");
    accepted.kind = CommandKind::Approval;
    let session = accepted.target.agent().unwrap().anchor_session().clone();
    store
        .set_current_request(&session, &request, 1)
        .await
        .unwrap();
    store.accept(accepted).await.unwrap();
    let mut changed = request.clone();
    changed.options[0].id = "allow-revised".into();
    store
        .set_current_request(&session, &changed, 1)
        .await
        .unwrap();
    assert!(store
        .claim_request_dispatch("approval-change", authorization(), 30, &session, &request)
        .await
        .is_err());
    assert_eq!(
        store
            .command("approval-change")
            .await
            .unwrap()
            .unwrap()
            .dispatch_attempts,
        0
    );
    store
        .claim_request_dispatch("approval-change", authorization(), 31, &session, &changed)
        .await
        .unwrap();
    store
        .record_delivery(
            "approval-change",
            "refusal",
            DeliveryFact::Rejected {
                receipt: b"vendor explicitly declined answer".to_vec(),
            },
            32,
        )
        .await
        .unwrap();
    assert_eq!(
        store.current_request(&session, &request.id).await.unwrap(),
        Some(changed)
    );
}

#[tokio::test]
async fn queue_candidates_rotate_past_busy_targets_without_overtaking_their_first_input() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir.path().join("control.db")).await;
    for index in 0..40 {
        let mut command = acceptance(&format!("target-{index}"));
        command.kind = CommandKind::Queue;
        command.target = CommandTarget::Agent {
            agent: AgentRef::Session(
                SessionId::decode(&format!("env:codex:target-{index}")).unwrap(),
            ),
        };
        store.accept(command).await.unwrap();
    }
    let mut second = acceptance("target-zero-second");
    second.kind = CommandKind::Queue;
    second.target = CommandTarget::Agent {
        agent: AgentRef::Session(SessionId::decode("env:codex:target-0").unwrap()),
    };
    store.accept(second).await.unwrap();
    let first = store.queued_commands(0, 32).await.unwrap();
    assert_eq!(first.len(), 32);
    let after = first.last().unwrap().accepted_observation.producer_seq;
    let later = store.queued_commands(after, 32).await.unwrap();
    assert_eq!(later.len(), 8);
    assert_eq!(later[0].acceptance.command_id, "target-32");
    assert!(later
        .iter()
        .all(|command| command.acceptance.command_id != "target-zero-second"));
    store
        .cancel_queued("target-0", 0, "subject-a")
        .await
        .unwrap();
    let later = store.queued_commands(after, 32).await.unwrap();
    assert_eq!(
        later.last().unwrap().acceptance.command_id,
        "target-zero-second"
    );
}
