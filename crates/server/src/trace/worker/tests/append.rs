use super::*;
use crate::trace::reading_store::read_scope;

async fn append_message(
    fixture: &Fixture,
    sequence: u64,
    offset: u64,
    id: &str,
    body: &str,
) -> u64 {
    let mut raw = serde_json::to_vec(&serde_json::json!({
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": id,
            "role": "assistant",
            "content": [{"type": "output_text", "text": body}]
        }
    }))
    .unwrap();
    raw.push(b'\n');
    fixture.native(sequence, offset, &raw).await;
    offset + raw.len() as u64
}

async fn warm_append_after(records: usize) -> usize {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let mut offset = header.len() as u64;
    for index in 0..records {
        offset = append_message(
            &fixture,
            index as u64 + 2,
            offset,
            &format!("independent-{index}"),
            &format!("independent message {index}"),
        )
        .await;
    }

    let (warm, _) = fixture.generation("warm-append").await;
    let worker = fixture.worker();
    let seeded = worker
        .catch_up(&warm, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    assert!(seeded.reader_rows_materialized > 0);
    let before = reader_projection(&fixture, &warm).await;
    let history_scope = read_scope(&agent(), None, "history");
    assert_eq!(
        before
            .rows
            .keys()
            .filter(|(scope, _)| scope == &history_scope)
            .count(),
        records,
        "The baseline must contain the complete seeded history"
    );

    append_message(
        &fixture,
        records as u64 + 2,
        offset,
        "warm-tail",
        "independent warm append",
    )
    .await;
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    let appended = worker.catch_up(&warm, 1, cut).await.unwrap();
    assert_eq!(
        appended.reduction.full_resolutions, 0,
        "An independent warm append must not rerun full reduction"
    );
    assert_eq!(appended.reduction.history_entries_updated, 1);
    assert!(appended.reduction.claims_resolved <= worker.limits.max_claims_per_observation);
    let warm_projection = reader_projection(&fixture, &warm).await;
    assert_eq!(
        warm_projection
            .rows
            .keys()
            .filter(|(scope, _)| scope == &history_scope)
            .count(),
        records + 1,
    );
    assert_eq!(
        revision_bytes(&fixture, &warm, &message("warm-tail")).await,
        [b"independent warm append".to_vec()]
    );
    for (key, row) in before
        .rows
        .iter()
        .filter(|((scope, _), _)| scope == &history_scope)
    {
        assert_eq!(
            warm_projection.rows.get(key),
            Some(row),
            "An independent tail cannot rewrite an earlier history row"
        );
    }

    let (cold, _) = fixture.generation("cold-reference").await;
    let mut reference_worker = fixture.worker();
    reference_worker.full_reader_rebuilds = true;
    reference_worker.catch_up(&cold, 1, cut).await.unwrap();
    assert_eq!(
        warm_projection,
        reader_projection(&fixture, &cold).await,
        "Warm append must match cold rows, order, membership, payloads, anchors, and authorization metadata"
    );
    assert!(
        appended.reader_rows_materialized <= 10,
        "Appending one independent message after {records} messages materialized {} reader rows",
        appended.reader_rows_materialized
    );
    cold.close().await;
    warm.close().await;
    fixture.evidence.close().await;
    assert!(appended.reduction.dependency_references_indexed > 0);
    appended.reader_rows_materialized
}

#[tokio::test]
async fn independent_tail_append_materializes_constant_rows_and_matches_cold_rebuild() {
    let short_history = warm_append_after(20).await;
    let long_history = warm_append_after(200).await;
    assert_eq!(
        short_history, long_history,
        "Reader row work for one independent append must not grow with prior history"
    );
}

#[tokio::test]
async fn failed_incremental_append_preserves_committed_rows_and_retries_the_same_tail() {
    let fixture = Fixture::new().await;
    let header = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    fixture.native(1, 0, header).await;
    let offset = append_message(
        &fixture,
        2,
        header.len() as u64,
        "committed",
        "committed body",
    )
    .await;
    let (store, _) = fixture.generation("append-rollback").await;
    let worker = fixture.worker();
    worker.catch_up(&store, 1, 2).await.unwrap();
    let before = reader_projection(&fixture, &store).await;
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    append_message(&fixture, 3, offset, "retry", "retry body").await;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(store.path()),
    )
    .await
    .unwrap();
    sqlx::raw_sql("CREATE TRIGGER fail_append BEFORE INSERT ON read_rows BEGIN SELECT RAISE(ABORT,'append rollback'); END")
        .execute(&mut connection).await.unwrap();
    assert!(worker.catch_up(&store, 1, 3).await.is_err());
    assert_eq!(store.checkpoint().await.unwrap().snapshot, snapshot);
    assert_eq!(reader_projection(&fixture, &store).await, before);
    sqlx::raw_sql("DROP TRIGGER fail_append")
        .execute(&mut connection)
        .await
        .unwrap();
    let retry = worker.catch_up(&store, 1, 3).await.unwrap();
    assert!(retry.reader_rows_materialized <= 10);
    let (cold, _) = fixture.generation("append-rollback-reference").await;
    let mut reference_worker = fixture.worker();
    reference_worker.full_reader_rebuilds = true;
    reference_worker.catch_up(&cold, 1, 3).await.unwrap();
    assert_eq!(
        reader_projection(&fixture, &store).await,
        reader_projection(&fixture, &cold).await
    );
}
