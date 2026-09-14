use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

async fn store() -> (HistoryStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(
        dir.path().join("history.db").to_str().unwrap(),
        BlobStore::new(dir.path().join("blobs")),
    )
    .await
    .unwrap();
    (store, dir)
}

fn block(session: &str, seq: i32, text: &str) -> Block {
    Block {
        session_id: session.into(),
        seq,
        turn_id: format!("turn-{session}"),
        kind: "assistant".into(),
        phase: Some("completed".into()),
        stream_id: None,
        span: None,
        interior_state: None,
        name: None,
        text: text.into(),
        folded: None,
        data: None,
        is_error: false,
        source: "agent".into(),
        arrival_at_ms: 1,
        recorded_at_ms: Some(1),
        started_at_ms: 1,
        started_recorded_at_ms: Some(1),
        created_at_ms: 1,
    }
}

fn record(version: u64) -> NativeRecord {
    NativeRecord {
        source: "rollout.jsonl".into(),
        version,
        raw_json: None,
        payload: serde_json::json!({"version": version}),
        blobs: Vec::new(),
    }
}

#[tokio::test]
async fn history_writer_cannot_block_native_retention_or_timeline_reads() {
    let (store, _dir) = store().await;
    store
        .put_blocks(&[block("session", 0, "committed")])
        .await
        .unwrap();
    let mut held = store.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("UPDATE blocks SET text_preview = 'uncommitted'")
        .execute(&mut *held)
        .await
        .unwrap();
    // The lock window stays open until both operations finish; natural scheduler
    // timing can never make this pass by running after the competing commit.
    tokio::time::timeout(Duration::from_secs(2), async {
        store
            .put_native_record("session", &record(1))
            .await
            .unwrap();
        assert_eq!(
            store.blocks("session", -1).await.unwrap()[0].text,
            "committed"
        );
    })
    .await
    .expect("history must not stall native acknowledgement or reads");
    held.rollback().await.unwrap();
    assert_eq!(
        store
            .native_record("session", "rollout.jsonl", 1)
            .await
            .unwrap(),
        Some(record(1).payload)
    );
}

#[tokio::test]
async fn native_writer_cannot_reserve_history_and_readers_cannot_write() {
    let (store, _dir) = store().await;
    let held = store
        .native_writer
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        store.put_blocks(&[block("session", 0, "searchable")]),
    )
    .await
    .expect("history writer must not attach native.db")
    .unwrap();
    assert!(sqlx::query("DELETE FROM blocks")
        .execute(&store.readers)
        .await
        .is_err());
    assert!(sqlx::query("DELETE FROM native.native_records")
        .execute(&store.readers)
        .await
        .is_err());
    held.rollback().await.unwrap();
}

async fn measured_write(store: &HistoryStore, value: Block) -> usize {
    let steps = Arc::new(AtomicUsize::new(0));
    let count = steps.clone();
    let mut conn = store.pool.acquire().await.unwrap();
    conn.lock_handle()
        .await
        .unwrap()
        .set_progress_handler(1, move || {
            count.fetch_add(1, Ordering::Relaxed);
            true
        });
    drop(conn);
    store.put_blocks(&[value]).await.unwrap();
    let mut conn = store.pool.acquire().await.unwrap();
    conn.lock_handle().await.unwrap().remove_progress_handler();
    steps.load(Ordering::Relaxed)
}

#[tokio::test]
async fn replacing_one_search_entry_does_not_scan_unrelated_history() {
    let (store, _dir) = store().await;
    store
        .put_blocks(&[block("target", 0, "oldword")])
        .await
        .unwrap();
    let small = measured_write(&store, block("target", 0, "newword")).await;
    let unrelated: Vec<_> = (0..2000)
        .map(|seq| block("unrelated", seq, "otherword"))
        .collect();
    store.put_blocks(&unrelated).await.unwrap();
    let large = measured_write(&store, block("target", 0, "finalword")).await;
    eprintln!("FTS replacement VM steps: small={small}, with 2000 unrelated blocks={large}");
    assert!(
        large < small * 4,
        "replacement work must not grow with the entire catalogue: {small} -> {large}"
    );
    assert!(store.search("newword", 10).await.unwrap().is_empty());
    assert_eq!(store.search("finalword", 10).await.unwrap()[0].hits, 1);
    assert_eq!(store.measure_session_bytes("target").await.unwrap(), (9, 1));
    store.prune_below("target", 0).await.unwrap();
    assert!(store.search("finalword", 10).await.unwrap().is_empty());
    assert_eq!(store.measure_session_bytes("target").await.unwrap(), (0, 0));
    assert_eq!(store.block_count("unrelated").await.unwrap(), 2000);
}

#[tokio::test]
async fn concurrent_retention_and_publication_preserve_all_native_records() {
    let (store, _dir) = store().await;
    let blocks = store.clone();
    let publish = tokio::spawn(async move {
        for i in 0..4 {
            let batch: Vec<_> = (i * 64..(i + 1) * 64)
                .map(|seq| block("session", seq, "indexed evidence"))
                .collect();
            blocks.put_blocks(&batch).await.unwrap();
        }
    });
    let raw = store.clone();
    let native = tokio::spawn(async move {
        for version in 0..100 {
            raw.put_native_record("session", &record(version))
                .await
                .unwrap();
        }
    });
    let accounting = store.clone();
    let maintenance = tokio::spawn(async move {
        for _ in 0..30 {
            accounting.record_session_bytes("session").await.unwrap();
            accounting.prune_below("unrelated", 1000).await.unwrap();
            accounting.sweep_unreferenced_blobs().await.unwrap();
        }
    });
    for task in [publish, native, maintenance] {
        task.await.unwrap();
    }
    assert_eq!(store.native_records("session").await.unwrap().len(), 100);
    assert_eq!(store.block_count("session").await.unwrap(), 256);
    let raw_bytes: i64 = (0..100)
        .map(|v| record(v).payload.to_string().len() as i64)
        .sum();
    assert_eq!(
        store.measure_session_bytes("session").await.unwrap(),
        (raw_bytes + 256 * 16, 256)
    );
    assert_eq!(store.native_record_failures(), 0);
}

#[tokio::test]
async fn garbage_collection_defers_during_publication_then_reclaims_replaced_bodies() {
    let (store, _dir) = store().await;
    store
        .put_blocks(&[block("session", 0, &"old body".repeat(1024))])
        .await
        .unwrap();
    let hash: String =
        sqlx::query_scalar("SELECT text_hash FROM blocks WHERE session_id='session'")
            .fetch_one(&store.readers)
            .await
            .unwrap();
    store
        .put_blocks(&[block("session", 0, "replacement")])
        .await
        .unwrap();
    let publication = store.publication.read().await;
    assert_eq!(store.sweep_unreferenced_blobs().await.unwrap(), 0);
    assert!(store.blobs.get(&hash).await.unwrap().is_some());
    drop(publication);
    assert_eq!(store.sweep_unreferenced_blobs().await.unwrap(), 1);
    assert!(store.blobs.get(&hash).await.unwrap().is_none());
    assert_eq!(store.sweep_unreferenced_blobs().await.unwrap(), 0);
    assert_eq!(
        store.blocks("session", -1).await.unwrap()[0].text,
        "replacement"
    );
}
