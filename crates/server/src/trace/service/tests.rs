use super::*;
use sylph_protocol::{AgentName, EnvironmentId, SessionId};

const NATIVE: &[u8] =
    include_bytes!("../../../../../fixtures/native/codex/user-input-response-item.jsonl");

async fn admit_native(service: &TraceService) {
    let environment = EnvironmentId::parse("env7fk2").unwrap();
    let issuer = Issuer::Environment(environment.clone());
    let scope = IngestionScope::Inventory(InventoryRef {
        environment,
        vendor: AgentName::parse("codex").unwrap(),
        instance: "recovery-fixture".into(),
    });
    let epoch = service.evidence.authorize_scope(&scope, &[]).await.unwrap();
    let mut offset = 0;
    for (index, bytes) in NATIVE.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "native".into(),
                producer_epoch: "recovery".into(),
                producer_seq: index as u64 + 1,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: "native-file".into(),
                },
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + bytes.len() as u64,
                    record_ordinal: index as u64,
                },
            },
            ingestion_authorization_epoch: epoch.clone(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "fixture.wall".into(),
                value: 100,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: Some("fixture".into()),
            schema_fingerprint: None,
            payload_ref: service.evidence.stage_payload(bytes).await.unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        service
            .evidence
            .admit_batch(&issuer, &[capture])
            .await
            .unwrap();
        offset += bytes.len() as u64;
    }
}

async fn close(service: TraceService) {
    for generation in service.generations.read().await.values() {
        generation.close().await;
    }
    service.evidence.close().await;
}

async fn recover_damage(corrupt: bool) {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    admit_native(&service).await;
    service.project_pending().await.unwrap();
    let original = service
        .catalogue
        .active_generation()
        .await
        .unwrap()
        .unwrap();
    let old_view = service.lease_guard("reader", None).await.unwrap();
    let committed_cut = service
        .active()
        .await
        .unwrap()
        .checkpoint()
        .await
        .unwrap()
        .snapshot
        .evidence_cut;
    assert!(committed_cut > original.ready.snapshot.evidence_cut);
    let mut scratch_manifest = original.ready.manifest.clone();
    scratch_manifest.generation = "unpublished-scratch".into();
    let (scratch_ticket, scratch) = service
        .publication
        .begin_build(scratch_manifest, 1)
        .await
        .unwrap();
    scratch.close().await;
    close(service).await;
    let file = directory.path().join(&original.ready.relative_file);
    if corrupt {
        std::fs::write(&file, b"this is not a SQLite database").unwrap();
    } else {
        std::fs::remove_file(&file).unwrap();
    }
    let recovered = TraceService::open(directory.path()).await.unwrap();
    assert!(matches!(
        recovered
            .active()
            .await
            .err()
            .expect("damaged projection must be unavailable")
            .downcast_ref::<TraceServiceError>(),
        Some(TraceServiceError::Recovering(_))
    ));
    assert!(recovered.lease_guard("reader", None).await.is_err());
    assert!(recovered.progress().await.fault.is_some());
    assert_eq!(
        recovered.catalogue.pointer().await.unwrap(),
        original.pointer
    );
    assert!(recovered
        .catalogue
        .pending_builds()
        .await
        .unwrap()
        .is_empty());
    assert!(directory.path().join(scratch_ticket.relative_file).exists());
    recovered.project_pending().await.unwrap();
    let published = recovered
        .catalogue
        .active_generation()
        .await
        .unwrap()
        .unwrap();
    assert_ne!(published.pointer.generation, original.pointer.generation);
    assert_ne!(
        published.pointer.generation.as_deref(),
        Some("unpublished-scratch")
    );
    assert!(published.ready.snapshot.evidence_cut >= committed_cut);
    let active = recovered.active().await.unwrap();
    let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:response-user").unwrap());
    active
        .summary(
            &active.checkpoint().await.unwrap().snapshot,
            &AnyObjectRef::Agent(agent),
        )
        .await
        .unwrap();
    assert!(recovered.progress().await.fault.is_none());
    assert!(matches!(
        recovered
            .lease_guard("reader", Some(&old_view.token))
            .await
            .expect_err("lost generation invalidates its old view")
            .downcast_ref::<super::super::evidence::catalogue::CatalogueError>(),
        Some(super::super::evidence::catalogue::CatalogueError::SnapshotExpired)
    ));
    close(recovered).await;
}

#[tokio::test]
async fn missing_published_projection_rebuilds_the_committed_tail_without_selecting_scratch() {
    recover_damage(false).await;
}

#[tokio::test]
async fn corrupt_published_projection_reports_recovery_until_evidence_is_republished() {
    recover_damage(true).await;
}

#[tokio::test]
async fn a_second_server_cannot_reset_live_archive_recovery_guards() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let error = TraceService::open(directory.path())
        .await
        .err()
        .expect("second server must fail");
    assert!(error.to_string().contains("already open by another server"));
    close(service).await;
    close(TraceService::open(directory.path()).await.unwrap()).await;
}

#[tokio::test]
async fn search_query_admission_uses_the_configured_concurrency_bound() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let mut permits = Vec::new();
    for _ in 0..service.search_limits.max_concurrent_queries {
        permits.push(service.try_search_permit().unwrap());
    }
    assert!(matches!(
        service.try_search_permit(),
        Err(SearchError::Limit("concurrent search queries"))
    ));
    drop(permits.pop());
    assert!(service.try_search_permit().is_ok());
    close(service).await;
}

#[tokio::test]
async fn product_background_collection_reclaims_unrooted_payloads_and_releases_the_service() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(TraceService::open(directory.path()).await.unwrap());
    let payload = service
        .evidence
        .stage_payload(b"orphaned fixture bytes")
        .await
        .unwrap();
    assert!(service
        .evidence
        .payload_range(&payload, 0, 22)
        .await
        .is_ok());
    service.spawn();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if service
                .evidence
                .payload_range(&payload, 0, 22)
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("collector must reclaim an unrooted payload");
    let weak = Arc::downgrade(&service);
    drop(service);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while weak.strong_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background workers must release their owner");
}

#[tokio::test]
async fn search_revision_and_payload_pins_survive_live_views_then_retire_after_invalidation() {
    use crate::trace::evidence::catalogue::PinOwnerKind;
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    admit_native(&service).await;
    service.project_pending().await.unwrap();
    service.index_pending().await.unwrap();
    let lease = service.lease_guard("reader", None).await.unwrap();
    let snapshot = lease.snapshot.clone();
    let index = service.search_store(&snapshot.generation).await.unwrap();
    let now = crate::models::now_ms();
    assert_eq!(index.checkpoint().await.unwrap(), Some(snapshot.clone()));
    assert_eq!(
        service
            .catalogue
            .minimum_leased_revision(&snapshot.generation, now)
            .await
            .unwrap(),
        Some(snapshot.view_revision)
    );
    service.rebuild().await.unwrap();
    service.retire_searches(now).await.unwrap();
    assert_eq!(index.checkpoint().await.unwrap(), Some(snapshot.clone()));
    service
        .catalogue
        .expire_lease("reader", &lease.token)
        .await
        .unwrap();
    assert_eq!(
        service
            .catalogue
            .minimum_leased_revision(&snapshot.generation, now)
            .await
            .unwrap(),
        Some(snapshot.view_revision)
    );
    // The export root is independent of both the search checkpoint and its jobs.
    service
        .catalogue
        .pin_revision(PinOwnerKind::Export, "fixture-export", &snapshot)
        .await
        .unwrap();
    service.retire_searches(now).await.unwrap();
    assert_eq!(index.checkpoint().await.unwrap(), None);
    assert_eq!(
        service
            .catalogue
            .release_owner_pins(PinOwnerKind::Search, &snapshot.generation, 128)
            .await
            .unwrap(),
        0
    );
    assert!(service
        .catalogue
        .generation_is_pinned(&snapshot.generation, now)
        .await
        .unwrap());
    assert_eq!(
        service
            .catalogue
            .minimum_leased_revision(&snapshot.generation, now)
            .await
            .unwrap(),
        Some(snapshot.view_revision)
    );
    service
        .catalogue
        .release_revision_pin(PinOwnerKind::Export, "fixture-export")
        .await
        .unwrap();
    assert!(!service
        .catalogue
        .generation_is_pinned(&snapshot.generation, now)
        .await
        .unwrap());
    assert_eq!(
        service
            .catalogue
            .minimum_leased_revision(&snapshot.generation, now)
            .await
            .unwrap(),
        None
    );
    close(service).await;
}

#[tokio::test]
async fn failed_new_views_release_capacity_without_expiring_borrowed_or_retained_views() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let view = service.lease_guard("reader", None).await.unwrap();
    let retained = view.token.clone();
    view.retain();
    drop(view);
    drop(
        service
            .lease_guard("reader", Some(&retained))
            .await
            .unwrap(),
    );
    assert!(service
        .catalogue
        .resolve_lease("reader", &retained, crate::models::now_ms())
        .await
        .is_ok());
    for _ in 0..70 {
        let view = service.lease_guard("reader", None).await.unwrap();
        let token = view.token.clone();
        drop(view);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if service
                    .catalogue
                    .resolve_lease("reader", &token, crate::models::now_ms())
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed request must release its view");
    }
    service.release_lease("reader", &retained).await.unwrap();
    assert!(service
        .catalogue
        .resolve_lease("reader", &retained, crate::models::now_ms())
        .await
        .is_err());
    close(service).await;
}
