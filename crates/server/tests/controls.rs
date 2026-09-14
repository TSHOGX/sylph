mod common;

use common::{json_body, MockRunner, TestBackend};
use serde_json::json;

#[tokio::test]
async fn refused_steer_remains_in_ledger_without_a_transcript_message() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session = backend.create_session("mock").await;
    let sent = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{session}/turns"),
                json!({"text":"start"}),
            )
            .await,
    )
    .await;
    let turn = sent["turn"]["id"].as_str().unwrap();
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session}/steer"),
            json!({"text":"refuse this steer", "turnId": turn, "idempotencyKey":"refusal"}),
        )
        .await;
    assert!(!response.status().is_success());
    let sid = sylph_protocol::SessionId::decode(&session).unwrap();
    let request = backend
        .state
        .store
        .find_control_request_by_idempotency(&sid, "refusal")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.state, "rejected");
    let history = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    assert!(history["turns"].is_array(), "{history}");
    assert!(
        !history.to_string().contains("refuse this steer"),
        "{history}"
    );
}

#[tokio::test]
async fn vendor_completion_waits_for_the_inflight_receipt_before_sealing_history() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session = backend.create_session("mock").await;
    let sent = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{session}/turns"),
                json!({"text":"start"}),
            )
            .await,
    )
    .await;
    let turn = sent["turn"]["id"].as_str().unwrap();
    let path = format!("/api/v1/sessions/{session}/steer");
    let steer = backend.post_authed(
        &path,
        json!({"text":"settle before receipt", "turnId":turn}),
    );
    let observe = async {
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let row = backend.state.store.get_turn(turn).await.unwrap().unwrap();
        assert!(
            !row.is_terminal(),
            "a pending receipt must prevent sealing: {}",
            row.status
        );
    };
    let (response, ()) = tokio::join!(steer, observe);
    assert!(
        response.status().is_success(),
        "{}",
        json_body(response).await
    );
    backend.await_turn(turn).await;
    backend.await_consolidation(turn).await;
    let history = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    assert!(
        history.to_string().contains("settle before receipt"),
        "{history}"
    );
}

#[tokio::test]
async fn rejected_send_does_not_render_an_undelivered_opening_prompt() {
    let backend = TestBackend::start().await;
    let session = backend.create_session("mock").await;
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session}/turns"),
            json!({"text":"refuse this input"}),
        )
        .await;
    assert!(!response.status().is_success());
    let history = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    assert!(history["turns"].is_array(), "{history}");
    assert!(
        !history.to_string().contains("refuse this input"),
        "{history}"
    );
}

#[tokio::test]
async fn native_queue_receipt_has_no_target_turn_or_transcript_message() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session = backend.create_session("mock").await;
    let sent = backend
        .post_authed(
            &format!("/api/v1/sessions/{session}/turns"),
            json!({"text":"start"}),
        )
        .await;
    assert!(sent.status().is_success());
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session}/native-queue"),
            json!({"text":"native follow-up", "priority":false, "idempotencyKey":"native"}),
        )
        .await;
    assert!(response.status().is_success());
    let request = json_body(response).await;
    assert_eq!(request["state"], "confirmed");
    assert_eq!(request["kind"], "native_queue");
    assert!(request["targetTurnId"].is_null());
    let history = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    assert!(history["turns"].is_array(), "{history}");
    assert!(
        !history.to_string().contains("native follow-up"),
        "{history}"
    );
}

#[tokio::test]
async fn envagent_refuses_a_different_turn_generation_even_when_vendor_would_accept() {
    use sylph_protocol::{CommandOutcome, EnvironmentCommand, IdempotencyPolicy, SessionId};
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let id = backend.create_session("mock").await;
    let sent = backend
        .post_authed(
            &format!("/api/v1/sessions/{id}/turns"),
            json!({"text":"start"}),
        )
        .await;
    assert!(sent.status().is_success());
    let session_id = SessionId::decode(&id).unwrap();
    let outcome = backend
        .state
        .env_bridge
        .remote_session_command(
            &session_id,
            EnvironmentCommand::Steer {
                session_id: session_id.clone(),
                text: "wrong generation".into(),
                expected_turn_id: Some("stale-generation".into()),
            },
            IdempotencyPolicy::AtMostOnce,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Rejected { code, .. } if code == "TURN_MOVED"));
}

#[tokio::test]
async fn live_history_preserves_unsealed_block_identity_and_stability() {
    use sylph_protocol::{Event, EventKind, Phase, SessionId};
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session = backend.create_session("mock").await;
    let sent = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{session}/turns"),
                json!({"text":"start"}),
            )
            .await,
    )
    .await;
    let turn = sent["turn"]["id"].as_str().unwrap();
    let mut event = Event::new(EventKind::Thinking, Some(Phase::Delta), 1234);
    event.stream_id = Some("unsealed-reasoning".into());
    event.text = "live prefix".into();
    let mut row = sylph_server::events::StreamEvent::from_protocol(&event)
        .to_row(&SessionId::decode(&session).unwrap(), turn);
    row.seq = 999;
    backend.state.store.append_event(&row).await.unwrap();
    let history = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await;
    let block = history["turns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["events"].as_array().unwrap())
        .find(|event| event["streamId"] == "unsealed-reasoning")
        .unwrap();
    assert_eq!(block["phase"], "delta");
    assert_eq!(block["stability"], "ephemeral");
    assert_eq!(block["text"], "live prefix");
}

#[tokio::test]
async fn a_lost_receipt_remains_unconfirmed_without_claiming_vendor_refusal() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session = backend.create_session("mock").await;
    let sent = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{session}/turns"),
                json!({"text":"start"}),
            )
            .await,
    )
    .await;
    let turn = sent["turn"]["id"].as_str().unwrap();
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session}/steer"),
            json!({"text":"lose this receipt", "turnId":turn, "idempotencyKey":"lost"}),
        )
        .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let sid = sylph_protocol::SessionId::decode(&session).unwrap();
    let request = backend
        .state
        .store
        .find_control_request_by_idempotency(&sid, "lost")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.state, "unconfirmed");
    assert!(request.vendor_receipt.is_none());
}
