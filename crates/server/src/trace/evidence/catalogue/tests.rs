use super::*;
use crate::{trace::cas::BlobStore, trace::evidence::AdmissionLimits};
use sylph_protocol::{
    trace::{Capture, SourceLocator, SourcePosition, SourceRef, TimeBasis, TimeEvidence, TimeUnit},
    SessionId,
};

struct Fixture {
    _dir: tempfile::TempDir,
    evidence: EvidenceStore,
    catalogue: EvidenceCatalogue,
    scope: IngestionScope,
    epoch: IngestionEpoch,
    agent: AgentRef,
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
        let epoch = evidence
            .authorize_scope(&scope, std::slice::from_ref(&agent))
            .await
            .unwrap();
        let catalogue =
            EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default()).unwrap();
        Self {
            _dir: dir,
            evidence,
            catalogue,
            scope,
            epoch,
            agent,
        }
    }
    fn proof(&self) -> ScopeProof {
        ScopeProof {
            scope: self.scope.clone(),
            epoch: self.epoch.clone(),
        }
    }
    async fn ticket(&self, name: &str) -> BuildTicket {
        self.catalogue
            .begin_build(manifest(name), 100)
            .await
            .unwrap()
    }
    async fn ready(&self, ticket: &BuildTicket, cut: u64) -> ReadyManifest {
        self.catalogue
            .register_file(ticket, "durable-file-identity")
            .await
            .unwrap();
        let ready = ReadyManifest {
            manifest: ticket.manifest.clone(),
            relative_file: ticket.relative_file.clone(),
            database_identity: "durable-file-identity".into(),
            snapshot: Snapshot {
                generation: ticket.manifest.generation.clone(),
                evidence_cut: cut,
                view_revision: cut,
            },
            fence_token: 2,
        };
        self.catalogue.record_ready(ticket, &ready).await.unwrap();
        ready
    }
    async fn publish(&self, name: &str) -> PublishedGeneration {
        let ticket = self.ticket(name).await;
        self.catalogue
            .pin_generation_scope(name, &self.proof(), 0)
            .await
            .unwrap();
        self.ready(&ticket, 0).await;
        let guard = self.catalogue.acquire_publication(&ticket).await.unwrap();
        let published = self.catalogue.publish(&ticket, &guard, 0).await.unwrap();
        self.catalogue.release_publication(&guard).await.unwrap();
        published
    }
    async fn capture(
        &self,
        epoch: &IngestionEpoch,
        issuer: Issuer,
        position: (&str, u64),
        raw: &[u8],
        subjects: Vec<AgentRef>,
        transport_kind: TransportKind,
    ) -> ObservationRef {
        let (producer, sequence) = position;
        let payload = self.evidence.stage_payload(raw).await.unwrap();
        let observation = ObservationRef {
            issuer: issuer.clone(),
            producer_id: producer.into(),
            producer_epoch: "incarnation".into(),
            producer_seq: sequence,
        };
        let capture = Capture {
            observation_ref: observation.clone(),
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: producer.into(),
                },
                generation: 1,
                locator: SourceLocator::Frame { ordinal: sequence },
            },
            ingestion_authorization_epoch: epoch.clone(),
            declared_subject_refs: subjects,
            captured_at: TimeEvidence {
                domain: "clock".into(),
                value: 100,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: payload,
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        self.evidence
            .admit_batch(&issuer, &[capture])
            .await
            .unwrap();
        observation
    }
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

#[tokio::test]
async fn only_a_sealed_ready_manifest_can_replace_the_expected_active_pointer() {
    let fixture = Fixture::new().await;
    let old = fixture.publish("old").await;
    let first = fixture.ticket("new-a").await;
    let competing = fixture.ticket("new-b").await;
    let guard = fixture.catalogue.acquire_publication(&first).await.unwrap();
    assert!(matches!(
        fixture.catalogue.publish(&first, &guard, 0).await,
        Err(CatalogueError::NotReady)
    ));
    assert_eq!(fixture.catalogue.pointer().await.unwrap(), old.pointer);
    assert!(matches!(
        fixture.catalogue.acquire_publication(&competing).await,
        Err(CatalogueError::PublicationBusy)
    ));
    fixture.ready(&first, 0).await;
    let next = fixture.catalogue.publish(&first, &guard, 0).await.unwrap();
    fixture.catalogue.release_publication(&guard).await.unwrap();
    fixture.ready(&competing, 0).await;
    let stale = fixture
        .catalogue
        .acquire_publication(&competing)
        .await
        .unwrap();
    assert!(matches!(
        fixture.catalogue.publish(&competing, &stale, 0).await,
        Err(CatalogueError::PublicationConflict)
    ));
    fixture.catalogue.release_publication(&stale).await.unwrap();
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
        "new-a"
    );
    assert_eq!(next.pointer.serial, old.pointer.serial + 1);
}

#[tokio::test]
async fn ready_retries_append_immutable_seals_and_publication_never_regresses_the_cut() {
    let fixture = Fixture::new().await;
    let observation = fixture
        .capture(
            &fixture.epoch,
            Issuer::Environment(fixture.agent.environment().clone()),
            ("native", 1),
            b"one",
            vec![fixture.agent.clone()],
            TransportKind::NativeJsonl,
        )
        .await;
    let ticket = fixture.ticket("candidate").await;
    let ready = fixture.ready(&ticket, 1).await;
    let guard = fixture
        .catalogue
        .acquire_publication(&ticket)
        .await
        .unwrap();
    assert!(fixture.catalogue.publish(&ticket, &guard, 2).await.is_err());
    fixture.catalogue.release_publication(&guard).await.unwrap();
    fixture
        .capture(
            &fixture.epoch,
            observation.issuer,
            ("native", 2),
            b"two",
            vec![fixture.agent.clone()],
            TransportKind::NativeJsonl,
        )
        .await;
    let mut second = ready.clone();
    second.snapshot.evidence_cut = 2;
    second.snapshot.view_revision = 2;
    second.fence_token = 4;
    fixture
        .catalogue
        .record_ready(&ticket, &second)
        .await
        .unwrap();
    let saved: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trace_ready_manifests WHERE generation='candidate'",
    )
    .fetch_one(&fixture.evidence.readers)
    .await
    .unwrap();
    assert_eq!(saved, 2);
    let first_json:String=sqlx::query_scalar("SELECT ready_json FROM trace_ready_manifests WHERE generation='candidate' AND sealed_cut=1").fetch_one(&fixture.evidence.readers).await.unwrap();
    assert_eq!(
        serde_json::from_str::<ReadyManifest>(&first_json).unwrap(),
        ready
    );
    let guard = fixture
        .catalogue
        .acquire_publication(&ticket)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalogue
            .publish(&ticket, &guard, 2)
            .await
            .unwrap()
            .ready
            .snapshot
            .evidence_cut,
        2
    );
}

#[tokio::test]
async fn a_reusable_lease_keeps_its_generation_and_subject_after_publication_changes() {
    let fixture = Fixture::new().await;
    let old = fixture.publish("old").await;
    let pending = fixture
        .catalogue
        .begin_lease("alice", 100, 1000)
        .await
        .unwrap();
    assert!(fixture
        .catalogue
        .generation_is_pinned("old", 101)
        .await
        .unwrap());
    fixture.publish("new").await;
    let lease = fixture
        .catalogue
        .finish_lease(&pending, &old.ready.snapshot, 102)
        .await
        .unwrap();
    fixture
        .catalogue
        .bind_lease_scope("alice", &lease.token, &fixture.proof(), 103)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalogue
            .resolve_lease("alice", &lease.token, 104)
            .await
            .unwrap(),
        lease
    );
    assert_eq!(lease.snapshot.generation, "old");
    assert!(matches!(
        fixture
            .catalogue
            .resolve_lease("bob", &lease.token, 104)
            .await,
        Err(CatalogueError::SubjectMismatch)
    ));
    assert!(matches!(
        fixture
            .catalogue
            .resolve_lease("alice", &lease.token, 1100)
            .await,
        Err(CatalogueError::SnapshotExpired)
    ));
    assert_eq!(
        fixture
            .catalogue
            .minimum_leased_revision("old", 104)
            .await
            .unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn deletion_invalidates_bound_leases_and_builds_without_reauthorizing_old_snapshots() {
    let fixture = Fixture::new().await;
    let published = fixture.publish("active").await;
    let pending = fixture
        .catalogue
        .begin_lease("alice", 100, 1000)
        .await
        .unwrap();
    let lease = fixture
        .catalogue
        .finish_lease(&pending, &published.ready.snapshot, 101)
        .await
        .unwrap();
    fixture
        .catalogue
        .bind_lease_scope("alice", &lease.token, &fixture.proof(), 102)
        .await
        .unwrap();
    let unused = fixture
        .catalogue
        .begin_lease("bob", 100, 1000)
        .await
        .unwrap();
    let unused = fixture
        .catalogue
        .finish_lease(&unused, &published.ready.snapshot, 101)
        .await
        .unwrap();
    let building = fixture.ticket("building").await;
    fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    assert!(matches!(
        fixture
            .catalogue
            .resolve_lease("alice", &lease.token, 103)
            .await,
        Err(CatalogueError::DeletedScope)
    ));
    assert!(matches!(
        fixture.catalogue.acquire_publication(&building).await,
        Err(CatalogueError::DeletedScope)
    ));
    let fresh = fixture
        .evidence
        .readopt_scope(&fixture.scope, std::slice::from_ref(&fixture.agent))
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .catalogue
            .bind_lease_scope(
                "bob",
                &unused.token,
                &ScopeProof {
                    scope: fixture.scope.clone(),
                    epoch: fresh
                },
                104
            )
            .await,
        Err(CatalogueError::DeletedScope)
    ));
    assert!(matches!(
        fixture
            .catalogue
            .bind_lease_scope("bob", &unused.token, &fixture.proof(), 104)
            .await,
        Err(CatalogueError::DeletedScope)
    ));
}

#[tokio::test]
async fn deletion_racing_pending_lease_finalization_cannot_publish_an_old_view() {
    let fixture = Fixture::new().await;
    let published = fixture.publish("active").await;
    let pending = fixture
        .catalogue
        .begin_lease("alice", 100, 1000)
        .await
        .unwrap();
    fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    assert!(matches!(
        fixture
            .catalogue
            .finish_lease(&pending, &published.ready.snapshot, 101)
            .await,
        Err(CatalogueError::DeletedScope)
    ));
    assert!(fixture
        .catalogue
        .generation_is_pinned("active", 101)
        .await
        .unwrap());
}

#[tokio::test]
async fn pending_uploads_pin_exact_physical_bytes_before_payload_publication() {
    let fixture = Fixture::new().await;
    let bytes = b"immutable bytes";
    let payload = PayloadRef::bytes(blake3::hash(bytes).to_hex()).unwrap();
    let upload = fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Generation,
            "building",
            payload.clone(),
            fixture.epoch.clone(),
            100,
        )
        .await
        .unwrap();
    assert!(!fixture
        .catalogue
        .reserve_collection(payload.digest())
        .await
        .unwrap());
    assert!(matches!(
        fixture.catalogue.finish_upload(&upload).await,
        Err(CatalogueError::PayloadMissing)
    ));
    fixture.evidence.stage_payload(bytes).await.unwrap();
    fixture.catalogue.finish_upload(&upload).await.unwrap();
    assert!(!fixture
        .catalogue
        .reserve_collection(payload.digest())
        .await
        .unwrap());
    let another = PayloadRef::manifest(payload.digest()).unwrap();
    let reservation = fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "job",
            another,
            fixture.epoch.clone(),
            101,
        )
        .await
        .unwrap();
    assert_eq!(reservation.payload.digest(), payload.digest());
    assert!(!fixture
        .catalogue
        .reserve_collection(payload.digest())
        .await
        .unwrap());
    let absent = PayloadRef::bytes("f".repeat(64)).unwrap();
    assert!(fixture
        .catalogue
        .reserve_collection(absent.digest())
        .await
        .unwrap());
    assert!(matches!(
        fixture
            .catalogue
            .begin_upload(
                PinOwnerKind::Export,
                "other",
                absent.clone(),
                fixture.epoch.clone(),
                102
            )
            .await,
        Err(CatalogueError::PayloadDeleting)
    ));
    fixture
        .catalogue
        .release_collection(absent.digest())
        .await
        .unwrap();
    fixture
        .catalogue
        .begin_upload(
            PinOwnerKind::Export,
            "other",
            absent,
            fixture.epoch.clone(),
            103,
        )
        .await
        .unwrap();
    fixture.catalogue.cancel_upload(&reservation).await.unwrap();
    assert_eq!(
        fixture
            .catalogue
            .release_owner_pins(PinOwnerKind::Generation, "building", 1)
            .await
            .unwrap(),
        1
    );
    assert!(fixture
        .catalogue
        .reserve_collection(payload.digest())
        .await
        .unwrap());
}

#[tokio::test]
async fn pinned_payload_validation_detects_corrupt_bytes_before_generation_publication() {
    let fixture = Fixture::new().await;
    let bytes = b"exact retained bytes";
    let payload = fixture.evidence.stage_payload(bytes).await.unwrap();
    let pin = PayloadPin {
        payload: payload.clone(),
        epoch: fixture.epoch.clone(),
    };
    fixture
        .catalogue
        .pin_payloads(
            PinOwnerKind::Generation,
            "generation",
            std::slice::from_ref(&pin),
        )
        .await
        .unwrap();
    let pins = fixture
        .catalogue
        .payload_pins(PinOwnerKind::Generation, "generation", None, 1)
        .await
        .unwrap();
    assert_eq!(pins[0].payload, payload);
    fixture.catalogue.verify_payload(&pins[0]).await.unwrap();
    let digest = payload.digest();
    let path = fixture
        ._dir
        .path()
        .join("blobs")
        .join(&digest[..2])
        .join(&digest[2..4])
        .join(digest);
    tokio::fs::write(path, b"corrupted").await.unwrap();
    assert!(fixture.catalogue.verify_payload(&pin).await.is_err());
    assert!(fixture
        .catalogue
        .payload_pins(
            PinOwnerKind::Generation,
            "generation",
            Some((&payload, &fixture.epoch)),
            1
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_late_retained_identity_binding_keeps_the_original_epoch_deleted_after_readoption() {
    let fixture = Fixture::new().await;
    let inventory = sylph_protocol::trace::InventoryRef {
        environment: fixture.agent.environment().clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        instance: "launch-before-native-id".into(),
    };
    let scope = IngestionScope::Inventory(inventory);
    let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
    let source = fixture
        .capture(
            &epoch,
            Issuer::Environment(fixture.agent.environment().clone()),
            ("launch-source", 1),
            b"native identity",
            vec![],
            TransportKind::NativeJsonl,
        )
        .await;
    let fact = OperationalFact::IdentityBound {
        command_id: "launch".into(),
        agent_ref: fixture.agent.clone(),
        source_evidence: vec![source],
    };
    let binding = fixture
        .capture(
            &epoch,
            Issuer::Server("server".into()),
            ("identity-binding", 1),
            &serde_json::to_vec(&fact).unwrap(),
            vec![],
            TransportKind::Control,
        )
        .await;
    let tombstone = fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    fixture
        .evidence
        .readopt_scope(&fixture.scope, std::slice::from_ref(&fixture.agent))
        .await
        .unwrap();
    let result = fixture
        .catalogue
        .bind_inventory_agent(
            &ScopeProof {
                scope: scope.clone(),
                epoch: epoch.clone(),
            },
            &fixture.agent,
            &binding,
            200,
        )
        .await
        .unwrap();
    assert_eq!(result, Some(tombstone));
    assert!(fixture
        .evidence
        .capture_payload(&binding, 1024 * 1024)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        fixture
            .catalogue
            .pin_generation_scope("missing", &ScopeProof { scope, epoch }, 1)
            .await,
        Err(CatalogueError::DeletedScope)
    ));
}

#[tokio::test]
async fn an_inventory_opened_during_suppression_stays_deleted_when_its_identity_arrives_after_readoption(
) {
    let fixture = Fixture::new().await;
    let tombstone = fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    let scope = IngestionScope::Inventory(sylph_protocol::trace::InventoryRef {
        environment: fixture.agent.environment().clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        instance: "opened-during-suppression".into(),
    });
    let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
    let source = fixture
        .capture(
            &epoch,
            Issuer::Environment(fixture.agent.environment().clone()),
            ("late-native-source", 1),
            b"native identity",
            vec![],
            TransportKind::NativeJsonl,
        )
        .await;
    let fact = OperationalFact::IdentityBound {
        command_id: "late-launch".into(),
        agent_ref: fixture.agent.clone(),
        source_evidence: vec![source],
    };
    let binding = fixture
        .capture(
            &epoch,
            Issuer::Server("server".into()),
            ("late-binding", 1),
            &serde_json::to_vec(&fact).unwrap(),
            vec![],
            TransportKind::Control,
        )
        .await;
    fixture
        .evidence
        .readopt_scope(&fixture.scope, std::slice::from_ref(&fixture.agent))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .catalogue
            .bind_inventory_agent(
                &ScopeProof {
                    scope: scope.clone(),
                    epoch: epoch.clone()
                },
                &fixture.agent,
                &binding,
                300
            )
            .await
            .unwrap(),
        Some(tombstone)
    );
    assert!(fixture
        .evidence
        .capture_payload(&binding, 1024 * 1024)
        .await
        .unwrap()
        .is_none());
    let current = fixture.evidence.current_scope_epoch(&scope).await.unwrap();
    assert!(current.is_none());
}

#[tokio::test]
async fn a_server_control_receipt_cannot_substitute_for_native_identity_evidence() {
    let fixture = Fixture::new().await;
    let scope = IngestionScope::Inventory(sylph_protocol::trace::InventoryRef {
        environment: fixture.agent.environment().clone(),
        vendor: fixture.agent.anchor_session().agent.clone(),
        instance: "invalid-native-proof".into(),
    });
    let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
    let claimed_source = fixture
        .capture(
            &epoch,
            Issuer::Server("server".into()),
            ("server-not-native", 1),
            b"a server receipt",
            vec![],
            TransportKind::Control,
        )
        .await;
    let fact = OperationalFact::IdentityBound {
        command_id: "launch".into(),
        agent_ref: fixture.agent.clone(),
        source_evidence: vec![claimed_source],
    };
    let binding = fixture
        .capture(
            &epoch,
            Issuer::Server("server".into()),
            ("invalid-binding", 1),
            &serde_json::to_vec(&fact).unwrap(),
            vec![],
            TransportKind::Control,
        )
        .await;
    assert!(fixture
        .catalogue
        .bind_inventory_agent(&ScopeProof { scope, epoch }, &fixture.agent, &binding, 200)
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ingestion_scope_bindings")
        .fetch_one(&fixture.evidence.readers)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn deleted_abandoned_derived_uploads_do_not_exhaust_the_next_epochs_capacity() {
    let fixture = Fixture::new().await;
    let catalogue = EvidenceCatalogue::new(
        fixture.evidence.clone(),
        CatalogueLimits {
            max_pending_uploads: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let old = catalogue
        .begin_upload(
            PinOwnerKind::Generation,
            "old",
            PayloadRef::bytes("1".repeat(64)).unwrap(),
            fixture.epoch.clone(),
            1,
        )
        .await
        .unwrap();
    fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    let fresh = fixture
        .evidence
        .readopt_scope(&fixture.scope, std::slice::from_ref(&fixture.agent))
        .await
        .unwrap();
    catalogue
        .begin_upload(
            PinOwnerKind::Generation,
            "fresh",
            PayloadRef::bytes("2".repeat(64)).unwrap(),
            fresh,
            2,
        )
        .await
        .unwrap();
    assert!(matches!(
        catalogue.finish_upload(&old).await,
        Err(CatalogueError::DeletedScope)
    ));
}

#[tokio::test]
async fn readonly_scope_validation_rejects_deletion_and_a_mismatched_scope() {
    let fixture = Fixture::new().await;
    assert!(fixture
        .catalogue
        .scope_is_readable(&fixture.proof())
        .await
        .unwrap());
    let wrong = ScopeProof {
        scope: IngestionScope::Agent(AgentRef::Session(
            SessionId::decode("env7fk2:codex:other").unwrap(),
        )),
        epoch: fixture.epoch.clone(),
    };
    assert!(!fixture.catalogue.scope_is_readable(&wrong).await.unwrap());
    fixture.evidence.delete_scope(&fixture.scope).await.unwrap();
    assert!(!fixture
        .catalogue
        .scope_is_readable(&fixture.proof())
        .await
        .unwrap());
}
