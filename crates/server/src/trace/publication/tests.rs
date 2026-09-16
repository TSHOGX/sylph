use super::*;
use crate::{
    trace::cas::BlobStore,
    trace::{
        evidence::{catalogue::CatalogueLimits, AdmissionLimits, EvidenceStore},
        generation::{ObjectUpdate, ProjectionBatch},
    },
};
use std::sync::atomic::{AtomicUsize, Ordering};
use sylph_protocol::{
    trace::{
        AgentRef, AgentThread, AnyObjectRef, Capture, IngestionEpoch, IngestionScope, Issuer,
        ObservationRef, SourceLocator, SourcePosition, SourceRef, TimeBasis, TimeEvidence,
        TimeUnit, TraceObject, TransportKind,
    },
    SessionId,
};

struct Fixture {
    dir: tempfile::TempDir,
    evidence: EvidenceStore,
    catalogue: EvidenceCatalogue,
    coordinator: PublicationCoordinator,
    agent: AgentRef,
    epoch: IngestionEpoch,
}
impl Fixture {
    async fn new(limits: PublicationLimits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let evidence = open_evidence(dir.path()).await;
        let agent = agent();
        let epoch = evidence
            .authorize_scope(
                &IngestionScope::Agent(agent.clone()),
                std::slice::from_ref(&agent),
            )
            .await
            .unwrap();
        let catalogue =
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
        let coordinator =
            PublicationCoordinator::new(catalogue.clone(), dir.path().join("projections"), limits)
                .unwrap();
        Self {
            dir,
            evidence,
            catalogue,
            coordinator,
            agent,
            epoch,
        }
    }
    async fn admit(&self, sequence: u64) {
        admit(&self.evidence, &self.epoch, sequence).await;
    }
    async fn initial(&self) -> GenerationStore {
        let (ticket, store) = self
            .coordinator
            .begin_build(manifest("old"), 100)
            .await
            .unwrap();
        self.coordinator
            .publish(&ticket, &store, None, &Worker::plain())
            .await
            .unwrap();
        store
    }
}
fn agent() -> AgentRef {
    AgentRef::Session(SessionId::decode("env7fk2:codex:root").unwrap())
}
fn manifest(name: &str) -> GenerationManifest {
    GenerationManifest {
        generation: name.into(),
        semantic_schema: "trace-1".into(),
        mapper_builds: std::collections::BTreeMap::from([("codex".into(), "mapper-1".into())]),
        reducer_build: "reducer-1".into(),
        capture_inventory_build: "inventory-1".into(),
        encoding_build: "encoding-1".into(),
    }
}
async fn open_evidence(path: &Path) -> EvidenceStore {
    EvidenceStore::open(
        path.join("evidence.db"),
        BlobStore::new(path.join("blobs")),
        AdmissionLimits::default(),
    )
    .await
    .unwrap()
}
async fn admit(evidence: &EvidenceStore, epoch: &IngestionEpoch, sequence: u64) {
    let agent = agent();
    let issuer = Issuer::Environment(agent.environment().clone());
    let bytes = format!("source record {sequence}");
    let payload = evidence.stage_payload(bytes.as_bytes()).await.unwrap();
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "capture".into(),
            producer_epoch: "incarnation".into(),
            producer_seq: sequence,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer: issuer.clone(),
                instance: "source".into(),
            },
            generation: 1,
            locator: SourceLocator::Frame { ordinal: sequence },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: vec![agent],
        captured_at: TimeEvidence {
            domain: "clock".into(),
            value: 100,
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
    };
    evidence.admit_batch(&issuer, &[capture]).await.unwrap();
}

struct Worker {
    calls: AtomicUsize,
    pause_call: Option<usize>,
    entered: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
    reject_validation: bool,
}
impl Worker {
    fn plain() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            pause_call: None,
            entered: Arc::new(tokio::sync::Notify::new()),
            resume: Arc::new(tokio::sync::Notify::new()),
            reject_validation: false,
        }
    }
    fn pausing(call: usize) -> Self {
        Self {
            pause_call: Some(call),
            ..Self::plain()
        }
    }
}
#[async_trait]
impl ProjectionCatchUp for Worker {
    async fn validate_generation(&self, _store: &GenerationStore, _cut: u64) -> anyhow::Result<()> {
        if self.reject_validation {
            anyhow::bail!("retained payload digest is corrupt");
        }
        Ok(())
    }
    async fn catch_up(
        &self,
        store: &GenerationStore,
        token: u64,
        through: u64,
    ) -> anyhow::Result<()> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.pause_call == Some(call) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        loop {
            let checkpoint = store.checkpoint().await?;
            let cut = checkpoint.snapshot.evidence_cut;
            if cut >= through {
                break;
            }
            let agent = agent();
            let object = TraceObject::Agent(AgentThread {
                vendor: agent.anchor_session().agent.clone(),
                agent_ref: agent,
                name: Some(format!("cut {}", cut + 1)),
                model: None,
            });
            store
                .apply(
                    token,
                    cut,
                    &ProjectionBatch {
                        through_cut: cut + 1,
                        objects: vec![ObjectUpdate::put(object, format!("cut {}", cut + 1))],
                        ..Default::default()
                    },
                )
                .await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn publication_catches_a_concurrently_advanced_old_writer_and_replays_the_later_admission_tail(
) {
    let fixture = Fixture::new(PublicationLimits::default()).await;
    fixture.admit(1).await;
    let old = fixture.initial().await;
    let old_snapshot = old.checkpoint().await.unwrap().snapshot;
    let pending = fixture
        .catalogue
        .begin_lease("reader", 100, 1000)
        .await
        .unwrap();
    let lease = fixture
        .catalogue
        .finish_lease(&pending, &old_snapshot, 101)
        .await
        .unwrap();
    let (ticket, private) = fixture
        .coordinator
        .begin_build(manifest("new"), 102)
        .await
        .unwrap();
    let worker = Arc::new(Worker::pausing(1));
    let task = {
        let coordinator = fixture.coordinator.clone();
        let ticket = ticket.clone();
        let private = private.clone();
        let old = old.clone();
        let worker = worker.clone();
        tokio::spawn(async move {
            coordinator
                .publish(&ticket, &private, Some(&old), worker.as_ref())
                .await
                .unwrap()
        })
    };
    worker.entered.notified().await;
    fixture.admit(2).await;
    let token = old.checkpoint().await.unwrap().fence_token;
    Worker::plain().catch_up(&old, token, 2).await.unwrap();
    worker.resume.notify_one();
    let published = task.await.unwrap();
    assert_eq!(published.ready.snapshot.evidence_cut, 2);
    assert_eq!(
        old.checkpoint().await.unwrap().writer_mode,
        WriterMode::Fenced
    );
    assert_eq!(
        fixture
            .catalogue
            .resolve_lease("reader", &lease.token, 103)
            .await
            .unwrap()
            .snapshot,
        old_snapshot
    );
    assert_eq!(
        old.summary(&old_snapshot, &AnyObjectRef::Agent(fixture.agent.clone()))
            .await
            .unwrap()
            .preview,
        "cut 1"
    );
    fixture.admit(3).await;
    let token = private.checkpoint().await.unwrap().fence_token;
    Worker::plain().catch_up(&private, token, 3).await.unwrap();
    assert_eq!(private.checkpoint().await.unwrap().snapshot.evidence_cut, 3);
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), 3);
}

#[tokio::test]
async fn barrier_timeout_resumes_old_projection_without_blocking_evidence_and_a_retry_publishes() {
    let mut fixture = Fixture::new(PublicationLimits::default()).await;
    fixture.admit(1).await;
    let old = fixture.initial().await;
    fixture.coordinator.limits.barrier_budget = Duration::from_millis(300);
    let (ticket, private) = fixture
        .coordinator
        .begin_build(manifest("retry"), 100)
        .await
        .unwrap();
    let worker = Arc::new(Worker::pausing(2));
    let task = {
        let coordinator = fixture.coordinator.clone();
        let ticket = ticket.clone();
        let private = private.clone();
        let old = old.clone();
        let worker = worker.clone();
        tokio::spawn(async move {
            coordinator
                .publish(&ticket, &private, Some(&old), worker.as_ref())
                .await
        })
    };
    worker.entered.notified().await;
    assert_eq!(
        old.checkpoint().await.unwrap().writer_mode,
        WriterMode::Fenced
    );
    fixture.admit(2).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(PublicationError::BarrierExpired)
    ));
    assert_eq!(
        old.checkpoint().await.unwrap().writer_mode,
        WriterMode::Running
    );
    assert_eq!(
        fixture
            .catalogue
            .active_generation()
            .await
            .unwrap()
            .unwrap()
            .ready
            .manifest
            .generation,
        "old"
    );
    fixture.coordinator.limits.barrier_budget = PublicationLimits::default().barrier_budget;
    let published = fixture
        .coordinator
        .publish(&ticket, &private, Some(&old), &Worker::plain())
        .await
        .unwrap();
    assert_eq!(published.ready.snapshot.evidence_cut, 2);
}

#[tokio::test]
async fn a_second_coordinator_cannot_resume_a_generation_fenced_by_another_live_publisher() {
    let fixture = Fixture::new(PublicationLimits::default()).await;
    let old = fixture.initial().await;
    let (ticket, private) = fixture
        .coordinator
        .begin_build(manifest("first"), 100)
        .await
        .unwrap();
    let (other_ticket, other_private) = fixture
        .coordinator
        .begin_build(manifest("second"), 100)
        .await
        .unwrap();
    let worker = Arc::new(Worker::pausing(2));
    let task = {
        let coordinator = fixture.coordinator.clone();
        let ticket = ticket.clone();
        let private = private.clone();
        let old = old.clone();
        let worker = worker.clone();
        tokio::spawn(async move {
            coordinator
                .publish(&ticket, &private, Some(&old), worker.as_ref())
                .await
                .unwrap()
        })
    };
    worker.entered.notified().await;
    let before = old.checkpoint().await.unwrap();
    let other = PublicationCoordinator::new(
        fixture.catalogue.clone(),
        fixture.dir.path().join("projections"),
        PublicationLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        other
            .publish(&other_ticket, &other_private, Some(&old), &Worker::plain())
            .await,
        Err(PublicationError::Catalogue(CatalogueError::PublicationBusy))
    ));
    assert_eq!(old.checkpoint().await.unwrap(), before);
    worker.resume.notify_one();
    task.await.unwrap();
}

#[tokio::test]
async fn missing_published_generation_is_an_explicit_recovery_failure() {
    let fixture = Fixture::new(PublicationLimits::default()).await;
    let old = fixture.initial().await;
    let path = old.path().to_path_buf();
    old.close().await;
    tokio::fs::remove_file(path).await.unwrap();
    assert!(fixture
        .coordinator
        .recover_active(&Worker::plain())
        .await
        .is_err());
    assert_eq!(
        fixture.catalogue.pointer().await.unwrap().generation,
        Some("old".into())
    );
}

#[tokio::test]
async fn failed_payload_validation_leaves_the_old_generation_running_and_authoritative() {
    let fixture = Fixture::new(PublicationLimits::default()).await;
    let old = fixture.initial().await;
    let (ticket, private) = fixture
        .coordinator
        .begin_build(manifest("corrupt-payload"), 100)
        .await
        .unwrap();
    let worker = Worker {
        reject_validation: true,
        ..Worker::plain()
    };
    assert!(matches!(
        fixture
            .coordinator
            .publish(&ticket, &private, Some(&old), &worker)
            .await,
        Err(PublicationError::Worker(_))
    ));
    assert_eq!(
        old.checkpoint().await.unwrap().writer_mode,
        WriterMode::Running
    );
    assert_eq!(
        fixture.catalogue.pointer().await.unwrap().generation,
        Some("old".into())
    );
    assert!(fixture.coordinator.recover_active(&worker).await.is_err());
}

#[tokio::test]
async fn a_missing_tail_validation_proof_cannot_be_sealed_as_a_complete_generation() {
    let fixture = Fixture::new(PublicationLimits::default()).await;
    let (_, store) = fixture
        .coordinator
        .begin_build(manifest("tail"), 100)
        .await
        .unwrap();
    let baseline = store.validate_for_publication().await.unwrap();
    Worker::plain().catch_up(&store, 1, 2).await.unwrap();
    let fence = store.fence(1).await.unwrap();
    assert!(matches!(
        store.validate_tail(&baseline, &fence, 1).await,
        Err(GenerationError::Limit(_))
    ));
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", store.path().display()))
        .await
        .unwrap();
    sqlx::query("DELETE FROM projection_commit_proofs WHERE view_revision=1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.validate_tail(&baseline, &fence, 2).await.is_err());
    pool.close().await;
}

#[cfg(unix)]
#[test]
fn publication_kill_probe() {
    let Some(root) = std::env::var_os("SYLPH_PUBLICATION_CRASH_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let evidence = open_evidence(&root).await;
        let catalogue = EvidenceCatalogue::new(evidence, CatalogueLimits::default()).unwrap();
        let mut coordinator = PublicationCoordinator::new(
            catalogue,
            root.join("projections"),
            PublicationLimits::default(),
        )
        .unwrap();
        let old = coordinator
            .recover_active(&Worker::plain())
            .await
            .unwrap()
            .unwrap();
        let (ticket, private) = coordinator
            .begin_build(manifest("crash-new"), 100)
            .await
            .unwrap();
        coordinator.crash = Some(
            match std::env::var("SYLPH_PUBLICATION_CRASH_POINT")
                .unwrap()
                .as_str()
            {
                "ready" => PublicationCrash::AfterReady,
                "pointer" => PublicationCrash::AfterPointer,
                _ => panic!("bad crash point"),
            },
        );
        coordinator
            .publish(&ticket, &private, Some(&old), &Worker::plain())
            .await
            .unwrap();
        panic!("crash probe survived");
    });
}

#[cfg(unix)]
#[tokio::test]
async fn sigkill_before_and_after_pointer_commit_recovers_only_the_catalogue_generation() {
    use std::os::unix::process::ExitStatusExt;
    for point in ["ready", "pointer"] {
        let fixture = Fixture::new(PublicationLimits::default()).await;
        fixture.admit(1).await;
        let old = fixture.initial().await;
        old.close().await;
        fixture.evidence.close().await;
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "trace::publication::tests::publication_kill_probe",
                "--exact",
                "--nocapture",
            ])
            .env("SYLPH_PUBLICATION_CRASH_ROOT", fixture.dir.path())
            .env("SYLPH_PUBLICATION_CRASH_POINT", point)
            .env("SYLPH_PUBLICATION_CRASH_PROCESS", "1")
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(9),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let evidence = open_evidence(fixture.dir.path()).await;
        let catalogue =
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
        let coordinator = PublicationCoordinator::new(
            catalogue.clone(),
            fixture.dir.path().join("projections"),
            PublicationLimits::default(),
        )
        .unwrap();
        let recovered = coordinator
            .recover_active(&Worker::plain())
            .await
            .unwrap()
            .unwrap();
        let expected = if point == "ready" { "old" } else { "crash-new" };
        assert_eq!(recovered.manifest().generation, expected);
        assert_eq!(
            recovered.checkpoint().await.unwrap().writer_mode,
            WriterMode::Running
        );
        assert_eq!(
            recovered.checkpoint().await.unwrap().snapshot.evidence_cut,
            1
        );
        let scope = IngestionScope::Agent(agent());
        let epoch = evidence.current_scope_epoch(&scope).await.unwrap().unwrap();
        admit(&evidence, &epoch, 2).await;
        let token = recovered.checkpoint().await.unwrap().fence_token;
        Worker::plain()
            .catch_up(&recovered, token, 2)
            .await
            .unwrap();
        assert_eq!(
            recovered.checkpoint().await.unwrap().snapshot.evidence_cut,
            2
        );
        assert_eq!(
            catalogue
                .active_generation()
                .await
                .unwrap()
                .unwrap()
                .ready
                .manifest
                .generation,
            expected
        );
    }
}
