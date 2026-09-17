use super::*;

fn assert_shared_roots(before: &WorkerState, after: &WorkerState) {
    assert!(before.source_states.ptr_eq(&after.source_states));
    assert!(before.pending.ptr_eq(&after.pending));
    assert!(before.source_replays.ptr_eq(&after.source_replays));
    assert!(before.gaps.ptr_eq(&after.gaps));
    assert!(before.captured_payloads.ptr_eq(&after.captured_payloads));
    assert!(before.previews.ptr_eq(&after.previews));
    assert!(before.object_provenance.ptr_eq(&after.object_provenance));
    assert!(before.correlations.ptr_eq(&after.correlations));
    assert!(
        before
            .correlation_evidence
            .ptr_eq(&after.correlation_evidence)
    );
    assert!(before.available_evidence.ptr_eq(&after.available_evidence));
    assert!(before.source_owners.ptr_eq(&after.source_owners));
    assert!(before.source_definitions.ptr_eq(&after.source_definitions));
    assert!(before.history_records.ptr_eq(&after.history_records));
    assert!(
        before
            .observation_sources
            .ptr_eq(&after.observation_sources)
    );
    assert!(
        before
            .reducer
            .claim_snapshot()
            .ptr_eq(&after.reducer.claim_snapshot())
    );
    assert!(Arc::ptr_eq(&before.reduction, &after.reduction));
}

async fn snapshot(worker: &ProjectionWorker, name: &str) -> (WorkerState, ArchiveContext) {
    let held = worker.states.lock().await.get(name).unwrap().clone();
    let mut state = held.lock().await;
    let archive = archive_context(&mut state);
    (state.clone(), archive)
}

fn retained_values(state: &WorkerState) -> serde_json::Value {
    serde_json::json!({
        "sources": state.source_states.iter().map(|(key, value)| (key, value.as_ref())).collect::<Vec<_>>(),
        "pending": state.pending.iter().map(|(key, value)| (key, value.as_ref())).collect::<Vec<_>>(),
        "replays": state.source_replays.iter().collect::<Vec<_>>(),
        "gaps": state.gaps.iter().collect::<Vec<_>>(),
        "payloads": state.captured_payloads.iter().collect::<Vec<_>>(),
        "previews": state.previews.iter().map(|(key, value)| (key, value.as_ref())).collect::<Vec<_>>(),
        "epochs": state.object_provenance.iter().collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn candidate_state_clones_share_nonempty_roots_and_isolate_large_values_at_twenty_and_two_hundred()
 {
    let fixture = Fixture::new().await;
    let capture = fixture.native_fixture().await.remove(0);
    for count in [20usize, 200] {
        let mut committed = WorkerState::default();
        for index in 0..count {
            let key = format!("source-{index}");
            let payload = PayloadRef::bytes(blake3::hash(key.as_bytes()).to_hex()).unwrap();
            let state = Arc::new(MapperState {
                values: BTreeMap::from([("retained".into(), serde_json::json!("x".repeat(4096)))]),
                ..MapperState::default()
            });
            committed.source_states.insert(key.clone(), state.clone());
            committed.pending.insert(
                key.clone(),
                Arc::new(PendingMapping {
                    admitted: AdmittedMapping {
                        admission_seq: 1,
                        occurrence_id: 1,
                        canonical_observation: capture.observation_ref.clone(),
                        capture: capture.clone(),
                    },
                    state: state.as_ref().clone(),
                    requests: vec![ArchiveRequest::ContinueHistory],
                    source_order: Some(key.clone()),
                }),
            );
            committed.source_replays.insert(
                key.clone(),
                SourceReplayCursor {
                    source: capture.source_position.source.clone(),
                    generation: 0,
                    epoch: fixture.epoch.clone(),
                    after: 1,
                    through: 2,
                },
            );
            committed.gaps.insert(
                key.clone(),
                SourceGap {
                    first: capture.observation_ref.clone(),
                    through: capture.observation_ref.clone(),
                    code: DiagnosticCode::CaptureGap,
                    reason: "retained gap".into(),
                    unmapped: 1,
                },
            );
            committed.captured_payloads.insert(
                key.clone(),
                imbl::vector![mapping::CapturedPayloadBinding {
                    parent: capture.observation_ref.clone(),
                    slot: "/body".into(),
                    attachment: CaptureAttachment {
                        locator: key.clone(),
                        source_position: None,
                        observed_version: None,
                        payload_ref: Some(payload.clone()),
                        availability: AttachmentAvailability::Captured,
                        relation: "retained-body".into(),
                    },
                    evidence: vec![capture.observation_ref.clone()],
                }],
            );
            committed.previews.insert(
                payload,
                Arc::new(sylph_trace::reading::PayloadPreview {
                    preview: "retained preview".repeat(256),
                    ..Default::default()
                }),
            );
            committed.object_provenance.insert(
                AnyObjectRef::Message(message(&key)),
                Arc::new(ObjectProvenance {
                    primary_claim: ClaimRef::native(agent(), "sharing", vec![key.clone()]).unwrap(),
                    primary_observation: capture.observation_ref.clone(),
                    primary_epoch: fixture.epoch.clone(),
                    dependency_epochs: imbl::ordset![fixture.epoch.clone()],
                }),
            );
        }
        committed.semantic_work.full_rebuilds = 7;
        let before = retained_values(&committed);
        let mut candidate = ProjectionUnit::new(&committed, 2);
        assert_shared_roots(&committed, &candidate.next);
        assert_eq!(
            candidate.next.semantic_work,
            semantic::SemanticWork::default()
        );
        let payload = PayloadRef::bytes(blake3::hash(b"source-0").to_hex()).unwrap();
        assert!(Arc::ptr_eq(
            &committed.source_states["source-0"],
            &candidate.next.source_states["source-0"]
        ));
        assert!(Arc::ptr_eq(
            &committed.pending["source-0"],
            &candidate.next.pending["source-0"]
        ));
        assert!(Arc::ptr_eq(
            &committed.previews[&payload],
            &candidate.next.previews[&payload]
        ));
        Arc::make_mut(candidate.next.source_states.get_mut("source-0").unwrap())
            .values
            .clear();
        Arc::make_mut(candidate.next.pending.get_mut("source-0").unwrap())
            .requests
            .clear();
        candidate
            .next
            .source_replays
            .get_mut("source-0")
            .unwrap()
            .after = 2;
        candidate.next.gaps.get_mut("source-0").unwrap().reason = "changed candidate".into();
        candidate
            .next
            .captured_payloads
            .get_mut("source-0")
            .unwrap()
            .clear();
        Arc::make_mut(candidate.next.previews.get_mut(&payload).unwrap())
            .preview
            .clear();
        Arc::make_mut(
            candidate
                .next
                .object_provenance
                .get_mut(&AnyObjectRef::Message(message("source-0")))
                .unwrap(),
        )
        .primary_epoch = IngestionEpoch::new("candidate-epoch").unwrap();
        let unchanged = PayloadRef::bytes(blake3::hash(b"source-1").to_hex()).unwrap();
        assert!(Arc::ptr_eq(
            &committed.source_states["source-1"],
            &candidate.next.source_states["source-1"]
        ));
        assert!(Arc::ptr_eq(
            &committed.pending["source-1"],
            &candidate.next.pending["source-1"]
        ));
        assert!(Arc::ptr_eq(
            &committed.previews[&unchanged],
            &candidate.next.previews[&unchanged]
        ));
        assert_ne!(retained_values(&candidate.next), before);
        assert_eq!(retained_values(&committed), before);
    }
}

async fn append(fixture: &Fixture, sequence: u64, offset: u64, id: &str) -> u64 {
    let mut raw = serde_json::to_vec(&serde_json::json!({"type":"response_item","payload":{
        "type":"message","id":id,"role":"assistant","content":[{"type":"output_text","text":id}]
    }}))
    .unwrap();
    raw.push(b'\n');
    fixture.native(sequence, offset, &raw).await;
    offset + raw.len() as u64
}

async fn seeded(records: usize, name: &str) -> (Fixture, GenerationStore, ProjectionWorker, u64) {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let mut offset = header.len() as u64;
    for index in 0..records {
        offset = append(&fixture, index as u64 + 2, offset, &format!("held-{index}")).await;
    }
    let (store, _) = fixture.generation(name).await;
    let worker = fixture.worker();
    worker
        .catch_up(&store, 1, records as u64 + 1)
        .await
        .unwrap();
    (fixture, store, worker, offset)
}

async fn refresh_work(records: usize) -> semantic::SemanticWork {
    let (fixture, store, worker, offset) = seeded(records, "semantic-sharing").await;
    let (committed, held) = snapshot(&worker, "semantic-sharing").await;
    assert_eq!(held.semantic.history_entries.len(), records);
    let before = held.clone();
    append(&fixture, records as u64 + 2, offset, "new-semantic-tail").await;
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    let progress = worker.catch_up(&store, 1, cut).await.unwrap();
    assert_eq!(progress.semantic.full_rebuilds, 0);
    assert_eq!(progress.semantic.append_updates, 1);
    assert!(progress.semantic.objects_examined > 0);
    assert!(progress.semantic.support_claims_examined > 0);
    assert_eq!(progress.semantic.content_observations_examined, 1);
    assert_eq!(progress.semantic.aliases_examined, 0);
    let (current, archive) = snapshot(&worker, "semantic-sharing").await;
    assert_eq!(
        current.semantic_work, progress.semantic,
        "Reading the committed archive must not hide a deferred full semantic refresh"
    );
    assert_eq!(held, before);
    assert_eq!(archive.semantic.history_entries.len(), records + 1);
    assert_eq!(committed.reducer.claim_snapshot(), held.claims);
    assert!(archive.semantic.agents.ptr_eq(&held.semantic.agents));
    assert!(
        archive
            .semantic
            .operations
            .ptr_eq(&held.semantic.operations)
    );
    assert!(
        archive
            .semantic
            .result_operations
            .ptr_eq(&held.semantic.result_operations)
    );
    let (cold, _) = fixture.generation("semantic-cold-oracle").await;
    let mut cold_worker = fixture.worker();
    cold_worker.full_reader_rebuilds = true;
    cold_worker.catch_up(&cold, 1, cut).await.unwrap();
    let (cold_state, cold_archive) = snapshot(&cold_worker, "semantic-cold-oracle").await;
    assert_eq!(*current.reduction, *cold_state.reduction);
    assert_eq!(archive, cold_archive);
    assert_eq!(
        reader_projection(&fixture, &store).await,
        reader_projection(&fixture, &cold).await
    );
    progress.semantic
}

#[tokio::test]
async fn a_semantic_tail_refresh_has_constant_work_at_twenty_and_two_hundred_and_matches_cold_projection()
 {
    assert_eq!(refresh_work(20).await, refresh_work(200).await);
}

async fn assert_retry_matches_cold(
    fixture: &Fixture,
    store: &GenerationStore,
    worker: &ProjectionWorker,
    name: &str,
    held: &ArchiveContext,
) {
    let original = held.clone();
    worker.catch_up(store, 1, 4).await.unwrap();
    let (published_state, published) = snapshot(worker, name).await;
    assert_eq!(held, &original);
    assert_eq!(published.semantic.history_entries.len(), 3);
    let (cold, _) = fixture.generation("sharing-reference").await;
    let mut cold_worker = fixture.worker();
    cold_worker.full_reader_rebuilds = true;
    cold_worker.catch_up(&cold, 1, 4).await.unwrap();
    let (cold_state, cold_archive) = snapshot(&cold_worker, "sharing-reference").await;
    assert_eq!(*published_state.reduction, *cold_state.reduction);
    assert_eq!(published, cold_archive);
    assert_eq!(
        reader_projection(fixture, store).await,
        reader_projection(fixture, &cold).await
    );
}

#[tokio::test]
async fn failed_unit_keeps_held_mapper_inputs_and_committed_roots_until_retry() {
    let name = "semantic-failure";
    let (fixture, store, worker, offset) = seeded(2, name).await;
    let (committed, held) = snapshot(&worker, name).await;
    let original = held.clone();
    let before = reader_projection(&fixture, &store).await;
    append(&fixture, 4, offset, "failed-tail-body").await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(store.path()),
    )
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_shared_state BEFORE INSERT ON mapper_states BEGIN SELECT RAISE(ABORT,'shared state rollback'); END")
        .execute(&mut connection).await.unwrap();
    assert!(worker.catch_up(&store, 1, 4).await.is_err());
    let (failed, failed_archive) = snapshot(&worker, name).await;
    assert_shared_roots(&committed, &failed);
    assert_eq!(failed_archive, held);
    assert_eq!(held, original);
    assert_eq!(
        store.checkpoint().await.unwrap().snapshot,
        *committed.committed_snapshot()
    );
    assert_eq!(reader_projection(&fixture, &store).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_shared_state")
        .execute(&mut connection)
        .await
        .unwrap();
    assert_retry_matches_cold(&fixture, &store, &worker, name, &held).await;
}

#[tokio::test]
async fn cancelled_unit_keeps_held_mapper_inputs_and_committed_roots_until_retry() {
    let name = "semantic-cancel";
    let (fixture, store, worker, offset) = seeded(2, name).await;
    let (committed, held) = snapshot(&worker, name).await;
    let original = held.clone();
    let before = reader_projection(&fixture, &store).await;
    let id = "cancelled-tail-body";
    append(&fixture, 4, offset, id).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(store.path()),
    )
    .await
    .unwrap();
    let transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let running = {
        let worker = worker.clone();
        let store = store.clone();
        tokio::spawn(async move { worker.catch_up(&store, 1, 4).await })
    };
    // This new body can appear only after the candidate was mapped. The generation
    // writer remains blocked, so none of its candidate maps can be committed yet.
    let expected = PayloadRef::bytes(blake3::hash(id.as_bytes()).to_hex()).unwrap();
    let staged = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if fixture
                .evidence
                .payload_bytes(&expected, 1024)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    transaction.rollback().await.unwrap();
    staged.expect("new candidate payload must be staged while generation publication is blocked");
    let (cancelled, cancelled_archive) = snapshot(&worker, name).await;
    assert_shared_roots(&committed, &cancelled);
    assert_eq!(cancelled_archive, held);
    assert_eq!(held, original);
    assert_eq!(
        store.checkpoint().await.unwrap().snapshot,
        *committed.committed_snapshot()
    );
    assert_eq!(reader_projection(&fixture, &store).await, before);
    assert_retry_matches_cold(&fixture, &store, &worker, name, &held).await;
}
