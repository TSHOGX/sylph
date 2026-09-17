use super::*;
use crate::trace::reading_store::read_scope;
use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};

struct SelectedHistory {
    fixture: Fixture,
    generation: GenerationStore,
    worker: ProjectionWorker,
    before: Snapshot,
    through: u64,
    records: usize,
}

async fn selected_history(records: usize, max_changes: usize) -> SelectedHistory {
    let fixture = Fixture::new().await;
    let mut offset = 0;
    let mut header = serde_json::to_vec(&serde_json::json!({
        "type":"session_meta", "ordinal":0,
        "payload":{"id":"response-user","history_mode":"paginated"}
    }))
    .unwrap();
    header.push(b'\n');
    let first = fixture.native(1, 0, &header).await;
    offset += header.len() as u64;
    for index in 0..records {
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "type":"response_item", "ordinal":index+1,
            "payload":{"type":"message","id":format!("selection-{index}"),"role":"assistant",
                "content":[{"type":"output_text","text":format!("retained selection message {index}")}]}
        })).unwrap();
        bytes.push(b'\n');
        fixture.native(index as u64 + 2, offset, &bytes).await;
        offset += bytes.len() as u64;
    }
    let issuer = first.observation_ref.issuer.clone();
    let inventory = SourceRef {
        issuer: issuer.clone(),
        instance: "selection-inventory".into(),
    };
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: "selection-inventory".into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            },
            SourcePosition {
                source: inventory.clone(),
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 1 },
            },
            &serde_json::to_vec(&SourceDefinition {
                source: first.source_position.source.clone(),
                generation: 0,
                locator: "/retained/selected.jsonl".into(),
                owner: agent(),
                basis: SourceOwnerBasis::NativeIdentity {
                    observation: first.observation_ref,
                },
            })
            .unwrap(),
            TransportKind::SourceInventory,
        )
        .await;
    let (generation, _) = fixture
        .generation_with_limits(
            "large-selection",
            QueryLimits {
                max_batch_changes: max_changes,
                ..QueryLimits::default()
            },
        )
        .await;
    let worker = fixture.worker();
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&generation, 1, cut).await.unwrap();
    let before = generation.checkpoint().await.unwrap().snapshot;
    let snapshot = NativeDatabaseSnapshot {
        database_locator: "/vendor/state_5.sqlite".into(),
        table: "threads".into(),
        columns: vec![],
        status: NativeDatabaseStatus::Available,
        row: [(
            "id".into(),
            NativeDatabaseValue::Text("response-user".into()),
        )]
        .into(),
        selected_source: Some(first.source_position.source),
        selected_generation: Some(0),
        selected_record_count: Some(records as u64 + 1),
        selected_pending_bytes: Some(0),
    };
    fixture
        .admit(
            ObservationRef {
                issuer,
                producer_id: "selection-inventory".into(),
                producer_epoch: "one".into(),
                producer_seq: 2,
            },
            SourcePosition {
                source: inventory,
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 2 },
            },
            &serde_json::to_vec(&snapshot).unwrap(),
            TransportKind::NativeDatabase,
        )
        .await;
    let through = fixture.evidence.evidence_cut().await.unwrap();
    SelectedHistory {
        fixture,
        generation,
        worker,
        before,
        through,
        records,
    }
}

async fn assert_complete(setup: &SelectedHistory, worker: &ProjectionWorker) {
    let (snapshot, reduction, _) = worker.projected_snapshot(&setup.generation).await.unwrap();
    assert_eq!(snapshot.evidence_cut, setup.through);
    let history = &reduction.histories[&agent()];
    let selected = history
        .selection
        .known()
        .expect("the retained database selects its complete source");
    assert_eq!(
        history.selected_entries.as_ref().unwrap().len(),
        setup.records
    );
    let scope = read_scope(&agent(), Some(selected), "history");
    let mut after = None;
    let mut previews = Vec::new();
    loop {
        let page = setup
            .generation
            .read_page(
                &snapshot,
                &scope,
                None,
                after.as_deref(),
                None,
                setup.generation.limits().max_items,
                false,
            )
            .await
            .unwrap();
        for row in page.rows {
            let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
            let ReadingContent::Message { parts, .. } = unit.content else {
                panic!("selected message")
            };
            previews.push(parts[0].preview.clone());
        }
        if !page.has_after {
            break;
        }
        after = page.after;
    }
    assert_eq!(
        previews,
        (0..setup.records)
            .map(|index| format!("retained selection message {index}"))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn one_valid_selection_larger_than_a_projection_batch_publishes_every_retained_message() {
    let setup = selected_history(150, QueryLimits::default().max_batch_changes).await;
    let captures = setup
        .fixture
        .evidence
        .admissions_after(setup.before.evidence_cut, 1)
        .await
        .unwrap()
        .captures;
    let held = setup
        .worker
        .states
        .lock()
        .await
        .get("large-selection")
        .unwrap()
        .clone();
    let state = held.lock().await;
    let prepared = setup
        .worker
        .prepare_unit(&setup.generation, &state, &captures, setup.through)
        .await
        .unwrap();
    assert!(prepared.batch.read_rows.len() > setup.generation.limits().max_batch_changes);
    assert!(prepared.batch.claims.len() < setup.worker.limits.max_claims_per_observation);
    eprintln!(
        "selection reconciliation: messages={} claims={} changed_reader_rows={} max_batch_changes={}",
        setup.records,
        prepared.batch.claims.len(),
        prepared.batch.read_rows.len(),
        setup.generation.limits().max_batch_changes,
    );
    drop(prepared);
    drop(state);
    let progress = setup
        .worker
        .catch_up(&setup.generation, 1, setup.through)
        .await
        .unwrap();
    assert_eq!(
        progress.suspended_sources, 0,
        "a large valid selection is not a source decoding gap"
    );
    assert_complete(&setup, &setup.worker).await;
    assert_eq!(
        setup
            .generation
            .snapshot_at(setup.before.view_revision)
            .await
            .unwrap(),
        setup.before
    );
}

#[tokio::test]
async fn cancelled_private_reconciliation_keeps_old_views_and_resumes_after_reopen() {
    let setup = selected_history(80, 64).await;
    let old_states = setup.generation.mapper_states(&setup.before).await.unwrap();
    let old_page = setup
        .generation
        .read_page(
            &setup.before,
            &read_scope(&agent(), None, "history"),
            None,
            None,
            None,
            8,
            false,
        )
        .await
        .unwrap();
    let old_unit: ReadingUnit = serde_json::from_value(old_page.rows[0].body.clone()).unwrap();
    let ReadingContent::Message { parts, .. } = old_unit.content else {
        panic!("retained message")
    };
    let old_body = &parts[0].payload;
    let (entered, _resume) = setup.generation.pause_reconciliation_after(0);
    let worker = setup.fixture.worker();
    let generation = setup.generation.clone();
    let through = setup.through;
    let task = tokio::spawn(async move { worker.catch_up(&generation, 1, through).await });
    tokio::time::timeout(std::time::Duration::from_secs(30), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        setup.generation.checkpoint().await.unwrap().snapshot,
        setup.before
    );
    assert_eq!(
        setup.generation.mapper_states(&setup.before).await.unwrap(),
        old_states
    );
    let future = Snapshot {
        view_revision: setup.before.view_revision + 1,
        evidence_cut: setup.through,
        ..setup.before.clone()
    };
    assert!(
        setup
            .generation
            .snapshot_at(future.view_revision)
            .await
            .is_err()
    );
    assert!(
        setup
            .generation
            .read_page(
                &future,
                &read_scope(&agent(), None, "history"),
                None,
                None,
                None,
                8,
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        setup
            .generation
            .read_page(
                &setup.before,
                &read_scope(&agent(), None, "history"),
                None,
                None,
                None,
                8,
                false
            )
            .await
            .unwrap()
            .rows
            .iter()
            .map(|row| row.body.clone())
            .collect::<Vec<_>>(),
        old_page
            .rows
            .iter()
            .map(|row| row.body.clone())
            .collect::<Vec<_>>()
    );
    setup
        .generation
        .authorize_payload(
            &setup.before,
            &crate::trace::generation::PayloadAnchor {
                via_ref: old_body.via_ref.object(),
                containing_agent: old_body.containing_agent.agent(),
                selection: old_body.selection.clone(),
                payload_ref: old_body.payload_ref.clone(),
                body_revision: old_body.body_revision.clone(),
                representation: old_body.representation,
                ranges: vec![],
                dependency_epochs: BTreeSet::new(),
            },
            0,
            old_body.total_bytes.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        setup
            .fixture
            .evidence
            .payload_bytes(&old_body.payload_ref, 100)
            .await
            .unwrap(),
        b"retained selection message 0"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let reopened = GenerationStore::open(
        setup.generation.path(),
        setup.generation.manifest().clone(),
        setup.generation.limits().clone(),
    )
    .await
    .unwrap();
    let worker = setup.fixture.worker();
    worker.catch_up(&reopened, 1, setup.through).await.unwrap();
    assert_complete(&setup, &worker).await;
    let fence = reopened.fence(1).await.unwrap();
    assert!(matches!(
        setup
            .generation
            .apply(
                1,
                &fence.checkpoint,
                &ProjectionBatch {
                    through_cut: setup.through,
                    continuation: true,
                    ..ProjectionBatch::default()
                }
            )
            .await,
        Err(GenerationError::Fenced)
    ));
}

#[tokio::test]
async fn fencing_waits_for_the_complete_reconciliation_and_refuses_its_old_writer() {
    let setup = selected_history(80, 64).await;
    let (entered, resume) = setup.generation.pause_reconciliation_after(1);
    let worker = setup.fixture.worker();
    let generation = setup.generation.clone();
    let through = setup.through;
    let writing = tokio::spawn(async move { worker.catch_up(&generation, 1, through).await });
    tokio::time::timeout(std::time::Duration::from_secs(30), entered.notified())
        .await
        .unwrap();
    let generation = GenerationStore::open(
        setup.generation.path(),
        setup.generation.manifest().clone(),
        setup.generation.limits().clone(),
    )
    .await
    .unwrap();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let fencing = tokio::spawn(async move {
        started.send(()).unwrap();
        generation.fence(1).await
    });
    waiting.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        !fencing.is_finished(),
        "a fence cannot sample partially staged rows"
    );
    assert_eq!(
        setup.generation.checkpoint().await.unwrap().snapshot,
        setup.before
    );
    resume.notify_one();
    writing.await.unwrap().unwrap();
    let fence = fencing.await.unwrap().unwrap();
    assert_eq!(fence.checkpoint.evidence_cut, setup.through);
    assert_eq!(
        fence.checkpoint.view_revision,
        setup.before.view_revision + 1
    );
    assert!(matches!(
        setup
            .generation
            .apply(
                1,
                &fence.checkpoint,
                &ProjectionBatch {
                    through_cut: setup.through,
                    continuation: true,
                    ..ProjectionBatch::default()
                },
            )
            .await,
        Err(GenerationError::Fenced)
    ));
    setup.generation.resume(&fence).await.unwrap();
    assert_complete(&setup, &setup.fixture.worker()).await;
}

#[tokio::test]
async fn failed_final_reconciliation_commit_keeps_the_old_cut_until_a_retry_publishes() {
    let setup = selected_history(80, 64).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(setup.generation.path()),
    )
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_reconciliation BEFORE UPDATE OF view_revision ON projection_state BEGIN SELECT RAISE(ABORT,'publication fault'); END")
        .execute(&mut connection).await.unwrap();
    assert!(
        setup
            .worker
            .catch_up(&setup.generation, 1, setup.through)
            .await
            .is_err()
    );
    assert_eq!(
        setup.generation.checkpoint().await.unwrap().snapshot,
        setup.before
    );
    assert!(
        setup
            .generation
            .changes_after(setup.before.view_revision, 8)
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("DROP TRIGGER reject_reconciliation")
        .execute(&mut connection)
        .await
        .unwrap();
    let restarted = setup.fixture.worker();
    restarted
        .catch_up(&setup.generation, 1, setup.through)
        .await
        .unwrap();
    assert_complete(&setup, &restarted).await;
}

/// S1.4 asks whether the cumulative reducer-claim limit blocks the required
/// histories. The largest retained Codex root in
/// `docs/measurements/unified-trace/codex-structure.json` is 13009 records, and
/// `WorkerLimits::max_reducer_claims` defaults to 250000, so the answer is a rate
/// rather than an opinion: measure claims per retained record at two scales and
/// project the ceiling. A rate measured at one size could be a fixed overhead.
#[tokio::test]
async fn retained_claims_per_record_leave_the_largest_corpus_history_within_its_limit() {
    // The three named long roots peak at this many records for one logical thread.
    const LARGEST_RETAINED_HISTORY: usize = 13009;
    let mut samples = Vec::new();
    for records in [50usize, 200] {
        let setup = selected_history(records, QueryLimits::default().max_batch_changes).await;
        setup
            .worker
            .catch_up(&setup.generation, 1, setup.through)
            .await
            .unwrap();
        let held = setup
            .worker
            .states
            .lock()
            .await
            .get("large-selection")
            .unwrap()
            .clone();
        let claims = held.lock().await.reducer.len();
        samples.push((records, claims));
    }
    let (small_records, small_claims) = samples[0];
    let (large_records, large_claims) = samples[1];
    // Separate the per-record slope from fixed header/inventory overhead, so the
    // projection does not inherit a constant as though it scaled.
    let slope = (large_claims - small_claims) as f64 / (large_records - small_records) as f64;
    let fixed = large_claims as f64 - slope * large_records as f64;
    let projected = (slope * LARGEST_RETAINED_HISTORY as f64 + fixed).ceil() as usize;
    let limit = setup_limit();
    eprintln!(
        "reducer_claim_rate small=({small_records},{small_claims}) large=({large_records},{large_claims}) claims_per_record={slope:.3} fixed_overhead={fixed:.1} projected_at_{LARGEST_RETAINED_HISTORY}={projected} max_reducer_claims={limit}"
    );
    assert!(
        slope > 0.0,
        "a per-record claim rate of zero would mean the measurement observed no growth"
    );
    assert!(
        projected < limit,
        "the largest retained history projects to {projected} claims against a {limit} limit, so the cumulative reducer-claim limit blocks a required history"
    );
    // The synthetic sweep above holds one record shape constant, which measures
    // the slope cleanly but understates a real history's mix of lifecycle,
    // content and completion records. Measure the retained fixture's actual mix
    // so the headroom is stated against an observed shape, not an invented one.
    let fixture = Fixture::new().await;
    let captures = fixture.native_fixture().await;
    let (generation, _) = fixture.generation("claim-rate-mixed").await;
    let worker = fixture.worker();
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&generation, 1, cut).await.unwrap();
    let mixed = worker
        .states
        .lock()
        .await
        .get("claim-rate-mixed")
        .unwrap()
        .clone();
    let mixed_claims = mixed.lock().await.reducer.len();
    let mixed_rate = mixed_claims as f64 / captures.len() as f64;
    let mixed_projection = (mixed_rate * LARGEST_RETAINED_HISTORY as f64).ceil() as usize;
    let mixed_records = captures.len();
    eprintln!(
        "reducer_claim_rate_mixed records={mixed_records} claims={mixed_claims} claims_per_record={mixed_rate:.3} projected_at_{LARGEST_RETAINED_HISTORY}={mixed_projection} max_reducer_claims={limit}"
    );
    assert!(
        mixed_projection < limit,
        "the retained mixed-shape rate projects to {mixed_projection} claims at {LARGEST_RETAINED_HISTORY} records against a {limit} limit"
    );
}

fn setup_limit() -> usize {
    WorkerLimits::default().max_reducer_claims
}
