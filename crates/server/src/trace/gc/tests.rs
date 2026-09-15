use super::*;
use crate::{
    storage::cas::BlobStore,
    trace::{
        evidence::{
            catalogue::{CatalogueLimits, PayloadPin, PinOwnerKind, ReadyManifest, ScopeProof},
            AdmissionLimits, EvidenceStore,
        },
        generation::{GenerationManifest, Snapshot},
        upload::{CaptureUploads, UploadLimits},
    },
};
use sylph_protocol::{
    trace::{
        AgentRef, IngestionEpoch, IngestionScope, PayloadChunk, PayloadCompleteness,
        PayloadEncoding, PayloadManifest,
    },
    SessionId,
};

struct Fixture {
    dir: tempfile::TempDir,
    evidence: EvidenceStore,
    catalogue: EvidenceCatalogue,
    epoch: IngestionEpoch,
    scope: IngestionScope,
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
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:root").unwrap());
        let scope = IngestionScope::Agent(agent.clone());
        let epoch = evidence.authorize_scope(&scope, &[agent]).await.unwrap();
        let catalogue =
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
        Self {
            dir,
            evidence,
            catalogue,
            epoch,
            scope,
        }
    }
    fn gc(&self) -> TraceGarbageCollector {
        TraceGarbageCollector::new(self.catalogue.clone(), GcLimits::default()).unwrap()
    }
    async fn publish(&self, name: &str) {
        let manifest = GenerationManifest {
            generation: name.into(),
            semantic_schema: "s".into(),
            mapper_builds: std::collections::BTreeMap::new(),
            reducer_build: "r".into(),
            capture_inventory_build: "i".into(),
            encoding_build: "e".into(),
        };
        let ticket = self
            .catalogue
            .begin_build(manifest.clone(), 0)
            .await
            .unwrap();
        self.catalogue.register_file(&ticket, name).await.unwrap();
        self.catalogue
            .pin_generation_scope(
                name,
                &ScopeProof {
                    scope: self.scope.clone(),
                    epoch: self.epoch.clone(),
                },
                0,
            )
            .await
            .unwrap();
        self.catalogue
            .record_ready(
                &ticket,
                &ReadyManifest {
                    manifest,
                    relative_file: ticket.relative_file.clone(),
                    database_identity: name.into(),
                    snapshot: Snapshot {
                        generation: name.into(),
                        evidence_cut: 0,
                        view_revision: 0,
                    },
                    fence_token: 2,
                },
            )
            .await
            .unwrap();
        let guard = self.catalogue.acquire_publication(&ticket).await.unwrap();
        self.catalogue.publish(&ticket, &guard, 0).await.unwrap();
        self.catalogue.release_publication(&guard).await.unwrap();
    }
}

#[tokio::test]
async fn raw_and_manifest_addresses_share_physical_bytes_until_the_last_typed_root_releases() {
    let fixture = Fixture::new().await;
    let body = b"retained content";
    let chunk = fixture.evidence.stage_payload(body).await.unwrap();
    let manifest = PayloadManifest {
        chunks: vec![PayloadChunk {
            digest: chunk.clone(),
            chunk_offset: 0,
            length: body.len() as u64,
            payload_offset: 0,
        }],
        media_type: "text/plain".into(),
        encoding: PayloadEncoding::Utf8,
        decoded_length: Some(body.len() as u64),
        completeness: PayloadCompleteness::Complete,
    };
    let reference = fixture.evidence.stage_manifest(&manifest).await.unwrap();
    let encoded = manifest.canonical_bytes().unwrap();
    let raw = fixture.evidence.stage_payload(&encoded).await.unwrap();
    assert_eq!(raw.digest(), reference.digest());
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Export,
            "manifest",
            &[PayloadPin {
                payload: reference.clone(),
                epoch: fixture.epoch.clone(),
            }],
        )
        .await
        .unwrap();
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Export,
            "raw",
            &[PayloadPin {
                payload: raw.clone(),
                epoch: fixture.epoch.clone(),
            }],
        )
        .await
        .unwrap();
    let gc = fixture.gc();
    assert_eq!(gc.run_once(0).await.unwrap().collected, 0);
    fixture
        .catalogue
        .release_owner_pins(PinOwnerKind::Export, "manifest", 10)
        .await
        .unwrap();
    let result = gc.run_once(1).await.unwrap();
    assert_eq!(result.collected, 1);
    assert!(fixture.evidence.payload_bytes(&chunk, 100).await.is_err());
    assert_eq!(
        fixture
            .evidence
            .payload_bytes(&raw, encoded.len())
            .await
            .unwrap(),
        encoded
    );
    fixture
        .catalogue
        .release_owner_pins(PinOwnerKind::Export, "raw", 10)
        .await
        .unwrap();
    assert_eq!(gc.run_once(2).await.unwrap().collected, 1);
    assert!(fixture
        .evidence
        .payload_bytes(&raw, encoded.len())
        .await
        .is_err());
}

#[tokio::test]
async fn an_old_generation_lease_pins_its_payload_until_expiry_then_collection_releases_it() {
    let fixture = Fixture::new().await;
    fixture.publish("old").await;
    let payload = fixture
        .evidence
        .stage_payload(b"old generation body")
        .await
        .unwrap();
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Generation,
            "old",
            &[PayloadPin {
                payload: payload.clone(),
                epoch: fixture.epoch.clone(),
            }],
        )
        .await
        .unwrap();
    let pending = fixture
        .catalogue
        .begin_lease("reader", 10, 100)
        .await
        .unwrap();
    fixture
        .catalogue
        .finish_lease(
            &pending,
            &Snapshot {
                generation: "old".into(),
                evidence_cut: 0,
                view_revision: 0,
            },
            11,
        )
        .await
        .unwrap();
    fixture.publish("new").await;
    let gc = fixture.gc();
    assert_eq!(gc.run_once(50).await.unwrap().collected, 0);
    assert_eq!(
        fixture.evidence.payload_bytes(&payload, 100).await.unwrap(),
        b"old generation body"
    );
    assert_eq!(gc.run_once(111).await.unwrap().collected, 1);
    assert!(fixture.evidence.payload_bytes(&payload, 100).await.is_err());
}

#[tokio::test]
async fn deleting_one_epoch_cannot_collect_shared_bytes_retained_by_another_recipient() {
    let fixture = Fixture::new().await;
    let payload = fixture
        .evidence
        .stage_payload(b"shared recipient bytes")
        .await
        .unwrap();
    let recipient = AgentRef::Session(SessionId::decode("env7fk3:codex:recipient").unwrap());
    let scope = IngestionScope::Agent(recipient.clone());
    let epoch = fixture
        .evidence
        .authorize_scope(&scope, &[recipient])
        .await
        .unwrap();
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Export,
            "origin",
            &[PayloadPin {
                payload: payload.clone(),
                epoch: fixture.epoch.clone(),
            }],
        )
        .await
        .unwrap();
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Export,
            "recipient",
            &[PayloadPin {
                payload: payload.clone(),
                epoch,
            }],
        )
        .await
        .unwrap();
    fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    let gc = fixture.gc();
    assert_eq!(gc.run_once(0).await.unwrap().collected, 0);
    assert_eq!(
        fixture.evidence.payload_bytes(&payload, 100).await.unwrap(),
        b"shared recipient bytes"
    );
    fixture.evidence.delete_scope(&scope).await.unwrap();
    assert_eq!(gc.run_once(1).await.unwrap().collected, 1);
    assert!(fixture.evidence.payload_bytes(&payload, 100).await.is_err());
}

#[tokio::test]
async fn interrupted_unlink_retries_durably_and_fences_new_reference_publication() {
    let fixture = Fixture::new().await;
    let payload = fixture
        .evidence
        .stage_payload(b"unreferenced orphan")
        .await
        .unwrap();
    let mut gc = fixture.gc();
    gc.crash_after_unlink = true;
    assert!(gc.run_once(0).await.is_err());
    assert!(fixture.evidence.payload_bytes(&payload, 100).await.is_err());
    assert!(fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "racing",
            payload.clone(),
            fixture.epoch.clone(),
            1
        )
        .await
        .is_err());
    assert!(fixture
        .evidence
        .stage_payload(b"unreferenced orphan")
        .await
        .is_err());
    let resumed = fixture.gc();
    assert_eq!(resumed.run_once(2).await.unwrap().collected, 1);
    let ticket = fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "fresh",
            payload.clone(),
            fixture.epoch.clone(),
            3,
        )
        .await
        .unwrap();
    fixture
        .evidence
        .stage_payload(b"unreferenced orphan")
        .await
        .unwrap();
    fixture.catalogue.finish_upload(&ticket).await.unwrap();
    assert_eq!(resumed.run_once(4).await.unwrap().collected, 0);
}

#[tokio::test]
async fn two_collectors_cannot_share_a_stale_reservation_that_deletes_republished_bytes() {
    let fixture = Fixture::new().await;
    let bytes = b"republished after collection";
    let payload = fixture.evidence.stage_payload(bytes).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mut first = fixture.gc();
    first.before_unlink = Some((entered.clone(), resume.clone()));
    let task = tokio::spawn(async move { first.run_once(0).await.unwrap() });
    entered.notified().await;
    let second = fixture.gc();
    let competing = second.run_once(1).await;
    let upload = fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "too-early",
            payload.clone(),
            fixture.epoch.clone(),
            1,
        )
        .await;
    // Release even when the faulty competing collector ran, so the test failure
    // is the preserved byte contract rather than a hanging peer.
    resume.notify_one();
    task.await.unwrap();
    assert!(competing.is_err());
    assert!(upload.is_err());
    let ticket = fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "republished",
            payload.clone(),
            fixture.epoch.clone(),
            2,
        )
        .await
        .unwrap();
    fixture.evidence.stage_payload(bytes).await.unwrap();
    fixture.catalogue.finish_upload(&ticket).await.unwrap();
    assert_eq!(second.run_once(3).await.unwrap().collected, 0);
    assert_eq!(
        fixture.evidence.payload_bytes(&payload, 100).await.unwrap(),
        bytes
    );
}

#[tokio::test]
async fn collection_bounds_candidates_and_ignores_files_outside_its_durable_digest_inventory() {
    let fixture = Fixture::new().await;
    let external = BlobStore::new(fixture.dir.path().join("blobs"));
    let external_hash = external.put(b"not owned by catalogue").await.unwrap();
    for n in 0..7 {
        fixture
            .evidence
            .stage_payload(format!("orphan {n}").as_bytes())
            .await
            .unwrap();
    }
    let gc = TraceGarbageCollector::new(
        fixture.catalogue.clone(),
        GcLimits {
            max_candidates: 2,
            max_epoch_checks: 1,
            max_root_releases: 1,
        },
    )
    .unwrap();
    let mut total = 0;
    for tick in 0..5 {
        let result = gc.run_once(tick).await.unwrap();
        assert!(result.inspected <= 2);
        assert!(result.collected <= 2);
        total += result.collected;
    }
    assert_eq!(total, 7);
    assert_eq!(
        external.get(&external_hash).await.unwrap().unwrap(),
        b"not owned by catalogue"
    );
    let uploads = CaptureUploads::new(fixture.catalogue.clone(), UploadLimits::default()).unwrap();
    uploads.recover_interrupted_writes().await.unwrap();
}

#[cfg(unix)]
#[test]
fn gc_kill_probe() {
    let Some(path) = std::env::var_os("SYLPH_GC_CRASH_ROOT") else {
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
        let catalogue = EvidenceCatalogue::new(evidence, CatalogueLimits::default()).unwrap();
        let mut gc = TraceGarbageCollector::new(catalogue, GcLimits::default()).unwrap();
        gc.crash_after_unlink = true;
        gc.run_once(0).await.unwrap();
        panic!("kill probe survived");
    });
}

#[cfg(unix)]
#[tokio::test]
async fn sigkill_after_unlink_retries_the_catalogue_reservation_without_deleting_newly_republished_bytes(
) {
    use std::os::unix::process::ExitStatusExt;
    let fixture = Fixture::new().await;
    let bytes = b"killed collector orphan";
    let payload = fixture.evidence.stage_payload(bytes).await.unwrap();
    fixture.evidence.close().await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["trace::gc::tests::gc_kill_probe", "--exact", "--nocapture"])
        .env("SYLPH_GC_CRASH_ROOT", fixture.dir.path())
        .env("SYLPH_GC_CRASH_PROCESS", "1")
        .output()
        .await
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence = EvidenceStore::open(
        fixture.dir.path().join("evidence.db"),
        BlobStore::new(fixture.dir.path().join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap();
    let catalogue = EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
    let gc = TraceGarbageCollector::new(catalogue.clone(), GcLimits::default()).unwrap();
    assert!(gc.run_once(1).await.is_err());
    gc.recover_interrupted_pass().await.unwrap();
    assert_eq!(gc.run_once(2).await.unwrap().collected, 1);
    let ticket = catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "fresh-after-crash",
            payload.clone(),
            fixture.epoch.clone(),
            3,
        )
        .await
        .unwrap();
    evidence.stage_payload(bytes).await.unwrap();
    catalogue.finish_upload(&ticket).await.unwrap();
    assert_eq!(gc.run_once(4).await.unwrap().collected, 0);
    assert_eq!(evidence.payload_bytes(&payload, 100).await.unwrap(), bytes);
}
