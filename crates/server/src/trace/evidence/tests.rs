use super::*;
use sylph_protocol::trace::IngestionScope;
use sylph_protocol::{
    trace::{
        capture::{
            AttachmentAvailability, CaptureAttachment, PayloadChunk, PayloadEncoding, PayloadRange,
            TimeBasis, TimeEvidence, TimeUnit, TransportKind,
        },
        identity::{SourceLocator, SourcePosition, SourceRef},
    },
    SessionId,
};

const RAW: &[u8] = b"{\"text\":\"A\\u0041\",\"text\":\"AA\"}\r\n";

#[tokio::test]
async fn raw_bytes_equal_to_a_manifest_encoding_keep_their_own_kind_in_both_insert_orders() {
    for raw_first in [true, false] {
        let fixture = Fixture::new().await;
        let chunk = fixture
            .store
            .stage_payload(b"actual decoded body")
            .await
            .unwrap();
        let manifest = PayloadManifest {
            chunks: vec![PayloadChunk {
                digest: chunk,
                chunk_offset: 0,
                length: 19,
                payload_offset: 0,
            }],
            media_type: "text/plain".into(),
            encoding: PayloadEncoding::Utf8,
            decoded_length: Some(19),
            completeness: PayloadCompleteness::Complete,
        };
        let encoded = manifest_bytes(&manifest).unwrap();
        let (raw, structured) = if raw_first {
            (
                fixture.store.stage_payload(&encoded).await.unwrap(),
                fixture.store.stage_manifest(&manifest).await.unwrap(),
            )
        } else {
            let structured = fixture.store.stage_manifest(&manifest).await.unwrap();
            (
                fixture.store.stage_payload(&encoded).await.unwrap(),
                structured,
            )
        };
        assert_ne!(raw, structured);
        assert_eq!(raw.digest(), structured.digest());
        assert_eq!(
            fixture
                .store
                .payload_bytes(&raw, encoded.len())
                .await
                .unwrap(),
            encoded
        );
        assert_eq!(
            fixture.store.payload_bytes(&structured, 100).await.unwrap(),
            b"actual decoded body"
        );
        assert_eq!(
            fixture.store.payload_manifest(&structured).await.unwrap(),
            Some(manifest)
        );
        assert!(fixture
            .store
            .payload_manifest(&raw)
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn partial_sidecars_keep_available_ranges_without_claiming_a_complete_capture() {
    let fixture = Fixture::new().await;
    let chunk = fixture.store.stage_payload(b"available").await.unwrap();
    let manifest = PayloadManifest {
        chunks: vec![PayloadChunk {
            digest: chunk,
            chunk_offset: 0,
            length: 9,
            payload_offset: 100,
        }],
        media_type: "text/plain".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: None,
        completeness: PayloadCompleteness::KnownRanges {
            ranges: vec![PayloadRange {
                start: 100,
                end_exclusive: 109,
            }],
        },
    };
    let reference = fixture.store.stage_manifest(&manifest).await.unwrap();
    assert_eq!(
        fixture.store.payload_manifest(&reference).await.unwrap(),
        Some(manifest.clone())
    );
    let range = fixture
        .store
        .payload_range(&reference, 102, 4)
        .await
        .unwrap();
    assert_eq!(range.bytes, b"aila");
    assert_eq!(range.offset, 102);
    assert_eq!(range.total_length, None);
    assert_eq!(range.coverage, manifest.completeness);
    assert!(matches!(
        fixture.store.payload_range(&reference, 99, 4).await,
        Err(EvidenceError::UnavailableRange)
    ));
    assert!(matches!(
        fixture.store.payload_bytes(&reference, 200).await,
        Err(EvidenceError::UnavailableRange)
    ));
    let incomplete = make_capture(&fixture.agent, &fixture.epoch, 1, 1, reference.clone());
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &incomplete.observation_ref.issuer,
                std::slice::from_ref(&incomplete)
            )
            .await,
        Err(EvidenceError::Invalid(_))
    ));
    let mut complete = fixture.capture(1, 1, RAW).await;
    complete.attachments.push(CaptureAttachment {
        locator: "partial.log".into(),
        source_position: None,
        observed_version: None,
        payload_ref: Some(reference),
        availability: AttachmentAvailability::Partial {
            reason: "only the retained range is readable".into(),
        },
        relation: "tool_output".into(),
    });
    let mut false_complete = complete.clone();
    false_complete.attachments[0].availability = AttachmentAvailability::Captured;
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &false_complete.observation_ref.issuer,
                std::slice::from_ref(&false_complete)
            )
            .await,
        Err(EvidenceError::Invalid(_))
    ));
    fixture
        .store
        .admit_batch(
            &complete.observation_ref.issuer,
            std::slice::from_ref(&complete),
        )
        .await
        .unwrap();
}

struct Fixture {
    directory: tempfile::TempDir,
    store: EvidenceStore,
    agent: AgentRef,
    epoch: IngestionEpoch,
}

#[tokio::test]
async fn cancelled_custom_begin_cannot_poison_the_next_capture_upload() {
    use sqlx::Connection;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Condvar, Mutex,
        },
        task::{Context, Poll, Wake, Waker},
        time::Duration,
    };

    struct Gate {
        entered: mpsc::Sender<()>,
        released: Mutex<bool>,
        resume: Condvar,
        used: AtomicBool,
    }
    impl Gate {
        fn block_once(&self) {
            if self.used.swap(true, Ordering::SeqCst) {
                return;
            }
            self.entered.send(()).unwrap();
            let released = self.released.lock().unwrap();
            let (released, timeout) = self
                .resume
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(
                *released && !timeout.timed_out(),
                "test must release the SQLite worker"
            );
        }
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.resume.notify_all();
        }
    }
    impl Wake for Gate {
        fn wake(self: Arc<Self>) {
            self.block_once();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.block_once();
        }
    }
    struct Unblock(Arc<Gate>);
    impl Drop for Unblock {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    fn gate() -> (Arc<Gate>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(Gate {
                entered: tx,
                released: Mutex::new(false),
                resume: Condvar::new(),
                used: AtomicBool::new(false),
            }),
            rx,
        )
    }

    let fixture = Fixture::new().await;
    let mut connection = fixture.store.writer.acquire().await.unwrap();
    let (begin_entered, begin_signal) = gate();
    let (ack_entered, ack_signal) = gate();
    let _begin_unblock = Unblock(begin_entered.clone());
    let _ack_unblock = Unblock(ack_entered.clone());
    let callback = begin_entered.clone();
    connection
        .lock_handle()
        .await
        .unwrap()
        .set_progress_handler(1, move || {
            callback.block_once();
            true
        });
    let waker = Waker::from(ack_entered.clone());
    let mut context = Context::from_waker(&waker);
    let mut begin = connection.begin_with("BEGIN IMMEDIATE");
    assert!(matches!(begin.as_mut().poll(&mut context), Poll::Pending));
    begin_signal.recv_timeout(Duration::from_secs(2)).unwrap();
    begin_entered.release();
    ack_signal.recv_timeout(Duration::from_secs(2)).unwrap();
    // The worker is paused in the acknowledgement's wake. Receiving that ack
    // advances begin_with to its handle-verification await, before Transaction
    // exists to roll back on drop. No scheduler timing decides this window.
    assert!(matches!(begin.as_mut().poll(&mut context), Poll::Pending));
    drop(begin);
    ack_entered.release();
    connection.ping().await.unwrap();
    assert!(
        connection.is_in_transaction(),
        "reproduction must cancel after BEGIN succeeds"
    );
    connection
        .lock_handle()
        .await
        .unwrap()
        .remove_progress_handler();
    drop(connection);

    let capture = fixture.capture(1, 1, RAW).await;
    let admitted = fixture
        .store
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    assert_eq!(admitted.evidence_cut, 1);
    assert_eq!(
        fixture
            .store
            .capture_payload(&capture.observation_ref, RAW.len())
            .await
            .unwrap()
            .unwrap(),
        RAW
    );
    let mut connection = fixture.store.writer.acquire().await.unwrap();
    assert!(!connection.is_in_transaction());
    let transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn ordinary_dropped_transaction_rolls_back_and_reuses_its_physical_connection() {
    use sqlx::Connection;
    let fixture = Fixture::new().await;
    let mut connection = fixture.store.writer.acquire().await.unwrap();
    // TEMP state belongs to the physical connection and disappears on close.
    sqlx::query("CREATE TEMP TABLE release_probe(value TEXT)")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO release_probe VALUES('committed')")
        .execute(&mut *connection)
        .await
        .unwrap();
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("INSERT INTO release_probe VALUES('rolled back')")
        .execute(&mut *transaction)
        .await
        .unwrap();
    drop(transaction);
    drop(connection);
    let mut connection = fixture.store.writer.acquire().await.unwrap();
    assert!(!connection.is_in_transaction());
    let values: Vec<String> = sqlx::query_scalar("SELECT value FROM release_probe")
        .fetch_all(&mut *connection)
        .await
        .unwrap();
    assert_eq!(
        values,
        vec!["committed"],
        "normal rollback must preserve the existing connection"
    );
}

impl Fixture {
    async fn new() -> Self {
        Self::with_limits(AdmissionLimits::default()).await
    }

    async fn with_limits(limits: AdmissionLimits) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = EvidenceStore::open(
            directory.path().join("evidence.db"),
            BlobStore::new(directory.path().join("blobs")),
            limits,
        )
        .await
        .unwrap();
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:root").unwrap());
        let epoch = store
            .authorize_scope(
                &IngestionScope::Agent(agent.clone()),
                std::slice::from_ref(&agent),
            )
            .await
            .unwrap();
        Self {
            directory,
            store,
            agent,
            epoch,
        }
    }

    async fn capture(&self, sequence: u64, ordinal: u64, bytes: &[u8]) -> Capture {
        let payload = self.store.stage_payload(bytes).await.unwrap();
        make_capture(&self.agent, &self.epoch, sequence, ordinal, payload)
    }

    async fn reopen(&mut self) {
        self.store.close().await;
        self.store = EvidenceStore::open(
            self.directory.path().join("evidence.db"),
            BlobStore::new(self.directory.path().join("blobs")),
            self.store.limits().clone(),
        )
        .await
        .unwrap();
    }
}

fn make_capture(
    agent: &AgentRef,
    epoch: &IngestionEpoch,
    sequence: u64,
    ordinal: u64,
    payload: PayloadRef,
) -> Capture {
    let issuer = Issuer::Environment(agent.environment().clone());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "capture-producer".into(),
            producer_epoch: "producer-incarnation-1".into(),
            producer_seq: sequence,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: "native-file-instance".into(),
            },
            generation: 1,
            locator: SourceLocator::Frame { ordinal },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: vec![agent.clone()],
        captured_at: TimeEvidence {
            domain: "capture-clock".into(),
            value: 1234,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: TransportKind::NativeJsonl,
        vendor_build: Some("measured-vendor-build".into()),
        schema_fingerprint: None,
        payload_ref: payload,
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}

#[tokio::test]
async fn equal_bytes_at_distinct_source_positions_remain_distinct_occurrences() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    let second = fixture.capture(2, 2, RAW).await;
    let receipt = fixture
        .store
        .admit_batch(&first.observation_ref.issuer, &[first.clone(), second])
        .await
        .unwrap();
    assert_eq!(receipt.accepted_through, 2);
    let page = fixture.store.admissions_after(0, 10).await.unwrap();
    assert_ne!(
        page.captures[0].occurrence_id,
        page.captures[1].occurrence_id
    );
    assert_ne!(
        page.captures[0].canonical_observation_ref,
        page.captures[1].canonical_observation_ref
    );
    assert_eq!(
        fixture
            .store
            .capture_payload(&first.observation_ref, RAW.len())
            .await
            .unwrap()
            .unwrap(),
        RAW
    );
}

#[tokio::test]
async fn durable_redelivery_reuses_receipt_and_reread_reuses_source_occurrence() {
    let mut fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    let initial = fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    fixture.reopen().await;
    let replay = fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    assert_eq!(replay.evidence_cut, initial.evidence_cut);
    assert_eq!(replay.accepted_through, 1);
    assert!(!replay.receipts[0].newly_recorded);
    let mut reread = first.clone();
    reread.observation_ref.producer_epoch = "new-producer-incarnation".into();
    reread.captured_at.value = 9999;
    fixture
        .store
        .admit_batch(
            &reread.observation_ref.issuer,
            std::slice::from_ref(&reread),
        )
        .await
        .unwrap();
    let page = fixture.store.admissions_after(0, 10).await.unwrap();
    assert_eq!(page.captures.len(), 2);
    assert_eq!(
        page.captures[0].occurrence_id,
        page.captures[1].occurrence_id
    );
    assert_eq!(
        page.captures[1].canonical_observation_ref,
        first.observation_ref
    );
}

#[tokio::test]
async fn a_collision_rolls_back_the_entire_producer_batch_and_cannot_skip_a_prefix() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    let second = fixture.capture(2, 2, b"next record").await;
    let collision = fixture.capture(3, 1, b"rewritten source bytes").await;
    assert!(matches!(
        fixture
            .store
            .admit_batch(&first.observation_ref.issuer, &[second.clone(), collision])
            .await,
        Err(EvidenceError::SourceCollision)
    ));
    assert_eq!(
        fixture
            .store
            .accepted_through(&first.observation_ref)
            .await
            .unwrap(),
        1
    );
    assert_eq!(fixture.store.evidence_cut().await.unwrap(), 1);
    assert!(fixture
        .store
        .receipt(&second.observation_ref)
        .await
        .unwrap()
        .is_none());
    let gap = fixture.capture(4, 4, b"fourth record").await;
    assert!(matches!(
        fixture
            .store
            .admit_batch(&first.observation_ref.issuer, &[gap])
            .await,
        Err(EvidenceError::Gap {
            expected: 2,
            received: 4
        })
    ));
    let mut wrong_receipt = first.clone();
    wrong_receipt.vendor_build = Some("different-build".into());
    assert!(matches!(
        fixture
            .store
            .admit_batch(&first.observation_ref.issuer, &[wrong_receipt])
            .await,
        Err(EvidenceError::ReceiptCollision)
    ));
}

#[tokio::test]
async fn source_generation_changes_keep_old_bytes_and_require_explicit_new_positions() {
    let fixture = Fixture::new().await;
    let original = fixture.capture(1, 0, b"original").await;
    let mut replacement = fixture.capture(2, 0, b"replacement").await;
    replacement.source_position.generation = 2;
    fixture
        .store
        .admit_batch(
            &original.observation_ref.issuer,
            &[original.clone(), replacement.clone()],
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .store
            .capture_payload(&original.observation_ref, 100)
            .await
            .unwrap()
            .unwrap(),
        b"original"
    );
    assert_eq!(
        fixture
            .store
            .capture_payload(&replacement.observation_ref, 100)
            .await
            .unwrap()
            .unwrap(),
        b"replacement"
    );
}

#[tokio::test]
async fn manifest_layout_does_not_change_source_occurrence_identity_or_exact_bytes() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    let split = RAW.len() / 2;
    let left = fixture.store.stage_payload(&RAW[..split]).await.unwrap();
    let right = fixture.store.stage_payload(&RAW[split..]).await.unwrap();
    let manifest = PayloadManifest {
        chunks: vec![
            PayloadChunk {
                digest: left,
                chunk_offset: 0,
                length: split as u64,
                payload_offset: 0,
            },
            PayloadChunk {
                digest: right,
                chunk_offset: 0,
                length: (RAW.len() - split) as u64,
                payload_offset: split as u64,
            },
        ],
        media_type: "application/x-ndjson".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: Some(RAW.len() as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let reference = fixture.store.stage_manifest(&manifest).await.unwrap();
    assert_ne!(reference, first.payload_ref);
    assert_eq!(
        fixture
            .store
            .payload_bytes(&reference, RAW.len())
            .await
            .unwrap(),
        RAW
    );
    let mut reread = first.clone();
    reread.observation_ref.producer_seq = 2;
    reread.payload_ref = reference;
    fixture
        .store
        .admit_batch(&first.observation_ref.issuer.clone(), &[first, reread])
        .await
        .unwrap();
    let page = fixture.store.admissions_after(0, 10).await.unwrap();
    assert_eq!(
        page.captures[0].occurrence_id,
        page.captures[1].occurrence_id
    );
}

#[tokio::test]
async fn chunk_limits_preserve_a_large_record_and_refuse_oversized_materialization() {
    let fixture = Fixture::with_limits(AdmissionLimits {
        max_chunk_bytes: 4,
        ..AdmissionLimits::default()
    })
    .await;
    let data = b"1234567890";
    assert!(matches!(
        fixture.store.stage_payload(data).await,
        Err(EvidenceError::Limit(_))
    ));
    let mut chunks = Vec::new();
    let mut offset = 0;
    for bytes in data.chunks(fixture.store.limits().max_chunk_bytes) {
        chunks.push(PayloadChunk {
            digest: fixture.store.stage_payload(bytes).await.unwrap(),
            chunk_offset: 0,
            length: bytes.len() as u64,
            payload_offset: offset,
        });
        offset += bytes.len() as u64;
    }
    let manifest = PayloadManifest {
        chunks,
        media_type: "application/octet-stream".into(),
        encoding: PayloadEncoding::Binary,
        decoded_length: Some(data.len() as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let reference = fixture.store.stage_manifest(&manifest).await.unwrap();
    let capture = make_capture(&fixture.agent, &fixture.epoch, 1, 1, reference.clone());
    fixture
        .store
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.store.payload_bytes(&reference, 9).await,
        Err(EvidenceError::Limit(_))
    ));
    assert_eq!(
        fixture.store.payload_bytes(&reference, 10).await.unwrap(),
        data
    );
}

#[tokio::test]
async fn captured_attachments_are_verified_and_rooted_before_the_receipt_commits() {
    let fixture = Fixture::new().await;
    let mut capture = fixture.capture(1, 1, RAW).await;
    let attachment = PayloadRef::bytes(blake3::hash(b"attachment bytes").to_hex()).unwrap();
    capture.attachments.push(CaptureAttachment {
        locator: "vendor-output.txt".into(),
        source_position: None,
        observed_version: Some("version-1".into()),
        payload_ref: Some(attachment.clone()),
        availability: AttachmentAvailability::Captured,
        relation: "runtime_output".into(),
    });
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture)
            )
            .await,
        Err(EvidenceError::Payload(_))
    ));
    assert_eq!(
        fixture
            .store
            .accepted_through(&capture.observation_ref)
            .await
            .unwrap(),
        0
    );
    fixture
        .store
        .stage_payload(b"attachment bytes")
        .await
        .unwrap();
    fixture
        .store
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    let rooted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM observation_payload_roots WHERE payload_ref = ?")
            .bind(attachment.as_str())
            .fetch_one(&fixture.store.readers)
            .await
            .unwrap();
    assert_eq!(rooted, 1);
}

#[tokio::test]
async fn corrupt_payloads_cannot_advance_an_acknowledged_prefix() {
    let fixture = Fixture::new().await;
    let capture = fixture.capture(1, 1, RAW).await;
    let digest = capture.payload_ref.digest();
    let path = fixture
        .directory
        .path()
        .join("blobs")
        .join(&digest[..2])
        .join(&digest[2..4])
        .join(digest);
    tokio::fs::write(path, b"corrupt bytes").await.unwrap();
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture)
            )
            .await,
        Err(EvidenceError::Payload(_))
    ));
    assert_eq!(
        fixture
            .store
            .accepted_through(&capture.observation_ref)
            .await
            .unwrap(),
        0
    );
    assert_eq!(fixture.store.evidence_cut().await.unwrap(), 0);
}

#[tokio::test]
async fn deleted_spool_items_release_the_prefix_without_resurrecting_evidence() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    let tombstone = fixture
        .store
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    assert!(fixture
        .store
        .capture_payload(&first.observation_ref, 100)
        .await
        .unwrap()
        .is_none());
    let replay = fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    assert!(!replay.receipts[0].newly_recorded);
    let never_uploaded = PayloadRef::bytes(blake3::hash(b"never retained").to_hex()).unwrap();
    let stale = make_capture(&fixture.agent, &fixture.epoch, 2, 2, never_uploaded);
    let result = fixture
        .store
        .admit_batch(&first.observation_ref.issuer, &[stale])
        .await
        .unwrap();
    assert_eq!(result.accepted_through, 2);
    assert_eq!(result.evidence_cut, 1);
    assert_eq!(
        result.receipts[0].disposition,
        ReceiptDisposition::DiscardedByDeletion {
            tombstone_id: tombstone
        }
    );
    let page = fixture.store.admissions_after(0, 1).await.unwrap();
    assert!(page.captures.is_empty());
    assert_eq!(page.next_after, 1);
    assert!(!page.has_more);
    let fresh_epoch = fixture
        .store
        .readopt_scope(
            &IngestionScope::Agent(fixture.agent.clone()),
            std::slice::from_ref(&fixture.agent),
        )
        .await
        .unwrap();
    assert_ne!(fresh_epoch, fixture.epoch);
    let mut attempted_retag = first.clone();
    attempted_retag.ingestion_authorization_epoch = fresh_epoch.clone();
    assert!(matches!(
        fixture
            .store
            .admit_batch(&first.observation_ref.issuer, &[attempted_retag])
            .await,
        Err(EvidenceError::ReceiptCollision)
    ));
    let mut recaptured = first.clone();
    recaptured.observation_ref.producer_seq = 3;
    recaptured.ingestion_authorization_epoch = fresh_epoch;
    fixture
        .store
        .admit_batch(&first.observation_ref.issuer, &[recaptured.clone()])
        .await
        .unwrap();
    assert_eq!(
        fixture
            .store
            .capture_payload(&recaptured.observation_ref, 100)
            .await
            .unwrap()
            .unwrap(),
        RAW
    );
    assert!(fixture
        .store
        .capture_payload(&first.observation_ref, 100)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .store
            .admissions_after(0, 10)
            .await
            .unwrap()
            .captures
            .len(),
        1
    );
}

#[tokio::test]
async fn inventory_epoch_and_authenticated_environment_are_checked_before_receipt() {
    let fixture = Fixture::new().await;
    let mut capture = fixture.capture(1, 1, RAW).await;
    let foreign = AgentRef::Session(SessionId::decode("env7fk3:codex:other").unwrap());
    let foreign_epoch = fixture
        .store
        .authorize_scope(
            &IngestionScope::Agent(foreign.clone()),
            std::slice::from_ref(&foreign),
        )
        .await
        .unwrap();
    capture.ingestion_authorization_epoch = foreign_epoch;
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture)
            )
            .await,
        Err(EvidenceError::Authority)
    ));
    capture.ingestion_authorization_epoch = fixture.epoch.clone();
    capture.declared_subject_refs = vec![AgentRef::Session(
        SessionId::decode("env7fk2:codex:unregistered").unwrap(),
    )];
    assert!(matches!(
        fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture)
            )
            .await,
        Err(EvidenceError::Authority)
    ));
    assert_eq!(
        fixture
            .store
            .accepted_through(&capture.observation_ref)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn bounded_admission_pages_report_more_work_across_deleted_rows() {
    let fixture = Fixture::new().await;
    let mut captures = Vec::new();
    for index in 1..=4 {
        captures.push(fixture.capture(index, index, RAW).await);
    }
    fixture
        .store
        .admit_batch(&captures[0].observation_ref.issuer, &captures)
        .await
        .unwrap();
    fixture
        .store
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    let page = fixture.store.admissions_after(0, 2).await.unwrap();
    assert!(page.captures.is_empty());
    assert!(page.has_more);
    assert_eq!(page.next_after, 2);
    let tail = fixture
        .store
        .admissions_after(page.next_after, 2)
        .await
        .unwrap();
    assert_eq!(tail.next_after, 4);
    assert!(!tail.has_more);
}

#[tokio::test]
async fn a_page_never_reports_a_cut_containing_an_unseen_concurrent_admission() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    fixture
        .store
        .admit_batch(&first.observation_ref.issuer, std::slice::from_ref(&first))
        .await
        .unwrap();
    let second = fixture.capture(2, 2, b"concurrent admission").await;
    let read = std::sync::Arc::new(tokio::sync::Notify::new());
    let resume = std::sync::Arc::new(tokio::sync::Notify::new());
    let page_task = {
        let store = fixture.store.clone();
        let read = read.clone();
        let resume = resume.clone();
        tokio::spawn(async move {
            store
                .admissions_page(0, 10, Some((&read, &resume)))
                .await
                .unwrap()
        })
    };
    read.notified().await;
    fixture
        .store
        .admit_batch(
            &second.observation_ref.issuer,
            std::slice::from_ref(&second),
        )
        .await
        .unwrap();
    resume.notify_one();
    let page = page_task.await.unwrap();
    assert_eq!(page.evidence_cut, 1);
    assert_eq!(page.captures.len(), 1);
    assert!(!page.has_more);
    let tail = fixture
        .store
        .admissions_after(page.next_after, 10)
        .await
        .unwrap();
    assert_eq!(tail.evidence_cut, 2);
    assert_eq!(
        tail.captures[0].capture.observation_ref,
        second.observation_ref
    );
}

#[tokio::test]
async fn an_admission_page_stops_at_its_configured_byte_budget_without_losing_the_tail() {
    let mut fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    let second = fixture.capture(2, 2, RAW).await;
    fixture.store.limits.max_read_bytes = serde_json::to_vec(&first).unwrap().len();
    fixture
        .store
        .admit_batch(
            &first.observation_ref.issuer,
            &[first.clone(), second.clone()],
        )
        .await
        .unwrap();
    let page = fixture.store.admissions_after(0, 10).await.unwrap();
    assert_eq!(page.captures.len(), 1);
    assert_eq!(
        page.captures[0].capture.observation_ref,
        first.observation_ref
    );
    assert!(page.has_more);
    let returned: usize = page
        .captures
        .iter()
        .map(|entry| serde_json::to_vec(&entry.capture).unwrap().len())
        .sum();
    assert!(returned <= fixture.store.limits().max_read_bytes);
    let tail = fixture
        .store
        .admissions_after(page.next_after, 10)
        .await
        .unwrap();
    assert_eq!(
        tail.captures[0].capture.observation_ref,
        second.observation_ref
    );
    assert!(!tail.has_more);
}

#[tokio::test]
async fn transaction_faults_never_acknowledge_an_uncommitted_observation() {
    for point in [
        CrashPoint::BeforeTransaction,
        CrashPoint::BeforeCommit,
        CrashPoint::AfterCommit,
    ] {
        let mut fixture = Fixture::new().await;
        let capture = fixture.capture(1, 1, RAW).await;
        assert!(
            matches!(fixture.store.admit_inner(&capture.observation_ref.issuer, std::slice::from_ref(&capture), Some(point)).await, Err(EvidenceError::Crash(found)) if found == point)
        );
        fixture.reopen().await;
        let expected_prefix = u64::from(point == CrashPoint::AfterCommit);
        assert_eq!(
            fixture
                .store
                .accepted_through(&capture.observation_ref)
                .await
                .unwrap(),
            expected_prefix
        );
        let retry = fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        assert_eq!(retry.accepted_through, 1);
        assert_eq!(
            fixture
                .store
                .admissions_after(0, 10)
                .await
                .unwrap()
                .captures
                .len(),
            1
        );
        assert_eq!(
            fixture
                .store
                .capture_payload(&capture.observation_ref, RAW.len())
                .await
                .unwrap()
                .unwrap(),
            RAW
        );
    }
}

/// Invoked as a separate process by the test below. Normal suite execution has
/// no fixture argument, so it cannot terminate the test runner itself.
#[cfg(unix)]
#[test]
fn admission_kill_probe() {
    let Some(directory) = std::env::var_os("SYLPH_EVIDENCE_CRASH_FIXTURE") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let point = match std::env::var("SYLPH_EVIDENCE_CRASH_POINT")
        .unwrap()
        .as_str()
    {
        "before_payload_sync" => CrashPoint::BeforePayloadSync,
        "after_payload_sync" => CrashPoint::AfterPayloadSync,
        "before_transaction" => CrashPoint::BeforeTransaction,
        "before_commit" => CrashPoint::BeforeCommit,
        "after_commit" => CrashPoint::AfterCommit,
        other => panic!("invalid crash point {other}"),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store = EvidenceStore::open(
            directory.join("evidence.db"),
            BlobStore::new(directory.join("blobs")),
            AdmissionLimits::default(),
        )
        .await
        .unwrap();
        let capture: Capture =
            serde_json::from_slice(&std::fs::read(directory.join("capture.json")).unwrap())
                .unwrap();
        assert_eq!(
            store.stage_payload_inner(RAW, Some(point)).await.unwrap(),
            capture.payload_ref
        );
        store
            .admit_inner(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
                Some(point),
            )
            .await
            .unwrap();
        panic!("the subprocess did not die at its crash point");
    });
}

#[cfg(unix)]
#[tokio::test]
async fn sigkill_during_admission_recovers_exact_bytes_and_only_the_committed_prefix() {
    use std::os::unix::process::ExitStatusExt;

    for (point, committed) in [
        ("before_payload_sync", false),
        ("after_payload_sync", false),
        ("before_transaction", false),
        ("before_commit", false),
        ("after_commit", true),
    ] {
        let mut fixture = Fixture::new().await;
        let reference = PayloadRef::bytes(blake3::hash(RAW).to_hex()).unwrap();
        let capture = make_capture(&fixture.agent, &fixture.epoch, 1, 1, reference);
        std::fs::write(
            fixture.directory.path().join("capture.json"),
            serde_json::to_vec(&capture).unwrap(),
        )
        .unwrap();
        fixture.store.close().await;
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "trace::evidence::tests::admission_kill_probe",
                "--exact",
                "--nocapture",
            ])
            .env("SYLPH_EVIDENCE_CRASH_FIXTURE", fixture.directory.path())
            .env("SYLPH_EVIDENCE_CRASH_POINT", point)
            .env("SYLPH_EVIDENCE_CRASH_PROCESS", "1")
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture.reopen().await;
        assert_eq!(
            fixture
                .store
                .accepted_through(&capture.observation_ref)
                .await
                .unwrap(),
            u64::from(committed)
        );
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&fixture.store.readers)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        fixture.store.stage_payload(RAW).await.unwrap();
        let retry = fixture
            .store
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        assert_eq!(retry.accepted_through, 1);
        assert_eq!(retry.receipts[0].newly_recorded, !committed);
        assert_eq!(
            fixture
                .store
                .capture_payload(&capture.observation_ref, RAW.len())
                .await
                .unwrap()
                .unwrap(),
            RAW
        );
        assert_eq!(
            fixture
                .store
                .admissions_after(0, 10)
                .await
                .unwrap()
                .captures
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn pre_identity_inventory_retains_frames_without_minting_a_session() {
    use sylph_protocol::trace::InventoryRef;
    let fixture = Fixture::new().await;
    let inventory = IngestionScope::Inventory(InventoryRef {
        environment: fixture.agent.environment().clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        instance: "launch-before-id".into(),
    });
    let epoch = fixture
        .store
        .authorize_scope(&inventory, &[])
        .await
        .unwrap();
    let mut captured = fixture.capture(1, 1, b"{\"startup\":true}\n").await;
    captured.ingestion_authorization_epoch = epoch.clone();
    captured.declared_subject_refs.clear();
    let admitted = fixture
        .store
        .admit_batch(
            &captured.observation_ref.issuer,
            std::slice::from_ref(&captured),
        )
        .await
        .unwrap();
    assert!(matches!(
        admitted.receipts[0].disposition,
        ReceiptDisposition::Retained { .. }
    ));
    assert_eq!(
        fixture
            .store
            .capture_payload(&captured.observation_ref, 1024)
            .await
            .unwrap(),
        Some(b"{\"startup\":true}\n".to_vec())
    );
    assert!(fixture
        .store
        .admissions_after(0, 10)
        .await
        .unwrap()
        .captures[0]
        .capture
        .declared_subject_refs
        .is_empty());
    let extended = fixture
        .store
        .authorize_scope(&inventory, std::slice::from_ref(&fixture.agent))
        .await
        .unwrap();
    assert_eq!(extended, epoch);
    let foreign = AgentRef::Session(SessionId::decode("env7fk2:pi:native-other").unwrap());
    assert!(matches!(
        fixture
            .store
            .authorize_scope(&inventory, std::slice::from_ref(&foreign))
            .await,
        Err(EvidenceError::Authority)
    ));
}

#[tokio::test]
async fn deletion_of_a_pre_identity_inventory_rejects_its_old_capture_epoch_after_readoption() {
    use sylph_protocol::trace::InventoryRef;
    let fixture = Fixture::new().await;
    let inventory = IngestionScope::Inventory(InventoryRef {
        environment: fixture.agent.environment().clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        instance: "launch-discarded".into(),
    });
    let epoch = fixture
        .store
        .authorize_scope(&inventory, &[])
        .await
        .unwrap();
    let mut captured = fixture.capture(1, 1, b"old startup\n").await;
    captured.ingestion_authorization_epoch = epoch.clone();
    captured.declared_subject_refs.clear();
    let tombstone = fixture.store.delete_scope(&inventory).await.unwrap();
    let fresh = fixture.store.readopt_scope(&inventory, &[]).await.unwrap();
    assert_ne!(fresh, epoch);
    let result = fixture
        .store
        .admit_batch(
            &captured.observation_ref.issuer,
            std::slice::from_ref(&captured),
        )
        .await
        .unwrap();
    assert_eq!(
        result.receipts[0].disposition,
        ReceiptDisposition::DiscardedByDeletion {
            tombstone_id: tombstone
        }
    );
    assert_eq!(
        fixture
            .store
            .capture_payload(&captured.observation_ref, 1024)
            .await
            .unwrap(),
        None
    );
    let mut recaptured = captured.clone();
    recaptured.observation_ref.producer_seq = 2;
    recaptured.ingestion_authorization_epoch = fresh;
    let result = fixture
        .store
        .admit_batch(
            &recaptured.observation_ref.issuer,
            std::slice::from_ref(&recaptured),
        )
        .await
        .unwrap();
    assert!(matches!(
        result.receipts[0].disposition,
        ReceiptDisposition::Retained { .. }
    ));
    assert_eq!(recaptured.source_position, captured.source_position);
}

#[tokio::test]
async fn current_scope_epoch_returns_live_authorization_and_omits_deleted_scope() {
    let fixture = Fixture::new().await;
    let scope = IngestionScope::Agent(fixture.agent.clone());
    assert_eq!(
        fixture.store.current_scope_epoch(&scope).await.unwrap(),
        Some(fixture.epoch.clone())
    );
    fixture.store.delete_scope(&scope).await.unwrap();
    assert_eq!(
        fixture.store.current_scope_epoch(&scope).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn source_replay_is_indexed_and_excludes_rereads_other_sources_generations_and_epochs() {
    let fixture = Fixture::new().await;
    let first = fixture.capture(1, 1, RAW).await;
    let mut reread = first.clone();
    reread.observation_ref.producer_seq = 2;
    let mut other_source = fixture.capture(3, 2, RAW).await;
    other_source.source_position.source.instance = "other-source".into();
    let mut other_generation = fixture.capture(4, 2, RAW).await;
    other_generation.source_position.generation = 2;
    let next = fixture.capture(5, 3, b"second exact record").await;
    let issuer = &first.observation_ref.issuer;
    fixture
        .store
        .admit_batch(
            issuer,
            &[
                first.clone(),
                reread,
                other_source,
                other_generation,
                next.clone(),
            ],
        )
        .await
        .unwrap();
    let source = &first.source_position.source;
    let record = fixture
        .store
        .source_record_after(source, 1, &fixture.epoch, 0, 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.admission_seq, 1);
    assert_eq!(record.capture.observation_ref, first.observation_ref);
    let record = fixture
        .store
        .source_record_after(source, 1, &fixture.epoch, 1, 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.admission_seq, 5);
    assert_eq!(record.capture.observation_ref, next.observation_ref);
    assert_eq!(
        fixture
            .store
            .payload_bytes(&record.capture.payload_ref, 128)
            .await
            .unwrap(),
        b"second exact record"
    );
    assert!(fixture
        .store
        .source_record_after(source, 1, &fixture.epoch, 1, 4)
        .await
        .unwrap()
        .is_none());
    let other_agent = AgentRef::Session(SessionId::decode("env7fk2:codex:another-epoch").unwrap());
    let other_scope = IngestionScope::Agent(other_agent.clone());
    let other_epoch = fixture
        .store
        .authorize_scope(&other_scope, std::slice::from_ref(&other_agent))
        .await
        .unwrap();
    let mut foreign = make_capture(&other_agent, &other_epoch, 1, 4, next.payload_ref.clone());
    foreign.observation_ref.producer_id = "another-epoch-producer".into();
    foreign.source_position.source = source.clone();
    fixture.store.admit_batch(issuer, &[foreign]).await.unwrap();
    assert!(fixture
        .store
        .source_record_after(source, 1, &fixture.epoch, 5, 6)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .store
            .source_record_after(source, 1, &other_epoch, 0, 6)
            .await
            .unwrap()
            .unwrap()
            .admission_seq,
        6
    );
    fixture
        .store
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    assert!(fixture
        .store
        .source_record_after(source, 1, &fixture.epoch, 0, 6)
        .await
        .unwrap()
        .is_none());
    let plan:Vec<String>=sqlx::query("EXPLAIN QUERY PLAN SELECT admission_seq FROM source_replay_records WHERE ingestion_epoch=? AND source_ref=? AND generation=? AND admission_seq>? AND admission_seq<=? ORDER BY admission_seq LIMIT 1")
        .bind(fixture.epoch.as_str()).bind(source.encode()).bind(1i64).bind(0i64).bind(6i64).fetch_all(&fixture.store.readers).await.unwrap().iter().map(|row|row.get("detail")).collect();
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH source_replay_records USING PRIMARY KEY")),
        "{plan:?}"
    );
    assert!(
        plan.iter()
            .all(|line| !line.contains("SCAN source_replay_records")
                && !line.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}
