mod common;

use common::{json_body, TestBackend};
use sylph_protocol::SessionId;
use sylph_server::models::{Event, Turn};

async fn seed_turn(backend: &TestBackend, session: &str, id: &str, text: &str) {
    let session_id = SessionId::decode(session).unwrap();
    backend
        .state
        .store
        .create_turn(&Turn {
            id: id.into(),
            session_id: session_id.clone(),
            status: "completed".into(),
            origin: "user".into(),
            origin_subject: None,
            request_text: String::new(),
            command_invocation_id: None,
            idempotency_key: None,
            error_code: None,
            error_message: None,
            close_reason: None,
            created_at_ms: 1000,
            started_at_ms: Some(1000),
            completed_at_ms: Some(1001),
            stopped_at_ms: None,
        })
        .await
        .unwrap();
    if !text.is_empty() {
        backend
            .state
            .store
            .append_event(&Event {
                id: None,
                session_id,
                turn_id: Some(id.into()),
                seq: 0,
                kind: "tool_result".into(),
                phase: Some("completed".into()),
                stream_id: Some("read-1".into()),
                span: None,
                interior_state: None,
                name: Some("Read".into()),
                text: text.into(),
                data_json: "{}".into(),
                is_error: false,
                stability: "stable".into(),
                source: "agent".into(),
                arrival_at_ms: 1001,
                recorded_at_ms: Some(1001),
                created_at_ms: 1001,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn equal_millisecond_turns_page_without_loss_or_repeats() {
    let backend = TestBackend::start().await;
    let session = backend.create_session("mock").await;
    for id in ["turn-a", "turn-b", "turn-c"] {
        seed_turn(&backend, &session, id, "").await;
    }
    let mut cursor = None;
    let mut found = Vec::new();
    for _ in 0..4 {
        let path = format!(
            "/api/v1/sessions/{session}/turns?limit=1{}",
            cursor
                .as_ref()
                .map(|cursor| format!("&cursor={cursor}"))
                .unwrap_or_default()
        );
        let page = json_body(backend.get_authed(&path).await).await;
        found.extend(
            page["turns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|turn| turn["turn"]["id"].as_str().unwrap().to_string()),
        );
        cursor = page["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found, ["turn-c", "turn-b", "turn-a"]);
}

#[tokio::test]
async fn history_preview_does_not_hydrate_a_missing_large_body() {
    let backend = TestBackend::start().await;
    let session = backend.create_session("mock").await;
    let text = "inspectable body ".repeat(150_000);
    seed_turn(&backend, &session, "large-turn", &text).await;
    sylph_server::storage::history::consolidate_turn(
        &backend.state.store,
        &backend.state.history,
        "large-turn",
    )
    .await
    .unwrap();
    backend
        .state
        .history
        .blobs()
        .remove(blake3::hash(text.as_bytes()).to_hex().as_ref())
        .await
        .unwrap();
    let page = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    let event = &page["turns"][0]["events"][0];
    assert!(
        event["text"].as_str().unwrap().len()
            <= sylph_server::storage::history::BLOCK_PREVIEW_BYTES
    );
    assert_eq!(event["bodyRef"]["textBytes"], text.len());
}
