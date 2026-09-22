use super::*;

async fn acknowledge(registry: &CaptureRegistry) -> Vec<QueuedCapture> {
    let mut all = Vec::new();
    loop {
        let pending = registry.pending(registry.limits.max_records).await.unwrap();
        if pending.is_empty() {
            return all;
        }
        for record in &pending {
            registry
                .acknowledge(&[CaptureReceipt::Retained {
                    observation: record.capture.observation_ref.clone(),
                    admission_seq: record.capture.observation_ref.producer_seq,
                }])
                .await
                .unwrap();
        }
        all.extend(pending);
    }
}

#[tokio::test]
async fn superseded_staging_preserves_each_record_epoch_clock_and_terminal_fragment_after_restart()
{
    let mut fixture = Fixture::new(
        CaptureLimits {
            max_records: 1,
            chunk_bytes: 1,
            read_bytes: 64,
            max_record_bytes: 128,
            ..Default::default()
        },
        1024 * 1024,
    )
    .await;
    fixture.source.identity_record_ordinal = Some(1);
    fixture.write(b"first\n{\"id\":\"native\"}\r\nlast\n{broken");
    fixture.capture().await;
    let mut all = acknowledge(&fixture.registry).await;
    fixture.write(b"replacement\n");
    let mut fresh = authorization("epoch-2");
    fresh.captured_at.value = 9999;
    fixture
        .registry
        .capture_jsonl(&fixture.source, fresh)
        .await
        .unwrap();
    all.extend(acknowledge(&fixture.registry).await);
    let registry = fixture.reopen().await;
    let frontier = registry.closed_tail_frontier().await.unwrap();
    let mut passes = 0;
    while registry.closed_tails_pending(frontier).await.unwrap() {
        let count = registry.drain_closed_tails(frontier).await.unwrap();
        assert!(count <= registry.limits.max_records);
        all.extend(acknowledge(&registry).await);
        passes += 1;
        assert!(passes <= 8, "drain jobs must advance");
    }
    let mut old: Vec<_> = all
        .into_iter()
        .filter(|item| item.capture.source_position.generation == 0)
        .collect();
    old.sort_by_key(|item| match item.capture.source_position.locator {
        SourceLocator::Jsonl { record_ordinal, .. } => record_ordinal,
        _ => panic!(),
    });
    assert_eq!(
        old.iter().map(bytes).collect::<Vec<_>>(),
        [
            b"first\n".to_vec(),
            b"{\"id\":\"native\"}\r\n".to_vec(),
            b"last\n".to_vec(),
            b"{broken".to_vec()
        ]
    );
    let mut offset = 0;
    for (ordinal, item) in old.iter().enumerate() {
        let length = bytes(item).len() as u64;
        assert_eq!(
            item.capture.source_position.locator,
            SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + length,
                record_ordinal: ordinal as u64
            }
        );
        assert_eq!(
            item.capture.ingestion_authorization_epoch,
            authorization("epoch-1").epoch
        );
        assert_eq!(item.capture.captured_at.value, 10);
        assert_eq!(
            item.capture
                .capture_diagnostics
                .iter()
                .any(|value| value.code
                    == sylph_protocol::trace::CaptureDiagnosticCode::IncompleteRecord),
            ordinal == 3
        );
        offset += length;
    }
    assert_eq!(
        registry
            .source_proof(
                &fixture.source.reference,
                0,
                &authorization("epoch-1").epoch
            )
            .await
            .unwrap(),
        Some(old[1].capture.observation_ref.clone())
    );
    assert!(registry
        .source_proof(
            &fixture.source.reference,
            0,
            &authorization("epoch-2").epoch
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(registry.spool.size_bytes().await.unwrap(), 0);
    assert!(registry.spool.is_empty().await.unwrap());
}

#[tokio::test]
async fn reauthorization_supplies_its_own_identity_proof_without_reusing_an_old_receipt() {
    let mut fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.source.identity_record_ordinal = Some(0);
    fixture.write(b"{\"id\":\"native\"}\n");
    let first = fixture.capture().await.observations.remove(0);
    acknowledge(&fixture.registry).await;
    let second = fixture
        .registry
        .capture_jsonl(&fixture.source, authorization("epoch-2"))
        .await
        .unwrap()
        .observations
        .remove(0);
    assert_ne!(first, second);
    assert_eq!(
        fixture
            .registry
            .source_proof(
                &fixture.source.reference,
                0,
                &authorization("epoch-1").epoch
            )
            .await
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        fixture
            .registry
            .source_proof(
                &fixture.source.reference,
                0,
                &authorization("epoch-2").epoch
            )
            .await
            .unwrap(),
        Some(second)
    );
}

#[tokio::test]
async fn staging_credit_can_drain_at_a_full_quota_and_is_released_only_by_receipts() {
    let fixture = Fixture::new(CaptureLimits::default(), 32 * 1024).await;
    let raw = vec![b'x'; 16 * 1024];
    fixture.write(&raw);
    fixture.capture().await;
    std::fs::remove_file(&fixture.source.locator).unwrap();
    fixture.capture().await;
    let held = fixture.registry.spool.size_bytes().await.unwrap();
    // Reopen with exactly the already committed usage. There is no spare byte
    // with which to create the drain envelope, so its reserved credit is essential.
    let spool = Spool::open(SpoolConfig::new(fixture.dir.path().join("spool.db"), held))
        .await
        .unwrap();
    let registry = CaptureRegistry::open(
        spool,
        fixture.registry.issuer.clone(),
        fixture.registry.policy.clone(),
        fixture.registry.limits,
    )
    .await
    .unwrap();
    let frontier = registry.closed_tail_frontier().await.unwrap();
    assert_eq!(registry.drain_closed_tails(frontier).await.unwrap(), 1);
    assert!(registry.spool.size_bytes().await.unwrap() <= registry.spool.max_bytes);
    let queued = registry.pending(4).await.unwrap();
    assert_eq!(bytes(&queued[0]), raw);
    assert!(registry.spool.size_bytes().await.unwrap() > 0);
    acknowledge(&registry).await;
    assert_eq!(registry.spool.size_bytes().await.unwrap(), 0);
}

#[tokio::test]
async fn closed_tail_pass_obeys_its_byte_budget_even_when_many_records_fit_its_item_limit() {
    let limits = CaptureLimits {
        max_records: 1,
        chunk_bytes: 128,
        read_bytes: 4096,
        max_record_bytes: 8192,
        ..Default::default()
    };
    let fixture = Fixture::new(limits, 1024 * 1024).await;
    fixture.write(&b"row\n".repeat(1000));
    fixture.capture().await;
    acknowledge(&fixture.registry).await;
    fixture.write(b"");
    fixture.capture().await;
    let spool = Spool::open(SpoolConfig::new(
        fixture.dir.path().join("spool.db"),
        fixture.registry.spool.max_bytes,
    ))
    .await
    .unwrap();
    let registry = CaptureRegistry::open(
        spool,
        fixture.registry.issuer.clone(),
        fixture.registry.policy.clone(),
        CaptureLimits {
            max_records: 128,
            read_bytes: 15,
            ..limits
        },
    )
    .await
    .unwrap();
    let frontier = registry.closed_tail_frontier().await.unwrap();
    let queued = registry.drain_closed_tails(frontier).await.unwrap();
    assert!(
        queued > 1,
        "the byte budget must be tested independently of a one-item limit"
    );
    let records = acknowledge(&registry).await;
    assert_eq!(records.len(), queued);
    let bytes: usize = records
        .iter()
        .map(|record| super::bytes(record).len())
        .sum();
    assert!(
        bytes <= registry.limits.read_bytes,
        "one bounded pass transferred {bytes} bytes against {}",
        registry.limits.read_bytes
    );
    assert!(registry.closed_tails_pending(frontier).await.unwrap());
}

#[tokio::test]
async fn independent_drainers_commit_one_copy_and_later_closures_do_not_extend_the_frontier() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"first fragment");
    fixture.capture().await;
    fixture.write(b"second fragment");
    fixture.capture().await;
    let frontier = fixture.registry.closed_tail_frontier().await.unwrap();
    fixture.write(b"current\n");
    fixture.capture().await;
    acknowledge(&fixture.registry).await;
    let other = fixture.reopen().await;
    let (a, b) = tokio::join!(
        fixture.registry.drain_closed_tails(frontier),
        other.drain_closed_tails(frontier)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    let queued = acknowledge(&fixture.registry).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(bytes(&queued[0]), b"first fragment");
    assert_eq!(
        fixture.registry.drain_closed_tails(frontier).await.unwrap(),
        0
    );
    assert!(!fixture
        .registry
        .closed_tails_pending(frontier)
        .await
        .unwrap());
    let later = fixture.registry.closed_tail_frontier().await.unwrap();
    assert!(later > frontier);
    assert_eq!(fixture.registry.drain_closed_tails(later).await.unwrap(), 1);
    assert_eq!(
        bytes(&acknowledge(&fixture.registry).await[0]),
        b"second fragment"
    );
    assert_eq!(fixture.registry.spool.size_bytes().await.unwrap(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn killed_tail_transfer_keeps_staging_or_one_queued_copy_with_identical_accounting() {
    use std::os::unix::process::ExitStatusExt;
    for stage in ["before_tail_commit", "after_tail_commit"] {
        let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
        fixture.write(b"first\n{broken");
        fixture.capture().await;
        acknowledge(&fixture.registry).await;
        fixture.write(b"replacement\n");
        fixture.capture().await;
        acknowledge(&fixture.registry).await;
        let held = fixture.registry.spool.size_bytes().await.unwrap();
        fixture.registry.spool.pool.close().await;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "spool::capture::tests::capture_crash_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("SYLPH_CAPTURE_CRASH_PATH", fixture.dir.path())
            .env("SYLPH_CAPTURE_CRASH_STAGE", stage)
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), child.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "{stage}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let recovered = fixture.reopen().await;
        assert_eq!(recovered.spool.size_bytes().await.unwrap(), held, "{stage}");
        let committed = stage == "after_tail_commit";
        assert_eq!(
            recovered.pending(10).await.unwrap().len(),
            usize::from(committed)
        );
        let frontier = recovered.closed_tail_frontier().await.unwrap();
        assert_eq!(
            recovered.drain_closed_tails(frontier).await.unwrap(),
            usize::from(!committed)
        );
        let queued = acknowledge(&recovered).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(bytes(&queued[0]), b"{broken");
        assert_eq!(queued[0].capture.source_position.generation, 0);
        assert_eq!(
            queued[0].capture.source_position.locator,
            SourceLocator::Jsonl {
                byte_start: 6,
                byte_end: 13,
                record_ordinal: 1
            }
        );
        assert_eq!(
            queued[0].capture.ingestion_authorization_epoch,
            authorization("epoch-1").epoch
        );
        assert_eq!(recovered.spool.size_bytes().await.unwrap(), 0);
        assert!(recovered.spool.is_empty().await.unwrap());
    }
}
