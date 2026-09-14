use super::*;

#[tokio::test]
async fn the_durable_event_cursor_survives_eviction_and_a_new_allocator() {
    let directory = tempfile::tempdir().unwrap();
    let store = seeded(directory.path()).await;
    let event = span_event(17, "assistant", "completed", "answer", None);
    store.append_event(&event).await.unwrap();
    sqlx::query("DELETE FROM events")
        .execute(store.raw_pool())
        .await
        .unwrap();
    let reopened = JournalStore::new(directory.path().join("journal.db").to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        reopened.max_event_seq(&session_id()).await.unwrap(),
        event.seq
    );
    let allocator = crate::events::SeqAllocator::new(reopened);
    assert_eq!(allocator.next(&session_id()).await.unwrap(), event.seq + 1);
}

#[tokio::test]
async fn startup_repair_appends_after_events_that_left_the_journal() {
    let directory = tempfile::tempdir().unwrap();
    let store = seeded(directory.path()).await;
    store
        .append_event(&span_event(1, "subagent", "started", "background", None))
        .await
        .unwrap();
    let later = span_event(41, "assistant", "completed", "later-answer", None);
    store.append_event(&later).await.unwrap();
    sqlx::query("DELETE FROM events WHERE seq = ?")
        .bind(later.seq)
        .execute(store.raw_pool())
        .await
        .unwrap();
    assert_eq!(store.close_stale_trajectories().await.unwrap(), 1);
    let closed_seq: i32 = sqlx::query_scalar(
        "SELECT seq FROM events WHERE kind = 'subagent' AND phase = 'completed'",
    )
    .fetch_one(store.read_pool())
    .await
    .unwrap();
    assert_eq!(closed_seq, later.seq + 1);
}
