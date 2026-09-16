use super::*;
use sylph_protocol::{AgentName, EnvironmentId, SessionId};

const NATIVE: &[u8] =
    include_bytes!("../../../../../fixtures/native/codex/user-input-response-item.jsonl");

#[tokio::test]
async fn retained_reader_cursor_and_lease_survive_service_restart() {
    use crate::trace::cursor::CursorBinding;
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    admit_native(&service).await;
    service.project_pending().await.unwrap();
    let lease = service.lease_guard("reader", None).await.unwrap();
    let token = lease.token.clone();
    let snapshot = lease.snapshot.clone();
    let binding = || CursorBinding {
        domain: "sylph.reader.position",
        subject: "reader",
        lease: &token,
        query_digest: "retained-query-and-grants",
    };
    let cursor = service
        .cursors()
        .seal("retained-private-position", binding())
        .unwrap();
    lease.retain();
    drop(lease);
    close(service).await;
    let restarted = TraceService::open(directory.path()).await.unwrap();
    assert_eq!(
        restarted.lease_guard("reader", Some(&token)).await.unwrap().snapshot,
        snapshot
    );
    assert_eq!(
        restarted.cursors().open(&cursor, binding()).unwrap(),
        "retained-private-position"
    );
    close(restarted).await;
}

#[tokio::test]
async fn incompatible_projection_contract_rebuilds_before_serving_retained_views() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    admit_native(&service).await;
    service.project_pending().await.unwrap();
    let mut incompatible = TraceService::manifest("incompatible-projection".into());
    incompatible.semantic_schema = "unrecognized-contract".into();
    let (ticket, private) = service
        .publication
        .begin_build(incompatible, crate::models::now_ms())
        .await
        .unwrap();
    let old = service.active().await.unwrap();
    service
        .publication
        .publish(&ticket, &private, Some(old.as_ref()), &service.worker)
        .await
        .unwrap();
    let snapshot = private.checkpoint().await.unwrap().snapshot;
    private.close().await;
    drop(old);
    let pending = service
        .catalogue
        .begin_lease("reader", crate::models::now_ms(), 120_000)
        .await
        .unwrap();
    let cursor = service
        .catalogue
        .finish_lease(&pending, &snapshot, crate::models::now_ms())
        .await
        .unwrap();
    let token = cursor.token.clone();
    drop(cursor);
    let evidence_cut = service.evidence.evidence_cut().await.unwrap();
    close(service).await;
    let restarted = TraceService::open(directory.path()).await.unwrap();
    assert!(
        restarted.active().await.is_err(),
        "an incompatible projection must not serve old rows"
    );
    assert!(restarted.lease_guard("reader", Some(&token)).await.is_err());
    restarted.project_pending().await.unwrap();
    let active = restarted.active().await.unwrap();
    assert_eq!(
        active.manifest(),
        &TraceService::manifest(active.manifest().generation.clone())
    );
    assert_eq!(restarted.evidence.evidence_cut().await.unwrap(), evidence_cut);
    assert_eq!(
        active.checkpoint().await.unwrap().snapshot.evidence_cut,
        evidence_cut
    );
    drop(active);
    close(restarted).await;
}

struct RetainedContractFixture {
    _directory: tempfile::TempDir,
    service: TraceService,
    manifest: GenerationManifest,
    path: PathBuf,
    snapshot: super::super::generation::Snapshot,
    token: String,
}

impl RetainedContractFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let service = TraceService::open(directory.path()).await.unwrap();
        admit_native(&service).await;
        service.project_pending().await.unwrap();
        let previous = service.active().await.unwrap();
        let mut manifest = TraceService::manifest("retained-previous-contract".into());
        manifest.semantic_schema = "retired-reader-contract".into();
        let (ticket, incompatible) = service
            .publication
            .begin_build(manifest.clone(), crate::models::now_ms())
            .await
            .unwrap();
        service
            .publication
            .publish(&ticket, &incompatible, Some(&previous), &service.worker)
            .await
            .unwrap();
        let snapshot = incompatible.checkpoint().await.unwrap().snapshot;
        let path = incompatible.path().to_owned();
        // The prior server issued this view before a newer contract was published.
        let pending = service
            .catalogue
            .begin_lease("reader", crate::models::now_ms(), 120_000)
            .await
            .unwrap();
        let token = service
            .catalogue
            .finish_lease(&pending, &snapshot, crate::models::now_ms())
            .await
            .unwrap()
            .token;
        let (ticket, current) = service
            .publication
            .begin_build(
                TraceService::manifest("current-contract".into()),
                crate::models::now_ms(),
            )
            .await
            .unwrap();
        service
            .publication
            .publish(&ticket, &current, Some(&incompatible), &service.worker)
            .await
            .unwrap();
        incompatible.close().await;
        current.close().await;
        drop(previous);
        close(service).await;
        let service = TraceService::open(directory.path()).await.unwrap();
        assert_eq!(
            service.active().await.unwrap().manifest().generation,
            "current-contract"
        );
        Self {
            _directory: directory,
            service,
            manifest,
            path,
            snapshot,
            token,
        }
    }

    async fn assert_expired(&self, result: anyhow::Result<()>) {
        assert!(matches!(
            result.as_ref().err().and_then(|error| {
                error.downcast_ref::<super::super::evidence::catalogue::CatalogueError>()
            }),
            Some(super::super::evidence::catalogue::CatalogueError::SnapshotExpired)
        ), "An incompatible retained contract must expire its view: {result:?}");
        assert!(matches!(
            self.service
                .catalogue
                .resolve_lease("reader", &self.token, crate::models::now_ms())
                .await,
            Err(super::super::evidence::catalogue::CatalogueError::SnapshotExpired)
        ));
    }
}

#[tokio::test]
async fn retained_incompatible_contract_cannot_load_from_disk() {
    let fixture = RetainedContractFixture::new().await;
    fixture
        .assert_expired(fixture.service.generation(&fixture.manifest.generation).await.map(|_| ()))
        .await;
    close(fixture.service).await;
}

#[tokio::test]
async fn retained_incompatible_contract_cannot_return_a_cached_store() {
    let fixture = RetainedContractFixture::new().await;
    let cached = GenerationStore::open(
        &fixture.path,
        fixture.manifest.clone(),
        QueryLimits::default(),
    )
    .await
    .unwrap();
    fixture.service.generations.write().await.insert(
        fixture.manifest.generation.clone(),
        Arc::new(cached),
    );
    fixture
        .assert_expired(fixture.service.generation(&fixture.manifest.generation).await.map(|_| ()))
        .await;
    close(fixture.service).await;
}

#[tokio::test]
async fn retained_incompatible_contract_cannot_borrow_an_existing_lease() {
    let fixture = RetainedContractFixture::new().await;
    fixture
        .assert_expired(fixture.service.lease_guard("reader", Some(&fixture.token)).await.map(|_| ()))
        .await;
    close(fixture.service).await;
}

#[tokio::test]
async fn retained_incompatible_contract_cannot_acquire_a_snapshot_lease() {
    let fixture = RetainedContractFixture::new().await;
    fixture
        .assert_expired(fixture.service.lease_at_snapshot("reader", &fixture.snapshot).await.map(|_| ()))
        .await;
    close(fixture.service).await;
}

pub(super) async fn admit_native(service: &TraceService) {
    admit_native_from(service, NATIVE, 0, 0).await;
}

async fn admit_native_from(service: &TraceService, raw: &[u8], ordinal: u64, mut offset: u64) {
    let environment = EnvironmentId::parse("env7fk2").unwrap();
    let issuer = Issuer::Environment(environment.clone());
    let scope = IngestionScope::Inventory(InventoryRef {
        environment,
        vendor: AgentName::parse("codex").unwrap(),
        instance: "recovery-fixture".into(),
    });
    let epoch = service.evidence.authorize_scope(&scope, &[]).await.unwrap();
    for (index, bytes) in raw.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let index = ordinal + index as u64;
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "native".into(),
                producer_epoch: "recovery".into(),
                producer_seq: index + 1,
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
                    record_ordinal: index,
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

async fn search_reuse_pipeline(records: usize) {
    use crate::trace::search::{SearchHistoryMode, SearchQueryContext, SearchScope};
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let mut raw = NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap()
        .to_vec();
    for index in 0..records {
        raw.extend(serde_json::to_vec(&serde_json::json!({
            "type":"response_item", "payload":{"type":"message", "id":format!("search-{index}"),
                "role":"assistant", "content":[{"type":"output_text", "text":format!("retained searchable message {index}")}]}
        })).unwrap());
        raw.push(b'\n');
    }
    admit_native_from(&service, &raw, 0, 0).await;
    service.project_pending().await.unwrap();
    let started = std::time::Instant::now();
    let cold = service.index_pending().await.unwrap();
    let cold_ms = started.elapsed().as_millis();
    assert_eq!(cold.sources_examined, records as u64);
    assert_eq!(cold.sources_reused, 0);
    assert!(cold.payload_bytes_read > 0);
    let unchanged = service.index_pending().await.unwrap();
    assert_eq!(unchanged.sources_examined, 0);
    assert_eq!(unchanged.payload_bytes_read, 0);
    close(service).await;

    // The warm pass must reuse the durable manifest, not a process-local cache.
    let service = TraceService::open(directory.path()).await.unwrap();
    let text = "newly appended searchable message";
    let mut appended = serde_json::to_vec(&serde_json::json!({
        "type":"response_item", "payload":{"type":"message", "id":"search-new",
            "role":"assistant", "content":[{"type":"output_text", "text":text}]}
    }))
    .unwrap();
    appended.push(b'\n');
    admit_native_from(&service, &appended, records as u64 + 1, raw.len() as u64).await;
    service.project_pending().await.unwrap();
    let started = std::time::Instant::now();
    let warm = service.index_pending().await.unwrap();
    let warm_ms = started.elapsed().as_millis();
    assert_eq!(warm.sources_examined, records as u64 + 1);
    assert_eq!(warm.sources_reused, records as u64);
    assert_eq!(warm.payload_bytes_read, text.len() as u64);
    assert_eq!(
        warm.documents_written,
        cold.documents_written / records as u64
    );
    assert_eq!(warm.documents_removed, 0);
    let lease = service.lease_guard("reader", None).await.unwrap();
    let index = service
        .search_store(&lease.snapshot.generation)
        .await
        .unwrap();
    let result = index
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "newly",
                scope: &SearchScope {
                    agent: None,
                    selection: None,
                    mode: SearchHistoryMode::Authored,
                    representation: None,
                },
                allowed_environments: &["env7fk2".into()].into(),
            },
            &lease.snapshot,
            &lease.token,
            crate::models::now_ms(),
            8,
        )
        .await
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].snippet, text);
    eprintln!("search scaling: records={records} cold_ms={cold_ms} cold={cold:?} warm_ms={warm_ms} warm={warm:?}");
    drop(index);
    drop(lease);
    close(service).await;
}

#[tokio::test]
async fn search_tail_after_restart_reuses_retained_text_and_indexes_only_new_sources() {
    search_reuse_pipeline(20).await;
}

#[tokio::test]
#[ignore = "manual real-pipeline search scaling measurement; run in its own process"]
async fn search_source_scaling_probe() {
    let records = std::env::var("SYLPH_SEARCH_BENCH_RECORDS")
        .unwrap_or_else(|_| "2000".into())
        .parse()
        .unwrap();
    search_reuse_pipeline(records).await;
}

pub(super) async fn close(service: TraceService) {
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

#[tokio::test]
async fn failed_view_cleanup_survives_the_request_runtime_being_destroyed() {
    let directory = tempfile::tempdir().unwrap();
    let service = TraceService::open(directory.path()).await.unwrap();
    let guard = service
        .lease_guard("temporary-runtime-reader", None)
        .await
        .unwrap();
    let token = guard.token.clone();
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // There is deliberately no yield after drop. Tokio never polls a spawned
        // task synchronously, and this request runtime is destroyed immediately.
        runtime.block_on(async move {
            drop(guard);
        });
        drop(runtime);
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service
            .catalogue
            .resolve_lease("temporary-runtime-reader", &token, crate::models::now_ms())
            .await
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lease cleanup must outlive the request's temporary runtime");
    close(service).await;
}

#[tokio::test]
async fn bootstrap_cancellation_before_its_final_read_releases_the_unreturned_view() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let environment = [
        ("HOME".into(), directory.path().display().to_string()),
        ("SYLPH_HOME".into(), directory.path().display().to_string()),
    ]
    .into();
    let resolved = crate::config::Resolved::from_parts(&environment, &Default::default(), false);
    let mut config = crate::config::Config::from_resolved(&resolved);
    config.cookie_secret = "lease-test".into();
    let state = crate::AppState::build(config).await.unwrap();
    admit_native(&state.trace).await;
    state.trace.project_pending().await.unwrap();
    let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:response-user").unwrap());
    let route = format!("/api/v1/trace/agents/{}", agent.encode());
    let request = || {
        Request::builder()
            .uri(&route)
            .header("host", "localhost")
            .header("cookie", "sylph_session=lease-test")
            .body(Body::empty())
            .unwrap()
    };
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    *state.trace.bootstrap_response_pause.lock().await = Some((entered.clone(), resume));
    let app = crate::router(state.clone());
    let task = tokio::spawn(app.clone().oneshot(request()));
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("bootstrap must reach its last read");
    let mut reader = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(state.config.data_dir.join("evidence.db"))
            .read_only(true),
    )
    .await
    .unwrap();
    let token: String = sqlx::query_scalar(
        "SELECT lease_id FROM trace_view_leases WHERE subject='bootstrap' AND state='active'",
    )
    .fetch_one(&mut reader)
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state
            .trace
            .catalogue
            .resolve_lease("bootstrap", &token, crate::models::now_ms())
            .await
            .is_ok()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a cancelled response must not retain its unreturned view");

    let response = app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let response: ReaderBootstrap = serde_json::from_slice(&bytes).unwrap();
    assert!(
        state
            .trace
            .catalogue
            .resolve_lease("bootstrap", &response.view.token, crate::models::now_ms())
            .await
            .is_ok(),
        "successful bootstrap transfers its view to the client"
    );
    state
        .trace
        .release_lease("bootstrap", &response.view.token)
        .await
        .unwrap();
}
