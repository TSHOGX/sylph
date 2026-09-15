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

#[tokio::test]
async fn archive_cleanup_preserves_changed_versions_even_when_the_payload_changes_back() {
    let directory = tempfile::tempdir().unwrap();
    let store = seeded(directory.path()).await;
    let event = span_event(1, "subagent", "started", "background", None);
    store.append_event(&event).await.unwrap();
    let snapshot = store.consolidation_snapshot("turn-1").await.unwrap();
    for phase in ["completed", "started"] {
        sqlx::query("UPDATE events SET phase=? WHERE seq=1")
            .bind(phase)
            .execute(store.raw_pool())
            .await
            .unwrap();
    }
    assert_eq!(store.delete_turn_events(&snapshot).await.unwrap(), 0);
    assert_eq!(
        store
            .turn_events_for_consolidation("turn-1")
            .await
            .unwrap()
            .len(),
        1
    );
    let current = store.consolidation_snapshot("turn-1").await.unwrap();
    assert_eq!(store.delete_turn_events(&current).await.unwrap(), 1);
}

#[tokio::test]
async fn archive_cleanup_rolls_back_all_batches_when_a_later_delete_fails() {
    let directory = tempfile::tempdir().unwrap();
    let store = seeded(directory.path()).await;
    let events: Vec<_> = (0..257)
        .map(|seq| span_event(seq, "assistant", "delta", "reply", None))
        .collect();
    store.append_events(&events).await.unwrap();
    let snapshot = store.consolidation_snapshot("turn-1").await.unwrap();
    sqlx::query("CREATE TRIGGER reject_archive_delete BEFORE DELETE ON events WHEN OLD.seq=256 BEGIN SELECT RAISE(FAIL,'injected cleanup failure'); END").execute(store.raw_pool()).await.unwrap();
    assert!(store.delete_turn_events(&snapshot).await.is_err());
    assert_eq!(
        store
            .turn_events_for_consolidation("turn-1")
            .await
            .unwrap()
            .len(),
        events.len()
    );
    sqlx::query("DROP TRIGGER reject_archive_delete")
        .execute(store.raw_pool())
        .await
        .unwrap();
    assert_eq!(
        store.delete_turn_events(&snapshot).await.unwrap(),
        events.len() as u64
    );
}
