use super::*;
use crate::SpoolConfig;
use sylph_protocol::trace::{TimeBasis, TimeUnit};
use sylph_protocol::{EnvironmentId, SessionId};

struct Fixture {
    dir: tempfile::TempDir,
    registry: CaptureRegistry,
    source: RegisteredSource,
}

impl Fixture {
    async fn new(limits: CaptureLimits, capacity: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.jsonl");
        std::fs::write(&path, []).unwrap();
        let spool = Spool::open(SpoolConfig::new(dir.path().join("spool.db"), capacity))
            .await
            .unwrap();
        let registry = CaptureRegistry::open(
            spool,
            Issuer::Environment(EnvironmentId::parse("env7").unwrap()),
            PathPolicy::from_roots([dir.path().to_path_buf()]).unwrap(),
            limits,
        )
        .await
        .unwrap();
        let source = registry.register_jsonl("pi.native", &path).await.unwrap();
        Self {
            dir,
            registry,
            source,
        }
    }
    async fn capture(&self) -> CapturePass {
        self.registry
            .capture_jsonl(&self.source, authorization("epoch-1"))
            .await
            .unwrap()
    }
    fn write(&self, bytes: &[u8]) {
        std::fs::write(&self.source.locator, bytes).unwrap();
    }
    async fn reopen(&self) -> CaptureRegistry {
        let spool = Spool::open(SpoolConfig::new(
            self.dir.path().join("spool.db"),
            self.registry.spool.max_bytes,
        ))
        .await
        .unwrap();
        CaptureRegistry::open(
            spool,
            self.registry.issuer.clone(),
            self.registry.policy.clone(),
            self.registry.limits,
        )
        .await
        .unwrap()
    }
}
fn authorization(epoch: &str) -> CaptureAuthorization {
    CaptureAuthorization {
        epoch: IngestionEpoch::new(epoch).unwrap(),
        subjects: vec![AgentRef::Session(
            SessionId::decode("env7:pi:native").unwrap(),
        )],
        captured_at: TimeEvidence {
            domain: "env7.wall".into(),
            value: 10,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        },
        vendor_build: Some("fixture".into()),
    }
}
fn bytes(item: &QueuedCapture) -> Vec<u8> {
    item.chunks
        .iter()
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect()
}

#[tokio::test]
async fn lexical_capture_keeps_crlf_duplicate_keys_malformed_lines_and_repetition() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    let raw = b"{\"a\":1, \"a\":2}\r\nnot JSON\n{\"same\":true}\n{\"same\":true}\n";
    fixture.write(raw);
    let pass = fixture.capture().await;
    assert_eq!(pass.complete_offset, raw.len() as u64);
    assert_eq!(pass.observations.len(), 4);
    let records = fixture.registry.pending(10).await.unwrap();
    assert_eq!(records.iter().flat_map(bytes).collect::<Vec<_>>(), raw);
    assert_ne!(
        records[2].capture.source_position,
        records[3].capture.source_position
    );
    assert_eq!(
        records[2].capture.payload_ref,
        records[3].capture.payload_ref
    );
    assert_eq!(
        records[0].capture.source_position.locator,
        SourceLocator::Jsonl {
            byte_start: 0,
            byte_end: 16,
            record_ordinal: 0
        }
    );
}

#[tokio::test]
async fn unterminated_json_stays_durable_without_advancing_complete_cursor() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"{\"ready\":true}");
    let first = fixture.capture().await;
    assert_eq!(first.complete_offset, 0);
    assert_eq!(first.pending_tail_bytes, 14);
    assert!(!fixture.registry.spool.is_empty().await.unwrap());
    assert!(fixture.registry.pending(5).await.unwrap().is_empty());
    let reopened = fixture.reopen().await;
    let registered = reopened
        .register_jsonl("pi.native", &fixture.source.locator)
        .await
        .unwrap();
    assert_eq!(registered.reference, fixture.source.reference);
    fixture.write(b"{\"ready\":true}\n");
    let completed = reopened
        .capture_jsonl(&registered, authorization("epoch-1"))
        .await
        .unwrap();
    assert_eq!(completed.complete_offset, 15);
    assert_eq!(completed.pending_tail_bytes, 0);
    assert_eq!(
        bytes(&reopened.pending(5).await.unwrap()[0]),
        b"{\"ready\":true}\n"
    );
}

#[tokio::test]
async fn a_record_crossing_read_and_transfer_chunks_retains_every_byte() {
    let limits = CaptureLimits {
        chunk_bytes: 3,
        read_bytes: 4,
        max_record_bytes: 128,
        ..CaptureLimits::default()
    };
    let fixture = Fixture::new(limits, 1024 * 1024).await;
    fixture.write(b"abcdefghijkl\n");
    for expected in [4, 8, 12] {
        let pass = fixture.capture().await;
        assert_eq!(pass.pending_tail_bytes, expected);
        assert_eq!(pass.complete_offset, 0);
    }
    let pass = fixture.capture().await;
    assert_eq!(pass.complete_offset, 13);
    assert_eq!(pass.pending_tail_bytes, 0);
    let item = fixture.registry.pending(5).await.unwrap().remove(0);
    assert_eq!(bytes(&item), b"abcdefghijkl\n");
    assert_eq!(item.chunks.len(), 5);
    let manifest = item.manifest.unwrap();
    assert_eq!(manifest.decoded_length, Some(13));
    assert_eq!(
        item.capture.payload_ref.digest(),
        blake3::hash(&manifest.canonical_bytes().unwrap())
            .to_hex()
            .as_str()
    );
}

#[tokio::test]
async fn reconnect_reread_uses_original_source_position_and_fixed_epoch() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"one\ntwo\n");
    fixture.capture().await;
    let first = fixture.registry.pending(5).await.unwrap();
    let reopened = fixture.reopen().await;
    assert!(reopened
        .capture_jsonl(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap()
        .observations
        .is_empty());
    assert_eq!(reopened.pending(5).await.unwrap(), first);
    let fresh = reopened
        .capture_jsonl(&fixture.source, authorization("epoch-2"))
        .await
        .unwrap();
    assert_eq!(fresh.observations.len(), 2);
    let reread = reopened.pending(10).await.unwrap();
    let new = reread
        .iter()
        .find(|item| item.capture.ingestion_authorization_epoch.as_str() == "epoch-2")
        .unwrap();
    assert_eq!(
        new.capture.source_position,
        first[0].capture.source_position
    );
    assert_ne!(
        new.capture.observation_ref,
        first[0].capture.observation_ref
    );
}

#[tokio::test]
async fn consumed_middle_rewrite_starts_new_generation_even_with_unchanged_edges() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    let mut raw = vec![b'a'; 10000];
    raw[4999] = b'\n';
    raw.push(b'\n');
    fixture.write(&raw);
    fixture.capture().await;
    raw[5500] = b'b';
    fixture.write(&raw);
    let changed = fixture.capture().await;
    assert_eq!(changed.generation, 1);
    assert!(changed.generation_changed);
    assert_eq!(changed.observations.len(), 2);
    let records = fixture.registry.pending(10).await.unwrap();
    assert_eq!(records.len(), 4);
    let latest = records
        .iter()
        .find(|item| {
            item.capture.source_position.generation == 1
                && matches!(
                    item.capture.source_position.locator,
                    SourceLocator::Jsonl {
                        record_ordinal: 1,
                        ..
                    }
                )
        })
        .unwrap();
    assert_eq!(bytes(latest)[500], b'b');
}

#[tokio::test]
async fn replacement_and_observed_absence_never_reuse_prior_generation() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"same\n");
    fixture.capture().await;
    let replacement = fixture.dir.path().join("replacement");
    std::fs::write(&replacement, b"same\n").unwrap();
    std::fs::rename(replacement, &fixture.source.locator).unwrap();
    assert_eq!(fixture.capture().await.generation, 1);
    std::fs::remove_file(&fixture.source.locator).unwrap();
    assert!(fixture.capture().await.source_missing);
    fixture.write(b"same\n");
    assert_eq!(fixture.capture().await.generation, 2);
    assert_eq!(fixture.registry.pending(10).await.unwrap().len(), 3);
}

#[tokio::test]
async fn competing_followers_commit_one_occurrence_and_one_cursor() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"one\ntwo\n");
    let other = fixture.reopen().await;
    let (a, b) = tokio::join!(
        fixture
            .registry
            .capture_jsonl(&fixture.source, authorization("epoch-1")),
        other.capture_jsonl(&fixture.source, authorization("epoch-1"))
    );
    assert_eq!(
        a.unwrap().observations.len() + b.unwrap().observations.len(),
        2
    );
    let records = fixture.registry.pending(10).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().flat_map(bytes).collect::<Vec<_>>(),
        b"one\ntwo\n"
    );

    // Open the conflicting window deliberately. Two independent readers both
    // possess the same committed revision before either publishes its append.
    fixture.write(b"one\ntwo\nthree\n");
    let auth = authorization("epoch-1");
    let stale = fixture
        .registry
        .cursor(&fixture.source, &auth.epoch)
        .await
        .unwrap();
    let first = acquire(&fixture.source.locator, &stale, fixture.registry.limits).unwrap();
    let second = acquire(&fixture.source.locator, &stale, fixture.registry.limits).unwrap();
    fixture
        .registry
        .commit(&fixture.source, &auth, &stale, first)
        .await
        .unwrap();
    assert!(matches!(
        other.commit(&fixture.source, &auth, &stale, second).await,
        Err(CaptureSpoolError::ReservationChanged)
    ));
    assert_eq!(fixture.registry.pending(10).await.unwrap().len(), 3);
}

#[tokio::test]
async fn failed_capture_transaction_preserves_sequence_cursor_and_staged_tail() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"partial");
    fixture.capture().await;
    fixture.write(b"partial\n");
    sqlx::raw_sql("CREATE TRIGGER capture_failure BEFORE INSERT ON capture_payload_chunks BEGIN SELECT RAISE(ABORT,'capture fault'); END;").execute(&fixture.registry.spool.pool).await.unwrap();
    assert!(fixture
        .registry
        .capture_jsonl(&fixture.source, authorization("epoch-1"))
        .await
        .is_err());
    let cursor = fixture
        .registry
        .cursor(&fixture.source, &authorization("epoch-1").epoch)
        .await
        .unwrap();
    assert_eq!(cursor.offset, 0);
    assert_eq!(cursor.tail, b"partial");
    assert!(fixture.registry.pending(5).await.unwrap().is_empty());
    sqlx::query("DROP TRIGGER capture_failure")
        .execute(&fixture.registry.spool.pool)
        .await
        .unwrap();
    let completed = fixture.capture().await;
    assert_eq!(completed.observations[0].producer_seq, 1);
    assert_eq!(completed.complete_offset, 8);
}

#[tokio::test]
async fn capacity_failure_rolls_back_all_capture_progress() {
    let fixture = Fixture::new(CaptureLimits::default(), 1).await;
    fixture.write(b"a\n");
    assert!(matches!(
        fixture
            .registry
            .capture_jsonl(&fixture.source, authorization("epoch-1"))
            .await,
        Err(CaptureSpoolError::Spool(SpoolError::Overflow { .. }))
    ));
    let cursor = fixture
        .registry
        .cursor(&fixture.source, &authorization("epoch-1").epoch)
        .await
        .unwrap();
    assert_eq!(cursor.offset, 0);
    assert_eq!(cursor.global_offset, 0);
    assert_eq!(fixture.registry.spool.size_bytes().await.unwrap(), 0);
}

#[tokio::test]
async fn only_terminal_contiguous_receipts_release_exact_queued_bytes() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"first\nsecond\n");
    fixture.capture().await;
    let queued = fixture.registry.pending(5).await.unwrap();
    assert!(fixture
        .registry
        .acknowledge(&[CaptureReceipt::Retained {
            observation: queued[1].capture.observation_ref.clone(),
            admission_seq: 2
        }])
        .await
        .is_err());
    assert_eq!(fixture.registry.pending(5).await.unwrap().len(), 2);
    assert_eq!(
        fixture
            .registry
            .acknowledge(&[
                CaptureReceipt::DiscardedByDeletion {
                    observation: queued[0].capture.observation_ref.clone(),
                    tombstone: "deleted-scope".into()
                },
                CaptureReceipt::Retained {
                    observation: queued[1].capture.observation_ref.clone(),
                    admission_seq: 2
                }
            ])
            .await
            .unwrap(),
        2
    );
    assert!(fixture.registry.pending(5).await.unwrap().is_empty());
    assert_eq!(fixture.registry.spool.size_bytes().await.unwrap(), 0);
    assert!(fixture.capture().await.observations.is_empty());

    assert!(fixture
        .registry
        .acknowledge(&[CaptureReceipt::Retained {
            observation: queued[0].capture.observation_ref.clone(),
            admission_seq: 1,
        }])
        .await
        .is_err());
}

#[tokio::test]
async fn large_queue_items_have_bounded_metadata_and_independent_chunk_reads() {
    let fixture = Fixture::new(
        CaptureLimits {
            chunk_bytes: 512,
            read_bytes: 1024,
            max_record_bytes: 8192,
            max_pending_read_bytes: 4096,
            max_metadata_bytes: 4096,
            ..CaptureLimits::default()
        },
        64 * 1024,
    )
    .await;
    let mut raw = vec![b'x'; 6143];
    raw.push(b'\n');
    fixture.write(&raw);
    for _ in 0..6 {
        fixture.capture().await;
    }
    assert!(fixture.registry.pending(1).await.is_err());
    let headers = fixture.registry.pending_metadata(1).await.unwrap();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].chunk_count, 12);
    let mut captured = Vec::new();
    for ordinal in 0..12 {
        let (_, bytes) = fixture
            .registry
            .queued_chunk(&headers[0].capture.observation_ref, ordinal)
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.len() <= 512);
        captured.extend(bytes);
    }
    assert_eq!(captured, raw);
}

#[tokio::test]
async fn copied_native_files_have_distinct_source_occurrences() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"same\n");
    fixture.capture().await;
    let copy = fixture.dir.path().join("copy.jsonl");
    std::fs::copy(&fixture.source.locator, &copy).unwrap();
    let registered = fixture
        .registry
        .register_jsonl("pi.native", copy)
        .await
        .unwrap();
    assert_ne!(registered.reference, fixture.source.reference);
    fixture
        .registry
        .capture_jsonl(&registered, authorization("epoch-1"))
        .await
        .unwrap();
    let records = fixture.registry.pending(5).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(bytes(&records[0]), bytes(&records[1]));
    assert_ne!(
        records[0].capture.source_position,
        records[1].capture.source_position
    );
}

#[tokio::test]
#[ignore = "isolated SIGKILL probe launched by the recovery test"]
async fn capture_crash_probe() {
    let directory =
        PathBuf::from(std::env::var_os("SYLPH_CAPTURE_CRASH_PATH").expect("probe directory"));
    let stage = std::env::var("SYLPH_CAPTURE_CRASH_STAGE").unwrap();
    let spool = Spool::open(SpoolConfig::new(directory.join("spool.db"), 1024 * 1024))
        .await
        .unwrap();
    let mut registry = CaptureRegistry::open(
        spool,
        Issuer::Environment(EnvironmentId::parse("env7").unwrap()),
        PathPolicy::from_roots([directory.clone()]).unwrap(),
        CaptureLimits::default(),
    )
    .await
    .unwrap();
    registry.crash_point = Some(match stage.as_str() {
        "before_commit" => "before_commit",
        "after_commit" => "after_commit",
        "before_ack_transaction" => "before_ack_transaction",
        "before_ack_commit" => "before_ack_commit",
        "after_ack_commit" => "after_ack_commit",
        _ => panic!("unknown crash stage"),
    });
    if stage.contains("ack") {
        let receipts: Vec<CaptureReceipt> =
            serde_json::from_slice(&std::fs::read(directory.join("receipts.json")).unwrap())
                .unwrap();
        registry.acknowledge(&receipts).await.unwrap();
        panic!("acknowledgment probe returned without SIGKILL");
    }
    let source = registry
        .register_jsonl("pi.native", directory.join("native.jsonl"))
        .await
        .unwrap();
    registry
        .capture_jsonl(&source, authorization("epoch-1"))
        .await
        .unwrap();
    panic!("crash probe returned without SIGKILL");
}

#[cfg(unix)]
#[tokio::test]
async fn killed_capture_recovers_cursor_and_lexical_queue_at_one_commit_boundary() {
    use std::os::unix::process::ExitStatusExt;
    for stage in ["before_commit", "after_commit"] {
        let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
        fixture.write(b"first\n");
        fixture.capture().await;
        fixture.write(b"first\nsecond\n");
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "spool::capture::tests::capture_crash_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("SYLPH_CAPTURE_CRASH_PATH", fixture.dir.path())
            .env("SYLPH_CAPTURE_CRASH_STAGE", stage)
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(std::time::Duration::from_secs(20), child)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let recovered = fixture.reopen().await;
        let cursor = recovered
            .cursor(&fixture.source, &authorization("epoch-1").epoch)
            .await
            .unwrap();
        let expected = if stage == "before_commit" { 6 } else { 13 };
        assert_eq!(cursor.offset, expected);
        assert_eq!(cursor.global_offset, expected);
        let queued = recovered.pending(10).await.unwrap();
        assert_eq!(queued.len(), if stage == "before_commit" { 1 } else { 2 });
        recovered
            .capture_jsonl(&fixture.source, authorization("epoch-1"))
            .await
            .unwrap();
        let mut queued = recovered.pending(10).await.unwrap();
        queued.sort_by_key(|item| match item.capture.source_position.locator {
            SourceLocator::Jsonl { byte_start, .. } => byte_start,
            _ => panic!("native record position"),
        });
        assert_eq!(
            queued.iter().flat_map(bytes).collect::<Vec<_>>(),
            b"first\nsecond\n"
        );
        assert_eq!(queued.len(), 2);
    }
}

#[tokio::test]
async fn snapshot_reauthorization_keeps_position_and_replacement_starts_a_generation() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.write(b"{\"selected\":1}");
    let first = fixture
        .registry
        .capture_snapshot(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap();
    let replay = fixture
        .registry
        .capture_snapshot(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap();
    assert!(replay.observations.is_empty());
    let reauthorized = fixture
        .registry
        .capture_snapshot(&fixture.source, authorization("epoch-2"))
        .await
        .unwrap();
    assert_eq!(first.generation, reauthorized.generation);
    let queue = fixture.registry.pending(10).await.unwrap();
    assert_eq!(
        queue[0].capture.source_position,
        queue[1].capture.source_position
    );
    assert_ne!(
        queue[0].capture.observation_ref,
        queue[1].capture.observation_ref
    );
    assert_eq!(bytes(&queue[0]), b"{\"selected\":1}");
    let replacement = fixture.dir.path().join("replacement");
    std::fs::write(&replacement, b"{\"selected\":1}").unwrap();
    std::fs::rename(replacement, &fixture.source.locator).unwrap();
    let changed = fixture
        .registry
        .capture_snapshot(&fixture.source, authorization("epoch-2"))
        .await
        .unwrap();
    assert_eq!(changed.generation, first.generation + 1);
    assert!(changed.generation_changed);
    std::fs::remove_file(&fixture.source.locator).unwrap();
    assert!(
        fixture
            .registry
            .capture_snapshot(&fixture.source, authorization("epoch-2"))
            .await
            .unwrap()
            .source_missing
    );
    fixture.write(b"{\"selected\":1}");
    assert_eq!(
        fixture
            .registry
            .capture_snapshot(&fixture.source, authorization("epoch-2"))
            .await
            .unwrap()
            .generation,
        changed.generation + 1
    );
}

#[tokio::test]
async fn oversized_snapshot_does_not_advance_a_cursor_or_queue_bytes() {
    let limits = CaptureLimits {
        chunk_bytes: 8,
        read_bytes: 16,
        max_record_bytes: 32,
        ..Default::default()
    };
    let fixture = Fixture::new(limits, 1024 * 1024).await;
    let file = std::fs::File::create(&fixture.source.locator).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    let error = fixture
        .registry
        .capture_snapshot(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap_err();
    assert!(matches!(error, CaptureSpoolError::RecordLimit(32)));
    assert!(fixture.registry.pending(1).await.unwrap().is_empty());
    assert_eq!(
        fixture
            .registry
            .cursor(&fixture.source, &authorization("epoch-1").epoch)
            .await
            .unwrap()
            .offset,
        0
    );
}

#[tokio::test]
async fn native_ownership_proof_names_the_identity_record_after_blank_and_unrelated_lines() {
    let mut fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    fixture.source.identity_record_ordinal = Some(2);
    fixture
        .write(b"\n{\"type\":\"queue-operation\"}\n{\"sessionId\":\"native\",\"type\":\"user\"}\n");
    let pass = fixture.capture().await;
    let proof = fixture
        .registry
        .source_proof(&fixture.source.reference, pass.generation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proof, pass.observations[2]);
    assert_ne!(proof, pass.observations[0]);
    let retained = fixture.registry.pending(4).await.unwrap();
    assert!(String::from_utf8(bytes(&retained[2]))
        .unwrap()
        .contains("sessionId"));
}

#[tokio::test]
async fn ordinary_capture_backpressure_preserves_capacity_for_an_explicit_process_gap() {
    let fixture = Fixture::new(CaptureLimits::default(), 8192).await;
    fixture
        .registry
        .capture_frame(
            "vendor/rpc",
            TransportKind::LiveStdout,
            vec![b'x'; 5600],
            authorization("epoch-1"),
        )
        .await
        .unwrap();
    let failed = fixture
        .registry
        .capture_frame(
            "vendor/rpc",
            TransportKind::LiveStdout,
            vec![b'x'; 500],
            authorization("epoch-1"),
        )
        .await;
    assert!(matches!(
        failed,
        Err(CaptureSpoolError::Spool(SpoolError::Overflow { .. }))
    ));
    let gap = fixture
        .registry
        .capture_frame(
            "vendor/lifecycle",
            TransportKind::Supervisor,
            b"{\"kind\":\"capture_gap\"}".to_vec(),
            authorization("epoch-1"),
        )
        .await
        .unwrap();
    let queued = fixture.registry.pending(4).await.unwrap();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[1].capture.observation_ref, gap);
    assert!(
        fixture.registry.spool.size_bytes().await.unwrap() <= fixture.registry.spool.max_bytes()
    );
}

#[tokio::test]
async fn capture_delivery_barrier_cannot_be_extended_by_later_live_frames() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    let first = fixture
        .registry
        .capture_frame(
            "rpc",
            TransportKind::LiveStdout,
            b"first\n".to_vec(),
            authorization("epoch-1"),
        )
        .await
        .unwrap();
    let frontier = fixture.registry.pending_frontiers().await.unwrap();
    let second = fixture
        .registry
        .capture_frame(
            "rpc",
            TransportKind::LiveStdout,
            b"second\n".to_vec(),
            authorization("epoch-1"),
        )
        .await
        .unwrap();
    let frozen = fixture
        .registry
        .pending_metadata_through(10, Some(&frontier))
        .await
        .unwrap();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].capture.observation_ref, first);
    fixture
        .registry
        .acknowledge(&[CaptureReceipt::Retained {
            observation: first,
            admission_seq: 1,
        }])
        .await
        .unwrap();
    assert!(fixture
        .registry
        .pending_metadata_through(10, Some(&frontier))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture.registry.pending_metadata(10).await.unwrap()[0]
            .capture
            .observation_ref,
        second
    );
}

#[cfg(unix)]
#[tokio::test]
async fn killed_acknowledgment_commits_receipts_queue_release_and_watermark_together() {
    use std::os::unix::process::ExitStatusExt;
    for stage in [
        "before_ack_transaction",
        "before_ack_commit",
        "after_ack_commit",
    ] {
        let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
        fixture.write(b"{\"key\":1,\"key\":2}\r\nsecond\nunrelated tail\n");
        fixture.capture().await;
        let original = fixture.registry.pending(10).await.unwrap();
        assert_eq!(original.len(), 3);
        let receipts = vec![
            CaptureReceipt::Retained {
                observation: original[0].capture.observation_ref.clone(),
                admission_seq: 17,
            },
            CaptureReceipt::DiscardedByDeletion {
                observation: original[1].capture.observation_ref.clone(),
                tombstone: "deleted-scope".into(),
            },
        ];
        std::fs::write(
            fixture.dir.path().join("receipts.json"),
            serde_json::to_vec(&receipts).unwrap(),
        )
        .unwrap();
        // No original connection remains to checkpoint the WAL during the kill.
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
        let committed = stage == "after_ack_commit";
        let queued = recovered.pending(10).await.unwrap();
        assert_eq!(queued, original[if committed { 2 } else { 0 }..], "{stage}");
        let through: i64 = sqlx::query_scalar("SELECT acked_through FROM capture_producers")
            .fetch_one(&recovered.spool.pool)
            .await
            .unwrap();
        assert_eq!(through, if committed { 2 } else { 0 }, "{stage}");
        let persisted: Vec<String> =
            sqlx::query_scalar("SELECT receipt_json FROM capture_receipts ORDER BY producer_seq")
                .fetch_all(&recovered.spool.pool)
                .await
                .unwrap();
        let persisted: Vec<CaptureReceipt> = persisted
            .iter()
            .map(|value| serde_json::from_str(value).unwrap())
            .collect();
        assert_eq!(
            persisted,
            if committed { receipts.clone() } else { vec![] },
            "{stage}"
        );
        assert_eq!(
            recovered.acknowledge(&receipts).await.unwrap(),
            if committed { 0 } else { 2 }
        );
        assert_eq!(recovered.pending(10).await.unwrap(), original[2..]);
        assert_eq!(
            bytes(&recovered.pending(10).await.unwrap()[0]),
            b"unrelated tail\n"
        );
        // Retried terminal receipts are immutable, including their dispositions.
        assert!(recovered
            .acknowledge(&[CaptureReceipt::Retained {
                observation: original[1].capture.observation_ref.clone(),
                admission_seq: 18,
            }])
            .await
            .is_err());
        recovered
            .acknowledge(&[CaptureReceipt::Retained {
                observation: original[2].capture.observation_ref.clone(),
                admission_seq: 19,
            }])
            .await
            .unwrap();
        assert!(recovered.pending(10).await.unwrap().is_empty());
        assert_eq!(recovered.spool.size_bytes().await.unwrap(), 0);
        assert!(recovered
            .capture_jsonl(&fixture.source, authorization("epoch-1"))
            .await
            .unwrap()
            .observations
            .is_empty());
    }
}

#[tokio::test]
async fn recycled_file_identity_cannot_hide_changed_committed_bytes_after_restart() {
    let fixture = Fixture::new(CaptureLimits::default(), 1024 * 1024).await;
    let first = b"{\"id\":\"same\",\"text\":\"old\"}\r\n";
    let replacement = b"{\"id\":\"same\",\"text\":\"new\"}\r\n";
    fixture.write(first);
    fixture.capture().await;
    let original = fixture.registry.pending(10).await.unwrap();
    let replacement_path = fixture.dir.path().join("replacement.jsonl");
    std::fs::write(&replacement_path, replacement).unwrap();
    std::fs::rename(&replacement_path, &fixture.source.locator).unwrap();
    // Make the durable observation equal the replacement's complete file identity.
    // This deterministically exercises identity reuse, including a filesystem
    // without a distinct birth time, without waiting for the kernel to recycle an
    // inode. The stored prefix digest and all captured bytes remain untouched.
    let recycled = serde_json::to_string(&FileIdentity::of(
        &std::fs::metadata(&fixture.source.locator).unwrap(),
    ))
    .unwrap();
    sqlx::query(
        "UPDATE capture_generations SET file_identity=? WHERE source_ref=? AND generation=0",
    )
    .bind(recycled)
    .bind(fixture.source.reference.encode())
    .execute(&fixture.registry.spool.pool)
    .await
    .unwrap();
    let registry = fixture.reopen().await;
    let pass = registry
        .capture_jsonl(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap();
    assert_eq!(
        pass.generation, 1,
        "matching file identity cannot certify the retained prefix"
    );
    assert!(pass.generation_changed);
    let mut queued = registry.pending(10).await.unwrap();
    queued.sort_by_key(|item| item.capture.source_position.generation);
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0], original[0]);
    assert_eq!(bytes(&queued[0]), first);
    assert_eq!(bytes(&queued[1]), replacement);
    assert_eq!(
        queued[1].capture.source_position.source,
        fixture.source.reference
    );
    assert_eq!(queued[1].capture.source_position.generation, 1);
    assert_eq!(
        queued[1].capture.source_position.locator,
        SourceLocator::Jsonl {
            byte_start: 0,
            byte_end: replacement.len() as u64,
            record_ordinal: 0,
        }
    );
    assert!(registry
        .capture_jsonl(&fixture.source, authorization("epoch-1"))
        .await
        .unwrap()
        .observations
        .is_empty());
}
