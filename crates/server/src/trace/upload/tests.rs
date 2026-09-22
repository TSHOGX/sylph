use super::*;
use crate::{
    trace::cas::BlobStore,
    trace::{
        evidence::{
            catalogue::{CatalogueLimits, EvidenceCatalogue},
            AdmissionLimits,
        },
        gc::{GcLimits, TraceGarbageCollector},
    },
};
use sylph_protocol::trace::{ObservationRef, PayloadManifest};
use sylph_protocol::{
    trace::{
        AgentRef, IngestionEpoch, IngestionScope, PayloadChunk, PayloadCompleteness,
        PayloadEncoding, SourceLocator, SourcePosition, SourceRef, TimeBasis, TimeEvidence,
        TimeUnit, TransportKind,
    },
    SessionId,
};
use sylph_protocol::{CaptureUploadTicket, PayloadDeclaration};

mod attachments;

struct Fixture {
    dir: tempfile::TempDir,
    evidence: EvidenceStore,
    uploads: CaptureUploads,
    agent: AgentRef,
    epoch: IngestionEpoch,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let evidence = EvidenceStore::open(
            dir.path().join("evidence.db"),
            BlobStore::new(dir.path().join("blobs")),
            AdmissionLimits::default(),
        )
        .await
        .unwrap();
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:one").unwrap());
        let epoch = evidence
            .authorize_scope(
                &IngestionScope::Agent(agent.clone()),
                std::slice::from_ref(&agent),
            )
            .await
            .unwrap();
        let uploads = CaptureUploads::new(
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap(),
            UploadLimits::default(),
        )
        .unwrap();
        Self {
            dir,
            evidence,
            uploads,
            agent,
            epoch,
        }
    }
    fn capture(&self, sequence: u64, position: u64, bytes: &[u8]) -> Capture {
        capture(
            &self.agent,
            &self.epoch,
            sequence,
            position,
            PayloadRef::bytes(blake3::hash(bytes).to_hex()).unwrap(),
        )
    }
    async fn ticket(&self, capture: &Capture, bytes: &[u8]) -> CaptureUploadTicket {
        let begin = self
            .uploads
            .begin(
                &capture.observation_ref.issuer,
                capture.clone(),
                vec![PayloadDeclaration::Bytes {
                    reference: capture.payload_ref.clone(),
                    length: bytes.len() as u64,
                }],
            )
            .await
            .unwrap();
        let BeginCapture::Pending(ticket) = begin else {
            panic!("expected pending capture")
        };
        ticket
    }
    async fn reopen(&mut self) {
        self.evidence.close().await;
        self.evidence = EvidenceStore::open(
            self.dir.path().join("evidence.db"),
            BlobStore::new(self.dir.path().join("blobs")),
            AdmissionLimits::default(),
        )
        .await
        .unwrap();
        self.uploads = CaptureUploads::new(
            EvidenceCatalogue::new(self.evidence.clone(), CatalogueLimits::default()).unwrap(),
            UploadLimits::default(),
        )
        .unwrap();
    }
}
fn capture(
    agent: &AgentRef,
    epoch: &IngestionEpoch,
    sequence: u64,
    position: u64,
    payload: PayloadRef,
) -> Capture {
    let issuer = Issuer::Environment(agent.environment().clone());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "capture".into(),
            producer_epoch: "incarnation".into(),
            producer_seq: sequence,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: "native-source".into(),
            },
            generation: 1,
            locator: SourceLocator::Frame { ordinal: position },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: vec![agent.clone()],
        captured_at: TimeEvidence {
            domain: "clock".into(),
            value: 1,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: TransportKind::NativeJsonl,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: payload,
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}

#[tokio::test]
async fn knowing_another_environments_cas_digest_cannot_supply_new_capture_bytes() {
    let fixture = Fixture::new().await;
    let bytes = b"private bytes from environment one";
    let first = fixture.capture(1, 1, bytes);
    let ticket = fixture.ticket(&first, bytes).await;
    fixture
        .uploads
        .put_chunk(
            &first.observation_ref.issuer,
            &ticket,
            &first.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&first.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    let agent = AgentRef::Session(SessionId::decode("env7fk3:codex:foreign").unwrap());
    let epoch = fixture
        .evidence
        .authorize_scope(
            &IngestionScope::Agent(agent.clone()),
            std::slice::from_ref(&agent),
        )
        .await
        .unwrap();
    let foreign = capture(&agent, &epoch, 1, 1, first.payload_ref.clone());
    let foreign_ticket = fixture.ticket(&foreign, bytes).await;
    assert!(matches!(
        fixture
            .uploads
            .finish(&foreign.observation_ref.issuer, &foreign_ticket)
            .await,
        Err(UploadError::Incomplete)
    ));
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 1);
    assert!(matches!(
        fixture
            .uploads
            .put_chunk(
                &first.observation_ref.issuer,
                &foreign_ticket,
                &foreign.payload_ref,
                bytes
            )
            .await,
        Err(UploadError::Authority)
    ));
    fixture
        .uploads
        .put_chunk(
            &foreign.observation_ref.issuer,
            &foreign_ticket,
            &foreign.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&foreign.observation_ref.issuer, &foreign_ticket)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&foreign.observation_ref, bytes.len())
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
}

#[tokio::test]
async fn only_a_same_epoch_same_source_occurrence_can_reuse_retained_transfer_proof() {
    let fixture = Fixture::new().await;
    let bytes = b"repeated body";
    let first = fixture.capture(1, 1, bytes);
    let first_ticket = fixture.ticket(&first, bytes).await;
    fixture
        .uploads
        .put_chunk(
            &first.observation_ref.issuer,
            &first_ticket,
            &first.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&first.observation_ref.issuer, &first_ticket)
        .await
        .unwrap();
    let reread = fixture.capture(2, 1, bytes);
    let reread_ticket = fixture.ticket(&reread, bytes).await;
    fixture
        .uploads
        .finish(&reread.observation_ref.issuer, &reread_ticket)
        .await
        .unwrap();
    let repeated = fixture.capture(3, 2, bytes);
    let repeated_ticket = fixture.ticket(&repeated, bytes).await;
    assert!(matches!(
        fixture
            .uploads
            .finish(&repeated.observation_ref.issuer, &repeated_ticket)
            .await,
        Err(UploadError::Incomplete)
    ));
    let page = fixture.evidence.admissions_after(0, 10).await.unwrap();
    assert_eq!(
        page.captures[0].occurrence_id,
        page.captures[1].occurrence_id
    );
}

#[tokio::test]
async fn deletion_discards_an_incomplete_upload_and_re_adoption_requires_fresh_owned_bytes() {
    let fixture = Fixture::new().await;
    let bytes = b"not retained yet";
    let original = fixture.capture(1, 1, bytes);
    let ticket = fixture.ticket(&original, bytes).await;
    fixture
        .evidence
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    let fresh_epoch = fixture
        .evidence
        .readopt_scope(
            &IngestionScope::Agent(fixture.agent.clone()),
            std::slice::from_ref(&fixture.agent),
        )
        .await
        .unwrap();
    let discarded = fixture
        .uploads
        .finish(&original.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert!(matches!(
        discarded.receipts[0].disposition,
        crate::trace::evidence::ReceiptDisposition::DiscardedByDeletion { .. }
    ));
    assert_eq!(discarded.evidence_cut, 0);
    fixture.evidence.stage_payload(bytes).await.unwrap();
    let fresh = capture(
        &fixture.agent,
        &fresh_epoch,
        2,
        1,
        original.payload_ref.clone(),
    );
    let fresh_ticket = fixture.ticket(&fresh, bytes).await;
    assert!(matches!(
        fixture
            .uploads
            .finish(&fresh.observation_ref.issuer, &fresh_ticket)
            .await,
        Err(UploadError::Incomplete)
    ));
    fixture
        .uploads
        .put_chunk(
            &fresh.observation_ref.issuer,
            &fresh_ticket,
            &fresh.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&fresh.observation_ref.issuer, &fresh_ticket)
        .await
        .unwrap();
}

#[tokio::test]
async fn deleting_an_abandoned_upload_releases_capacity_without_the_old_client_returning() {
    let mut fixture = Fixture::new().await;
    fixture.uploads.limits.max_open_uploads = 1;
    let abandoned = fixture.capture(1, 1, b"abandoned");
    fixture.ticket(&abandoned, b"abandoned").await;
    fixture
        .evidence
        .delete_scope(&IngestionScope::Agent(fixture.agent.clone()))
        .await
        .unwrap();
    fixture.epoch = fixture
        .evidence
        .readopt_scope(
            &IngestionScope::Agent(fixture.agent.clone()),
            std::slice::from_ref(&fixture.agent),
        )
        .await
        .unwrap();
    let fresh = fixture.capture(2, 2, b"fresh");
    let ticket = fixture.ticket(&fresh, b"fresh").await;
    fixture
        .uploads
        .put_chunk(
            &fresh.observation_ref.issuer,
            &ticket,
            &fresh.payload_ref,
            b"fresh",
        )
        .await
        .unwrap();
    // The producer must still receive the old scope's terminal discard before
    // its contiguous prefix can advance; quota release does not invent a receipt.
    let BeginCapture::Complete(discard) = fixture
        .uploads
        .begin(&abandoned.observation_ref.issuer.clone(), abandoned, vec![])
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(discard.accepted_through, 1);
}

#[tokio::test]
async fn interrupted_manifest_upload_resumes_its_owned_chunks_and_commits_only_when_complete() {
    let mut fixture = Fixture::new().await;
    let left = b"left-part";
    let right = b"right-part";
    let a = PayloadRef::bytes(blake3::hash(left).to_hex()).unwrap();
    let b = PayloadRef::bytes(blake3::hash(right).to_hex()).unwrap();
    let manifest = PayloadManifest {
        chunks: vec![
            PayloadChunk {
                digest: a.clone(),
                chunk_offset: 0,
                length: left.len() as u64,
                payload_offset: 0,
            },
            PayloadChunk {
                digest: b.clone(),
                chunk_offset: 0,
                length: right.len() as u64,
                payload_offset: left.len() as u64,
            },
        ],
        media_type: "text/plain".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: Some((left.len() + right.len()) as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let reference = crate::trace::evidence::manifest_ref(&manifest).unwrap();
    let capture = capture(&fixture.agent, &fixture.epoch, 1, 1, reference.clone());
    let declarations = vec![
        PayloadDeclaration::Manifest {
            reference: reference.clone(),
            manifest: manifest.clone(),
        },
        PayloadDeclaration::Bytes {
            reference: a.clone(),
            length: left.len() as u64,
        },
        PayloadDeclaration::Bytes {
            reference: b.clone(),
            length: right.len() as u64,
        },
    ];
    let BeginCapture::Pending(ticket) = fixture
        .uploads
        .begin(
            &capture.observation_ref.issuer,
            capture.clone(),
            declarations.clone(),
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    fixture
        .uploads
        .put_chunk(&capture.observation_ref.issuer, &ticket, &a, left)
        .await
        .unwrap();
    fixture.reopen().await;
    let BeginCapture::Pending(retry) = fixture
        .uploads
        .begin(
            &capture.observation_ref.issuer,
            capture.clone(),
            declarations,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(retry, ticket);
    assert!(matches!(
        fixture
            .uploads
            .finish(&capture.observation_ref.issuer, &ticket)
            .await,
        Err(UploadError::Incomplete)
    ));
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 0);
    assert!(matches!(
        fixture
            .uploads
            .put_chunk(&capture.observation_ref.issuer, &ticket, &b, b"wrong")
            .await,
        Err(UploadError::BytesMismatch)
    ));
    fixture
        .uploads
        .put_chunk(&capture.observation_ref.issuer, &ticket, &b, right)
        .await
        .unwrap();
    let admitted = fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(admitted.accepted_through, 1);
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, 100)
            .await
            .unwrap()
            .unwrap(),
        [left.as_slice(), right.as_slice()].concat()
    );
    let replay = fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert!(!replay.receipts[0].newly_recorded);
    assert_eq!(replay.evidence_cut, 1);
}

#[tokio::test]
async fn admitted_upload_transfers_its_pin_atomically_and_cancelled_upload_releases_its_bytes() {
    let fixture = Fixture::new().await;
    let bytes = b"survive upload transition";
    let capture = fixture.capture(1, 1, bytes);
    let ticket = fixture.ticket(&capture, bytes).await;
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    let gc = TraceGarbageCollector::new(fixture.uploads.catalogue().clone(), GcLimits::default())
        .unwrap();
    assert_eq!(gc.run_once(10).await.unwrap().collected, 0);
    fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(gc.run_once(20).await.unwrap().collected, 0);
    let other = fixture.capture(2, 2, b"discard temporary upload");
    let temp = fixture.ticket(&other, b"discard temporary upload").await;
    fixture
        .uploads
        .put_chunk(
            &other.observation_ref.issuer,
            &temp,
            &other.payload_ref,
            b"discard temporary upload",
        )
        .await
        .unwrap();
    fixture
        .uploads
        .cancel(&other.observation_ref.issuer, &temp)
        .await
        .unwrap();
    let progress = gc.run_once(30).await.unwrap();
    assert_eq!(progress.collected, 1);
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, 100)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
    assert!(fixture
        .evidence
        .payload_bytes(&other.payload_ref, 100)
        .await
        .is_err());
}

#[cfg(unix)]
#[test]
fn capture_upload_kill_probe() {
    let Some(path) = std::env::var_os("SYLPH_CAPTURE_UPLOAD_ROOT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let evidence = EvidenceStore::open(
            path.join("evidence.db"),
            BlobStore::new(path.join("blobs")),
            AdmissionLimits::default(),
        )
        .await
        .unwrap();
        let uploads = CaptureUploads::new(
            EvidenceCatalogue::new(evidence, CatalogueLimits::default()).unwrap(),
            UploadLimits::default(),
        )
        .unwrap();
        let capture: Capture =
            serde_json::from_slice(&std::fs::read(path.join("capture.json")).unwrap()).unwrap();
        let ticket: CaptureUploadTicket =
            serde_json::from_slice(&std::fs::read(path.join("ticket.json")).unwrap()).unwrap();
        uploads
            .put_chunk_inner(
                &capture.observation_ref.issuer,
                &ticket,
                &capture.payload_ref,
                b"crash transfer bytes",
                true,
            )
            .await
            .unwrap();
        panic!("kill probe survived");
    });
}

#[cfg(unix)]
#[tokio::test]
async fn sigkill_after_blob_sync_before_chunk_receipt_requires_the_same_owned_transfer_on_retry() {
    use std::os::unix::process::ExitStatusExt;
    let mut fixture = Fixture::new().await;
    let bytes = b"crash transfer bytes";
    let capture = fixture.capture(1, 1, bytes);
    let ticket = fixture.ticket(&capture, bytes).await;
    std::fs::write(
        fixture.dir.path().join("capture.json"),
        serde_json::to_vec(&capture).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fixture.dir.path().join("ticket.json"),
        serde_json::to_vec(&ticket).unwrap(),
    )
    .unwrap();
    fixture.evidence.close().await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "trace::upload::tests::capture_upload_kill_probe",
            "--exact",
            "--nocapture",
        ])
        .env("SYLPH_CAPTURE_UPLOAD_ROOT", fixture.dir.path())
        .env("SYLPH_CAPTURE_UPLOAD_CRASH_PROCESS", "1")
        .output()
        .await
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.reopen().await;
    fixture.uploads.recover_interrupted_writes().await.unwrap();
    assert!(matches!(
        fixture
            .uploads
            .finish(&capture.observation_ref.issuer, &ticket)
            .await,
        Err(UploadError::Incomplete)
    ));
    let gc = TraceGarbageCollector::new(fixture.uploads.catalogue().clone(), GcLimits::default())
        .unwrap();
    assert_eq!(gc.run_once(0).await.unwrap().collected, 0);
    fixture
        .uploads
        .put_chunk(
            &capture.observation_ref.issuer,
            &ticket,
            &capture.payload_ref,
            bytes,
        )
        .await
        .unwrap();
    fixture
        .uploads
        .finish(&capture.observation_ref.issuer, &ticket)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, bytes.len())
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
}

#[tokio::test]
async fn admitted_chunk_retries_verify_exact_bytes_and_preserve_the_original_receipt() {
    let fixture = Fixture::new().await;
    let bytes = b"durable exact bytes";
    let capture = fixture.capture(1, 1, bytes);
    let issuer = &capture.observation_ref.issuer;
    let ticket = fixture.ticket(&capture, bytes).await;
    fixture
        .uploads
        .put_chunk(issuer, &ticket, &capture.payload_ref, bytes)
        .await
        .unwrap();
    let first = fixture.uploads.finish(issuer, &ticket).await.unwrap();
    fixture
        .uploads
        .put_chunk(issuer, &ticket, &capture.payload_ref, bytes)
        .await
        .unwrap();
    let second = fixture.uploads.finish(issuer, &ticket).await.unwrap();
    assert_eq!(
        first.receipts[0].observation_ref,
        second.receipts[0].observation_ref
    );
    assert_eq!(
        first.receipts[0].disposition,
        second.receipts[0].disposition
    );
    assert!(!second.receipts[0].newly_recorded);
    let mut changed = bytes.to_vec();
    changed[0] ^= 1;
    assert!(matches!(
        fixture
            .uploads
            .put_chunk(issuer, &ticket, &capture.payload_ref, &changed)
            .await,
        Err(UploadError::BytesMismatch)
    ));
    assert!(matches!(
        fixture
            .uploads
            .put_chunk(issuer, &ticket, &capture.payload_ref, &[])
            .await,
        Err(UploadError::BytesMismatch)
    ));
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 1);
}
