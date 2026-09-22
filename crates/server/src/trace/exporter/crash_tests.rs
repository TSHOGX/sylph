use super::*;
use crate::{
    db::control::{CommandState, OutboxLimits},
    trace::{
        cas::BlobStore,
        evidence::{
            catalogue::{CatalogueLimits, EvidenceCatalogue},
            AdmissionLimits, EvidenceStore,
        },
        upload::UploadLimits,
    },
};
use std::{os::unix::process::ExitStatusExt, path::Path, time::Duration};
use sylph_protocol::trace::{
    AgentRef, AuthorizationReceipt, CommandAcceptance, CommandKind, DeliveryFact, Issuer,
    OperationalFact, RetryPolicy, SubmittedRevision,
};

const INPUT: &[u8] = b"exact submitted input\x00\xff\r\n";
const RECEIPT: &[u8] = b"{\"queue\":\"accepted\",\"queue\":\"accepted\"}\r\n";

async fn open(path: &Path) -> (ControlStore, EvidenceStore, OutboxExporter) {
    let control = ControlStore::open(
        &path.join("control.db"),
        Issuer::Server("outbox-crash-test".into()),
        OutboxLimits::default(),
    )
    .await
    .unwrap();
    let evidence = EvidenceStore::open(
        path.join("evidence.db"),
        BlobStore::new(path.join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap();
    let exporter = OutboxExporter::new(
        control.clone(),
        CaptureUploads::new(
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap(),
            UploadLimits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    (control, evidence, exporter)
}

fn authorization() -> AuthorizationReceipt {
    AuthorizationReceipt {
        subject: "user".into(),
        decision_ref: "grant".into(),
        lease_ref: None,
        evaluated_at_ms: 10,
    }
}

fn result() -> DeliveryFact {
    DeliveryFact::ReceiverAccepted {
        receipt: RECEIPT.to_vec(),
    }
}

#[test]
fn outbox_kill_probe() {
    let Some(path) = std::env::var_os("SYLPH_OUTBOX_CRASH_FIXTURE") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (control, _evidence, exporter) = open(Path::new(&path)).await;
        control
            .record_delivery("command", "dispatch-result", result(), 30)
            .await
            .unwrap();
        exporter.run_once().await.unwrap();
        panic!("the selected crash boundary was not reached");
    });
}

#[tokio::test]
async fn killed_result_and_export_transactions_recover_exact_facts_without_redispatch() {
    for point in [
        "before_result_commit",
        "after_result_commit",
        "before_export_admission",
        "after_export_admission",
        "before_export_mark_commit",
        "after_export_mark_commit",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (control, evidence, _) = open(dir.path()).await;
        let agent = AgentRef::Session("env:codex:outbox-crash".parse().unwrap());
        let epoch = evidence
            .authorize_scope(
                &IngestionScope::Agent(agent.clone()),
                std::slice::from_ref(&agent),
            )
            .await
            .unwrap();
        control
            .accept(CommandAcceptance {
                command_id: "command".into(),
                target: CommandTarget::Agent { agent },
                kind: CommandKind::Send,
                input: SubmittedRevision {
                    revision: "input".into(),
                    media_type: "application/octet-stream".into(),
                    bytes: INPUT.to_vec(),
                },
                authorization: authorization(),
                ingestion_authorization_epoch: epoch,
                retry_policy: RetryPolicy::AtMostOnce,
                idempotency_key: None,
                accepted_at_ms: 10,
            })
            .await
            .unwrap();
        control
            .claim_dispatch("command", authorization(), 20)
            .await
            .unwrap();
        let original = control.pending().await.unwrap();
        control.close().await;
        evidence.close().await;

        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "trace::exporter::crash_tests::outbox_kill_probe",
                "--exact",
                "--nocapture",
            ])
            .env("SYLPH_OUTBOX_CRASH_FIXTURE", dir.path())
            .env("SYLPH_OUTBOX_CRASH_POINT", point)
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(30), child.output())
            .await
            .expect("crash probe timed out")
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "{point}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let (control, evidence, exporter) = open(dir.path()).await;
        let result_committed = point != "before_result_commit";
        let admitted = matches!(
            point,
            "after_export_admission" | "before_export_mark_commit" | "after_export_mark_commit"
        );
        let marked = point == "after_export_mark_commit";
        let command = control.command("command").await.unwrap().unwrap();
        assert_eq!(command.acceptance.input.bytes, INPUT, "{point}");
        assert_eq!(command.dispatch_attempts, 1, "{point}");
        assert_eq!(
            control.dispatch_result("command").await.unwrap(),
            result_committed.then(result),
            "{point}"
        );
        assert_eq!(
            command.state,
            if result_committed {
                CommandState::ReceiverAccepted
            } else {
                CommandState::Dispatching
            },
            "{point}"
        );
        assert_eq!(
            evidence.evidence_cut().await.unwrap(),
            u64::from(admitted),
            "{point}"
        );
        let pending = control.pending().await.unwrap();
        assert_eq!(
            pending.len(),
            2 + usize::from(result_committed) - usize::from(marked),
            "{point}"
        );
        for occurrence in &original {
            if !marked || occurrence.capture.observation_ref.producer_seq != 1 {
                assert!(pending.contains(occurrence), "{point}");
            }
        }
        assert_eq!(
            control.progress(40).await.unwrap().pending_count,
            pending.len() as u64
        );
        assert_eq!(
            control.recover_inflight(40).await.unwrap(),
            usize::from(!result_committed),
            "{point}"
        );
        let terminal = control.command("command").await.unwrap().unwrap();
        assert_eq!(
            terminal.state,
            if result_committed {
                CommandState::ReceiverAccepted
            } else {
                CommandState::Unknown
            }
        );
        assert!(
            matches!(
                control.claim_dispatch("command", authorization(), 50).await,
                Err(OutboxError::InvalidTransition)
            ),
            "{point}: recovery must never grant a second vendor attempt"
        );

        // The first export may have committed only in evidence. Replaying all
        // pending facts must preserve its occurrence identity and exact bytes.
        let before_retry = control.pending().await.unwrap();
        exporter.run_once().await.unwrap();
        assert_eq!(exporter.run_once().await.unwrap().transferred, 0);
        assert_eq!(evidence.evidence_cut().await.unwrap(), 3, "{point}");
        for occurrence in original.iter().chain(before_retry.iter()) {
            assert_eq!(
                evidence
                    .capture_payload(
                        &occurrence.capture.observation_ref,
                        occurrence.payload.len()
                    )
                    .await
                    .unwrap(),
                Some(occurrence.payload.clone()),
                "{point}"
            );
        }
        let page = evidence.admissions_after(0, 10).await.unwrap();
        assert_eq!(page.captures.len(), 3, "{point}");
        let last = &page.captures[2].capture;
        let bytes = evidence
            .capture_payload(&last.observation_ref, 4096)
            .await
            .unwrap()
            .unwrap();
        let fact: OperationalFact = serde_json::from_slice(&bytes).unwrap();
        assert!(
            matches!(fact, OperationalFact::Delivery { fact, .. } if if result_committed { fact == result() } else { matches!(fact, DeliveryFact::OutcomeUnknown { .. }) }),
            "{point}"
        );
        let progress = control.progress(60).await.unwrap();
        assert_eq!(progress.pending_count, 0);
        assert_eq!(progress.pending_bytes, 0);
        assert_eq!(progress.oldest_pending_age_ms, None);
        assert_eq!(
            control
                .command("command")
                .await
                .unwrap()
                .unwrap()
                .dispatch_attempts,
            1
        );
        control.close().await;
        evidence.close().await;
    }
}
