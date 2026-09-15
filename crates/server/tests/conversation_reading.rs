mod common;
use common::{json_body, TestBackend};
use serde_json::{json, Value};
use sylph_protocol::SessionId;
use sylph_server::models::{Event, Turn};

async fn fixture(backend: &TestBackend) -> String {
    let id = backend.create_session("mock").await;
    let turn: Turn = serde_json::from_value(json!({"id":"reading-turn","sessionId":id,
        "status":"running","origin":"user","requestText":"Inspect this workspace","createdAtMs":1}))
    .unwrap();
    backend.state.store.create_turn(&turn).await.unwrap();
    let entries = [
        (
            "assistant",
            "I will inspect the files",
            json!({"phase":"commentary"}),
        ),
        ("tool_call", "{}", json!({"input":{"command":"cargo test"}})),
        ("tool_result", "one failed check", json!({"exitCode":1})),
        (
            "assistant",
            "The final answer",
            json!({"phase":"final_answer"}),
        ),
        ("assistant", "An unclassified reply", json!({})),
    ];
    for (i, (kind, text, data)) in entries.into_iter().enumerate() {
        let e = Event {
            id: None,
            session_id: SessionId::decode(&id).unwrap(),
            turn_id: Some(turn.id.clone()),
            seq: i as i32,
            kind: kind.into(),
            phase: Some("completed".into()),
            stream_id: Some(if i == 2 {
                "item-1".into()
            } else {
                format!("item-{i}")
            }),
            span: None,
            interior_state: None,
            name: (i == 1).then(|| "Bash".into()),
            text: text.into(),
            data_json: data.to_string(),
            is_error: i == 2,
            stability: "stable".into(),
            source: "agent".into(),
            arrival_at_ms: i as i64 + 10,
            recorded_at_ms: None,
            created_at_ms: i as i64 + 10,
        };
        backend.state.store.append_events(&[e]).await.unwrap();
    }
    id
}

#[tokio::test]
async fn collapsed_work_keeps_answers_and_unknown_prose_without_loading_process() {
    let backend = TestBackend::start().await;
    let id = fixture(&backend).await;
    let response = backend
        .get_authed(&format!("/api/v1/sessions/{id}/conversation"))
        .await;
    assert_eq!(response.status(), 200);
    let page: Value = json_body(response).await;
    let nodes = page["nodes"].as_array().unwrap();
    assert_eq!(
        nodes
            .iter()
            .map(|n| n["event"]["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["user", "work", "assistant", "assistant"]
    );
    assert_eq!(nodes[1]["failed"], 1);
    assert_eq!(nodes[1]["turn"]["status"], "running");
    assert_eq!(nodes[2]["readingRole"], "answer");
    assert_eq!(nodes[3]["readingRole"], "unknown");
    let process: Value = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{id}/conversation?parent={}",
                nodes[1]["id"].as_str().unwrap()
            ))
            .await,
    )
    .await;
    assert_eq!(
        process["nodes"][0]["event"]["text"],
        "I will inspect the files"
    );
    assert_eq!(process["nodes"][1]["event"]["kind"], "execution");
    assert!(!serde_json::to_string(&process)
        .unwrap()
        .contains("The final answer"));
}

#[tokio::test]
async fn conversation_rejects_invalid_cursors_and_bounds_large_windows() {
    let backend = TestBackend::start().await;
    let id = fixture(&backend).await;
    let r = backend
        .get_authed(&format!("/api/v1/sessions/{id}/conversation?cursor=bad"))
        .await;
    assert_eq!(r.status(), 400);
    let page: Value = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}/conversation?limit=1"))
            .await,
    )
    .await;
    assert_eq!(page["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(page["hasMore"], true);
}

#[tokio::test]
async fn delegation_catalogue_is_paged_without_expanding_any_trajectory() {
    let backend = TestBackend::start().await;
    let id = fixture(&backend).await;
    for i in 0..25 {
        let e = Event {
            id: None,
            session_id: SessionId::decode(&id).unwrap(),
            turn_id: Some("reading-turn".into()),
            seq: 20 + i,
            kind: "subagent".into(),
            phase: Some(if i < 3 { "started" } else { "completed" }.into()),
            stream_id: Some(format!("delegate-{i}")),
            span: None,
            interior_state: None,
            name: Some(format!("Check {i}")),
            text: "Delegate summary".into(),
            data_json: "{}".into(),
            is_error: i == 8,
            stability: "stable".into(),
            source: "agent".into(),
            arrival_at_ms: i as i64 + 30,
            recorded_at_ms: None,
            created_at_ms: i as i64 + 30,
        };
        backend.state.store.append_events(&[e]).await.unwrap();
    }
    let page: Value = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}/delegations?limit=10"))
            .await,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 10);
    assert_eq!(page["counts"]["active"], 3);
    assert_eq!(page["counts"]["finished"], 22);
    assert_eq!(page["hasMore"], true);
    assert_eq!(page["items"][0]["name"], "Check 0");
    let found: Value = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}/delegations?q=Check%2024"))
            .await,
    )
    .await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["target"]["nodeId"], "s:delegate-24");
}

#[tokio::test]
async fn a_presentation_correction_moves_the_same_address_without_losing_its_body() {
    let backend = TestBackend::start().await;
    let id = fixture(&backend).await;
    let outline = backend
        .state
        .store
        .conversation_page(&id, "", None, false, 40)
        .await
        .unwrap();
    let work = outline["nodes"][1]["id"].as_str().unwrap();
    let process = backend
        .state
        .store
        .conversation_page(&id, work, None, false, 40)
        .await
        .unwrap();
    let address = process["nodes"][0]["id"].as_str().unwrap().to_owned();
    let cursor = format!("{}:", outline["snapshot"].as_i64().unwrap() + 1);
    let correction = Event {
        id: None,
        session_id: SessionId::decode(&id).unwrap(),
        turn_id: Some("reading-turn".into()),
        seq: 6,
        kind: "assistant".into(),
        phase: Some("completed".into()),
        stream_id: Some("item-0".into()),
        span: None,
        interior_state: None,
        name: None,
        text: "I will inspect the files".into(),
        data_json: json!({"phase":"final_answer"}).to_string(),
        is_error: false,
        stability: "stable".into(),
        source: "agent".into(),
        arrival_at_ms: 30,
        recorded_at_ms: None,
        created_at_ms: 30,
    };
    backend
        .state
        .store
        .append_events(&[correction])
        .await
        .unwrap();
    let changes = backend
        .state
        .store
        .conversation_page(&id, "", Some(&cursor), true, 40)
        .await
        .unwrap();
    assert!(changes["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == address && n["readingRole"] == "answer"));
    let removals = backend
        .state
        .store
        .conversation_page(&id, work, Some(&cursor), true, 40)
        .await
        .unwrap();
    assert!(removals["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == address && n["removed"] == true));
    let body: Value = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}/nodes/{address}/body"))
            .await,
    )
    .await;
    assert_eq!(body["text"], "I will inspect the files");
}

#[tokio::test]
async fn an_empty_retired_work_summary_cannot_displace_the_latest_answer() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("mock").await;
    let turn:Turn=serde_json::from_value(json!({"id":"retired-work","sessionId":id,"status":"running","origin":"user","requestText":"Read","createdAtMs":1})).unwrap();
    backend.state.store.create_turn(&turn).await.unwrap();
    for (seq, role) in ["commentary", "final_answer"].into_iter().enumerate() {
        let event = Event {
            id: None,
            session_id: SessionId::decode(&id).unwrap(),
            turn_id: Some(turn.id.clone()),
            seq: seq as i32,
            kind: "assistant".into(),
            phase: Some("completed".into()),
            stream_id: Some("reply".into()),
            span: None,
            interior_state: None,
            name: None,
            text: "The answer".into(),
            data_json: json!({"phase":role}).to_string(),
            is_error: false,
            stability: "stable".into(),
            source: "agent".into(),
            arrival_at_ms: seq as i64,
            recorded_at_ms: None,
            created_at_ms: seq as i64,
        };
        backend.state.store.append_events(&[event]).await.unwrap();
    }
    let page = backend
        .state
        .store
        .conversation_page(&id, "", None, false, 1)
        .await
        .unwrap();
    assert_eq!(page["nodes"][0]["event"]["text"], "The answer");
    assert_ne!(page["nodes"][0]["removed"], true);
}
