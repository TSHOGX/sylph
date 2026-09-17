use super::*;
use crate::trace::{generation::PayloadAnchor, reading_store::read_scope};
use serde_json::json;

const THREAD: &str = "response-user";
const TURN: &str = "message-update-turn";
const ANSWER: &str = "streaming-answer";

fn delta(text: &str) -> serde_json::Value {
    json!({"method":"item/agentMessage/delta","params":{
        "threadId":THREAD,"turnId":TURN,"itemId":ANSWER,"delta":text,
    }})
}

fn replacement(text: &str) -> serde_json::Value {
    json!({"method":"item/started","params":{
        "threadId":THREAD,"turnId":TURN,
        "item":{"type":"agentMessage","id":ANSWER,"text":text},
    }})
}

fn final_message(text: &str) -> serde_json::Value {
    json!({"method":"item/completed","params":{
        "threadId":THREAD,"turnId":TURN,
        "item":{"type":"agentMessage","id":ANSWER,"text":text,"phase":"final_answer"},
    }})
}

struct Stream {
    fixture: Fixture,
    store: GenerationStore,
    worker: ProjectionWorker,
    cold: GenerationStore,
    cold_worker: ProjectionWorker,
    sequence: u64,
    row_key: (String, String),
    initial_row: OracleRow,
    unrelated: BTreeMap<(String, String), OracleRow>,
}

impl Stream {
    async fn new(records: usize) -> Self {
        let fixture = Fixture::new().await;
        let (store, _) = fixture.generation("message-update").await;
        let (cold, _) = fixture.generation("message-update-oracle").await;
        let worker = fixture.worker();
        let mut cold_worker = fixture.worker();
        cold_worker.full_reader_rebuilds = true;
        let mut sequence = 0;
        admit(
            &fixture,
            &mut sequence,
            json!({"method":"thread/started","params":{"thread":{"id":THREAD}}}),
        )
        .await;
        admit(
            &fixture,
            &mut sequence,
            json!({"method":"turn/started","params":{
                "threadId":THREAD,"turn":{"id":TURN,"startedAt":1},
            }}),
        )
        .await;
        for index in 0..records {
            admit(&fixture, &mut sequence, json!({"method":"item/completed","params":{
                "threadId":THREAD,"turnId":TURN,
                "item":{"type":"agentMessage","id":format!("unrelated-{index}"),"text":format!("Unrelated history {index}")},
            }})).await;
        }
        admit(&fixture, &mut sequence, delta("A中")).await;
        worker.catch_up(&store, 1, sequence).await.unwrap();
        cold_worker.catch_up(&cold, 1, sequence).await.unwrap();
        let projection = reader_projection(&fixture, &store).await;
        assert_eq!(projection, reader_projection(&fixture, &cold).await);
        let history = read_scope(&agent(), None, "history");
        let mut unrelated = projection
            .rows
            .into_iter()
            .filter(|((scope, _), _)| scope == &history)
            .collect::<BTreeMap<_, _>>();
        assert_eq!(unrelated.len(), records + 1);
        let row_key = unrelated.iter().find_map(|(key, row)| {
            let unit: ReadingUnit = serde_json::from_value(row.body.clone()).unwrap();
            matches!(unit.content, ReadingContent::Message {message_ref, ..} if message_ref == message(ANSWER)).then(|| key.clone())
        }).expect("the first turn-bearing delta must have a readable message row");
        let initial_row = unrelated.remove(&row_key).unwrap();
        let stream = Self {
            fixture,
            store,
            worker,
            cold,
            cold_worker,
            sequence,
            row_key,
            initial_row,
            unrelated,
        };
        stream.assert_body(&stream.snapshot().await, "A中").await;
        stream
    }

    async fn snapshot(&self) -> Snapshot {
        self.store.checkpoint().await.unwrap().snapshot
    }

    async fn admit(&mut self, frame: serde_json::Value) {
        admit(&self.fixture, &mut self.sequence, frame).await;
    }

    async fn advance(&mut self, frame: serde_json::Value) -> WorkerProgress {
        self.admit(frame).await;
        self.worker
            .catch_up(&self.store, 1, self.sequence)
            .await
            .unwrap()
    }

    async fn target(&self, snapshot: &Snapshot) -> (ReadingUnit, PayloadAccess) {
        let row = self
            .store
            .read_row(snapshot, &self.row_key.0, &self.row_key.1)
            .await
            .unwrap();
        let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
        assert_eq!(unit.address, self.row_key.1);
        let Some(execution) = &unit.activation else {
            panic!("normal Codex turnId must survive into message execution")
        };
        let activation = ActivationRef::native(agent(), "codex.turn", vec![TURN.into()]).unwrap();
        assert_eq!(
            execution.reference,
            Some(TraceAddress::of(&AnyObjectRef::Activation(activation)))
        );
        let ReadingContent::Message {
            message_ref, parts, ..
        } = &unit.content
        else {
            panic!("streaming message remains readable")
        };
        assert_eq!(message_ref, &message(ANSWER));
        assert_eq!(parts.len(), 1);
        (unit.clone(), parts[0].payload.clone())
    }

    async fn assert_body(
        &self,
        snapshot: &Snapshot,
        expected: &str,
    ) -> (ReadingUnit, PayloadAccess) {
        let (unit, access) = self.target(snapshot).await;
        let ReadingContent::Message { parts, .. } = &unit.content else {
            unreachable!()
        };
        assert_eq!(parts[0].preview, expected);
        assert_eq!(
            self.fixture
                .evidence
                .payload_bytes(&access.payload_ref, 4096)
                .await
                .unwrap(),
            expected.as_bytes()
        );
        self.store
            .authorize_payload(snapshot, &anchor(&access), 0, expected.len() as u64)
            .await
            .unwrap();
        (unit, access)
    }

    async fn assert_cold_oracle(&self) {
        self.cold_worker
            .catch_up(&self.cold, 1, self.sequence)
            .await
            .unwrap();
        let actual = reader_projection(&self.fixture, &self.store).await;
        assert_eq!(
            actual.rows[&self.row_key].order_key,
            self.initial_row.order_key
        );
        assert_eq!(actual.rows[&self.row_key].anchor, self.initial_row.anchor);
        for (key, row) in &self.unrelated {
            assert_eq!(
                actual.rows.get(key),
                Some(row),
                "an existing-message update must preserve unrelated history rows"
            );
        }
        assert_eq!(actual, reader_projection(&self.fixture, &self.cold).await,
            "current bodies, order, epoch metadata, memberships, parts and payload reachability must match full projection");
        assert_eq!(claims(&self.store).await, claims(&self.cold).await);
    }

    async fn assert_superseded_parts(
        &self,
        old: &Snapshot,
        old_unit: &ReadingUnit,
        old_access: &PayloadAccess,
        old_text: &str,
    ) {
        let current = self.snapshot().await;
        let (new_unit, new_access) = self.target(&current).await;
        let ReadingContent::Message { revision_ref, .. } = &old_unit.content else {
            unreachable!()
        };
        let ReadingContent::Message {
            revision_ref: new_revision,
            ..
        } = &new_unit.content
        else {
            unreachable!()
        };
        let scope = sylph_trace::reading::parts_key(
            &agent(),
            None,
            &old_unit.anchor.object(),
            revision_ref,
        );
        let old_parts = self
            .store
            .read_page(old, &scope, None, None, None, 8, false)
            .await
            .unwrap();
        assert_eq!(old_parts.rows.len(), 1);
        assert_eq!(
            old_parts.rows[0].body["payload"]["payloadRef"],
            json!(old_access.payload_ref)
        );
        if revision_ref != new_revision {
            let obsolete = self
                .store
                .read_page(&current, &scope, None, None, None, 8, false)
                .await
                .unwrap();
            assert!(
                obsolete.rows.is_empty(),
                "obsolete part collections cannot survive in the current view"
            );
            assert!(matches!(
                self.store
                    .authorize_payload(&current, &anchor(old_access), 0, old_text.len() as u64)
                    .await,
                Err(GenerationError::PayloadNotReachable)
            ));
            assert_ne!(new_access.body_revision, old_access.body_revision);
        }
        self.assert_body(old, old_text).await;
    }
}

async fn admit(fixture: &Fixture, sequence: &mut u64, frame: serde_json::Value) {
    *sequence += 1;
    let issuer = Issuer::Environment(agent().environment().clone());
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: "message-update".into(),
                producer_epoch: "one".into(),
                producer_seq: *sequence,
            },
            SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: "message-update".into(),
                },
                generation: 0,
                locator: SourceLocator::Frame { ordinal: *sequence },
            },
            &serde_json::to_vec(&frame).unwrap(),
            TransportKind::LiveStdout,
        )
        .await;
}

fn anchor(access: &PayloadAccess) -> PayloadAnchor {
    PayloadAnchor {
        via_ref: access.via_ref.object(),
        containing_agent: access.containing_agent.agent(),
        selection: access.selection.clone(),
        payload_ref: access.payload_ref.clone(),
        body_revision: access.body_revision.clone(),
        representation: access.representation,
        ranges: vec![],
    }
}

#[derive(Debug, PartialEq, Eq)]
struct UpdateWork {
    reduction: ReductionWork,
    membership: MembershipWork,
    reader: sylph_trace::reading::AppendWork,
    semantic: SemanticWork,
    rows: usize,
    updates: usize,
}

fn supported_work(progress: WorkerProgress, limit: usize) -> UpdateWork {
    assert_eq!(progress.reduction.full_resolutions, 0);
    assert!(progress.reduction.claims_resolved > 0 && progress.reduction.claims_resolved <= limit);
    assert_eq!(progress.membership.entries_examined, 1);
    assert_eq!(progress.membership.full_reconciliations, 0);
    assert_eq!(progress.semantic.full_rebuilds, 0);
    assert_eq!(progress.semantic.message_updates, 1);
    assert_eq!(progress.message_updates, 1);
    assert!(!progress.reader_append.index_unavailable);
    assert!(!progress.reader_append.guard_budget_exhausted);
    assert!(progress.reader_rows_materialized > 0);
    UpdateWork {
        reduction: progress.reduction,
        membership: progress.membership,
        reader: progress.reader_append,
        semantic: progress.semantic,
        rows: progress.reader_rows_materialized,
        updates: progress.message_updates,
    }
}

async fn measured_sequence(records: usize) -> Vec<UpdateWork> {
    let mut stream = Stream::new(records).await;
    let mut result = Vec::new();
    let mut previous_text = "A中";
    for (frame, expected) in [
        (delta("A中"), "A中A中"),
        (replacement("B"), "B"),
        (delta("C"), "BC"),
        (final_message("Final differs"), "Final differs"),
        (delta("late"), "Final differs"),
    ] {
        let old = stream.snapshot().await;
        let (old_unit, old_access) = stream.target(&old).await;
        let progress = stream.advance(frame).await;
        result.push(supported_work(
            progress,
            stream.worker.limits.max_claims_per_observation,
        ));
        let (_, current) = stream.assert_body(&stream.snapshot().await, expected).await;
        if expected == "A中A中" {
            let manifest = stream
                .fixture
                .evidence
                .payload_manifest(&current.payload_ref)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(manifest.chunks.len(), 2);
            assert_eq!(manifest.chunks[0].digest, manifest.chunks[1].digest);
        }
        stream
            .assert_superseded_parts(&old, &old_unit, &old_access, previous_text)
            .await;
        stream.assert_cold_oracle().await;
        previous_text = expected;
    }
    result
}

#[tokio::test]
async fn turn_bearing_message_updates_bound_work_independently_of_twenty_or_two_hundred_unrelated_messages(
) {
    assert_eq!(measured_sequence(20).await, measured_sequence(200).await);
}

#[tokio::test]
#[ignore = "production message projection scaling measurement"]
async fn existing_message_scaling_probe() {
    let records = std::env::var("SYLPH_MESSAGE_BENCH_RECORDS")
        .map(|value| {
            value
                .parse::<usize>()
                .expect("message benchmark record count is an integer")
        })
        .unwrap_or(2000);
    assert!(records > 0);
    let seed_started = std::time::Instant::now();
    let mut stream = Stream::new(records).await;
    eprintln!(
        "message-benchmark records={records} seed_with_admission_and_baseline_oracle_seconds={:.6}",
        seed_started.elapsed().as_secs_f64(),
    );
    let mut retained = Vec::new();
    let mut previous_text = "A中";
    for (phase, frame, expected) in [
        ("duplicate-delta", delta("A中"), "A中A中"),
        ("replacement", replacement("B"), "B"),
        ("replacement-delta", delta("C"), "BC"),
        ("final", final_message("Final differs"), "Final differs"),
        ("late-delta", delta("late"), "Final differs"),
    ] {
        let old = stream.snapshot().await;
        let (unit, access) = stream.target(&old).await;
        retained.push((old, unit, access, previous_text));
        // Admission durability and assertion/oracle work are measured separately
        // from the production worker's mapping, projection, and commit work.
        stream.admit(frame).await;
        let projected_at = std::time::Instant::now();
        let progress = stream
            .worker
            .catch_up(&stream.store, 1, stream.sequence)
            .await
            .unwrap();
        let projection_seconds = projected_at.elapsed().as_secs_f64();
        eprintln!(
            "message-benchmark records={records} phase={phase} projection_seconds={projection_seconds:.6} counters={}",
            serde_json::to_string(&progress).unwrap(),
        );
        supported_work(progress, stream.worker.limits.max_claims_per_observation);
        let (_, current) = stream.assert_body(&stream.snapshot().await, expected).await;
        if phase == "duplicate-delta" {
            let manifest = stream
                .fixture
                .evidence
                .payload_manifest(&current.payload_ref)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(manifest.chunks.len(), 2);
            assert_eq!(manifest.chunks[0].digest, manifest.chunks[1].digest);
        }
        for (snapshot, unit, access, text) in &retained {
            stream
                .assert_superseded_parts(snapshot, unit, access, text)
                .await;
        }
        previous_text = expected;
    }
    let oracle_started = std::time::Instant::now();
    stream.assert_cold_oracle().await;
    eprintln!(
        "message-benchmark records={records} final_cold_oracle_seconds={:.6}",
        oracle_started.elapsed().as_secs_f64(),
    );
}

#[tokio::test]
async fn failed_existing_message_update_preserves_old_parts_and_retries_without_losing_bytes() {
    let mut stream = Stream::new(2).await;
    let old = stream.snapshot().await;
    let (unit, access) = stream.target(&old).await;
    let before = reader_projection(&stream.fixture, &stream.store).await;
    stream.admit(delta("A中")).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(stream.store.path()),
    )
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TRIGGER reject_message_update BEFORE INSERT ON read_rows BEGIN SELECT RAISE(ABORT,'message update rollback'); END")
        .execute(&mut connection).await.unwrap();
    assert!(stream
        .worker
        .catch_up(&stream.store, 1, stream.sequence)
        .await
        .is_err());
    assert_eq!(stream.snapshot().await, old);
    assert_eq!(
        reader_projection(&stream.fixture, &stream.store).await,
        before
    );
    stream.assert_body(&old, "A中").await;
    sqlx::raw_sql("DROP TRIGGER reject_message_update")
        .execute(&mut connection)
        .await
        .unwrap();
    let progress = stream
        .worker
        .catch_up(&stream.store, 1, stream.sequence)
        .await
        .unwrap();
    supported_work(progress, stream.worker.limits.max_claims_per_observation);
    stream.assert_body(&stream.snapshot().await, "A中A中").await;
    stream
        .assert_superseded_parts(&old, &unit, &access, "A中")
        .await;
    stream.assert_cold_oracle().await;
}

#[tokio::test]
async fn cancelled_existing_message_update_keeps_the_committed_snapshot_and_can_retry() {
    let mut stream = Stream::new(2).await;
    let old = stream.snapshot().await;
    let before = reader_projection(&stream.fixture, &stream.store).await;
    stream.admit(delta("cancelled-append")).await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(stream.store.path()),
    )
    .await
    .unwrap();
    let transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let running = {
        let worker = stream.worker.clone();
        let store = stream.store.clone();
        let cut = stream.sequence;
        tokio::spawn(async move { worker.catch_up(&store, 1, cut).await })
    };
    let payload = PayloadRef::bytes(blake3::hash(b"cancelled-append").to_hex()).unwrap();
    let staged = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stream
            .fixture
            .evidence
            .payload_bytes(&payload, 1024)
            .await
            .is_err()
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    transaction.rollback().await.unwrap();
    staged.expect(
        "cancellation window must observe retained new output before the blocked generation commit",
    );
    assert_eq!(stream.snapshot().await, old);
    assert_eq!(
        reader_projection(&stream.fixture, &stream.store).await,
        before
    );
    let progress = stream
        .worker
        .catch_up(&stream.store, 1, stream.sequence)
        .await
        .unwrap();
    supported_work(progress, stream.worker.limits.max_claims_per_observation);
    stream
        .assert_body(&stream.snapshot().await, "A中cancelled-append")
        .await;
    stream.assert_body(&old, "A中").await;
    stream.assert_cold_oracle().await;
}

#[tokio::test]
async fn restarted_message_updates_reconstruct_old_views_and_resume_bounded_warm_updates() {
    let mut stream = Stream::new(20).await;
    let old = stream.snapshot().await;
    let (unit, access) = stream.target(&old).await;
    let path = stream.store.path().to_path_buf();
    let manifest = stream.store.manifest().clone();
    let limits = stream.store.limits().clone();
    stream.store.close().await;
    stream.store = GenerationStore::open(path, manifest, limits).await.unwrap();
    stream.worker = stream.fixture.worker();
    let restored = stream
        .worker
        .catch_up(&stream.store, 1, stream.sequence)
        .await
        .unwrap();
    assert!(
        restored.reduction.full_resolutions > 0,
        "restart restoration work must be reported"
    );
    stream.advance(delta("A中")).await;
    stream.assert_body(&stream.snapshot().await, "A中A中").await;
    stream
        .assert_superseded_parts(&old, &unit, &access, "A中")
        .await;
    stream.assert_cold_oracle().await;
    let progress = stream.advance(replacement("Restored replacement")).await;
    supported_work(progress, stream.worker.limits.max_claims_per_observation);
    stream
        .assert_body(&stream.snapshot().await, "Restored replacement")
        .await;
    stream.assert_cold_oracle().await;
}
