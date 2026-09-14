use super::*;

async fn preserves_the_reserved_row_after_remote_ingest(fail_first: bool) {
    let (bridge, _directory) = bridge().await;
    let session_id = session("reserved-sequence");
    bridge
        .store
        .create_session(&Session {
            id: session_id.clone(),
            workdir: Some("/tmp".into()),
            title: None,
            inject_identity: false,
            fidelity: sylph_protocol::Fidelity::Stream,
            flags_json: "{}".into(),
            status: "idle".into(),
            created_at_ms: 1,
            updated_at_ms: 1,
        })
        .await
        .unwrap();
    bridge
        .store
        .upsert_environment(
            &session_id.environment,
            "install",
            "fixture",
            "development",
            &hash_secret("token"),
            "[]",
        )
        .await
        .unwrap();
    let producer = ProducerId::parse("reserved-producer").unwrap();
    let (sender, _receiver) = mpsc::channel(4);
    let hello = sylph_protocol::Hello::current(
        "install",
        producer.clone(),
        Some(session_id.environment.clone()),
        0,
    );
    let welcome = bridge
        .register_connection(&hello, "token", sender)
        .await
        .unwrap();
    let reserved = bridge.seq.next(&session_id).await.unwrap();
    let mut event = Event::new(EventKind::Assistant, Some(Phase::Completed), 10);
    event.text = "remote event".into();
    let item = sylph_protocol::SequencedEvent::pending(
        producer.clone(),
        ProducerSeq::new(1),
        session_id.clone(),
        event,
        None,
    );
    let batch = EventBatch::new(
        session_id.environment.clone(),
        producer,
        welcome.connection_epoch,
        vec![item],
    )
    .unwrap();

    if fail_first {
        sqlx::query("CREATE TRIGGER reject_reserved_sequence_test BEFORE INSERT ON events BEGIN SELECT RAISE(FAIL, 'forced write failure'); END")
            .execute(bridge.store.raw_pool()).await.unwrap();
        assert!(bridge.ingest_batch(batch.clone()).await.is_err());
        sqlx::query("DROP TRIGGER reject_reserved_sequence_test")
            .execute(bridge.store.raw_pool())
            .await
            .unwrap();
    }

    assert!(matches!(
        bridge.ingest_batch(batch).await.unwrap(),
        IngestResult::Ack(_)
    ));
    let mut pending =
        StreamEvent::from_protocol(&Event::new(EventKind::Assistant, Some(Phase::Completed), 9));
    pending.seq = reserved;
    pending.text = "reserved server event".into();
    pending.source = "server".into();
    let mut row = pending.to_row(&session_id, "");
    row.turn_id = None;
    bridge.store.append_event(&row).await.unwrap();

    let rows: Vec<(i32, String)> = sqlx::query_as("SELECT seq, text FROM events ORDER BY seq")
        .fetch_all(bridge.store.read_pool())
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (reserved, "reserved server event".into()),
            (reserved + 1, "remote event".into()),
        ]
    );
    assert_eq!(bridge.seq.next(&session_id).await.unwrap(), reserved + 2);
}

#[tokio::test]
async fn remote_ingest_does_not_reuse_an_uncommitted_server_reservation() {
    preserves_the_reserved_row_after_remote_ingest(false).await;
}

#[tokio::test]
async fn a_failed_remote_ingest_preserves_other_producers_reservations() {
    preserves_the_reserved_row_after_remote_ingest(true).await;
}
