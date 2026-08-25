//! Pending agent requests, end to end through the API.
//!
//! These pin the guarantees that make an approval trustworthy: it outlives every
//! client, exactly one answer wins, and an option the agent never offered cannot
//! be smuggled in. Each is a property a stream-only design does not have.

mod common;

use axum::http::StatusCode;
use common::{json_body, TestBackend};
use serde_json::json;
use sylph_web_backend::models::PendingRequest;

/// Insert a request the way the link drain task would, so these tests exercise
/// the API and the store without needing a live adapter process.
async fn raise_request(server: &TestBackend, session_id: &str, tool: &str) -> PendingRequest {
    let request = PendingRequest {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        turn_id: "turn-1".to_string(),
        adapter_request_id: "adapter-req-1".to_string(),
        kind: "approval".to_string(),
        tool: Some(tool.to_string()),
        tool_use_id: Some("toolu_01".to_string()),
        title: format!("Allow {}?", tool),
        detail_json: json!({"command": "rm -rf build"}).to_string(),
        options_json: json!([
            {"id": "allow", "label": "Allow once"},
            {"id": "deny", "label": "Deny"}
        ])
        .to_string(),
        default_option: Some("deny".to_string()),
        risk: Some("destructive".to_string()),
        allow_persistent: true,
        expires_at_ms: None,
        status: "pending".to_string(),
        chosen_option: None,
        resolved_input_json: None,
        resolved_by: None,
        created_at_ms: 1_700_000_000_000,
        resolved_at_ms: None,
    };
    server
        .state
        .store
        .create_pending_request(&request)
        .await
        .expect("request must persist")
}

#[tokio::test]
async fn a_pending_request_is_listed_with_what_the_agent_proposed() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    raise_request(&server, &session_id, "Bash").await;

    let response = server
        .get_authed(&format!("/api/v1/sessions/{}/requests", session_id))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let requests = body.as_array().expect("an array of requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["kind"], "approval");
    assert_eq!(requests[0]["tool"], "Bash");
    assert_eq!(requests[0]["risk"], "destructive");
    // The proposed input has to reach the client, or an approval dialog can only
    // say "allow Bash?" without saying which command.
    assert!(requests[0]["detailJson"]
        .as_str()
        .unwrap()
        .contains("rm -rf build"));
}

/// The adapter's own request id must not be exposed. A client that could send it
/// could answer a request that was never raised.
#[tokio::test]
async fn the_adapter_request_id_is_never_serialized_to_a_client() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    raise_request(&server, &session_id, "Bash").await;

    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/requests", session_id))
            .await,
    )
    .await;

    let rendered = body.to_string();
    assert!(
        !rendered.contains("adapter-req-1"),
        "the adapter's id leaked to the client: {rendered}"
    );
}

/// The durability guarantee. A request survives the server restarting — except
/// that a restart also proves no link survived, so it is abandoned rather than
/// left pending, because nothing can answer it any more.
#[tokio::test]
async fn a_restart_abandons_pending_requests_rather_than_leaving_them_answerable() {
    let mut server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(&server, &session_id, "Bash").await;

    server.restart().await;

    // No longer listed as open: the agent that raised it is gone.
    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/requests", session_id))
            .await,
    )
    .await;
    assert_eq!(body.as_array().unwrap().len(), 0);

    // But the row is kept, so the timeline can explain why the turn stopped
    // instead of showing an unexplained gap.
    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .expect("the row must be retained");
    assert_eq!(stored.status, "abandoned");
    assert!(stored.resolved_at_ms.is_some());
}

/// An option the agent never offered must be refused. Reaching the adapter with
/// an unrecognized string leaves its handling undefined, and for an approval
/// "undefined" must never be able to mean "allow".
#[tokio::test]
async fn an_option_the_agent_never_offered_is_refused() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(&server, &session_id, "Bash").await;

    let response = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow_everything_forever"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");

    // Still open: a refused answer must not consume the request.
    let stored = server.state.store.get_request(&request.id).await.unwrap().unwrap();
    assert_eq!(stored.status, "pending");
}

#[tokio::test]
async fn resolving_an_unknown_request_is_a_404() {
    let server = TestBackend::start().await;
    let response = server
        .post_authed(
            "/api/v1/requests/does-not-exist/resolve",
            json!({"option": "allow"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "REQUEST_NOT_FOUND");
}

/// First resolution wins, and the loser is told it lost rather than silently
/// succeeding. Two people answering one approval is a defined outcome.
#[tokio::test]
async fn a_second_answer_to_one_request_is_a_conflict() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(&server, &session_id, "Bash").await;

    // No link exists in this harness, so the first answer claims the row and then
    // reports that nothing is waiting for it. That is the correct order: the
    // claim is what must be exclusive.
    let first = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow"}),
        )
        .await;
    assert_eq!(first.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(first).await["error"]["code"], "LINK_GONE");

    let claimed = server.state.store.get_request(&request.id).await.unwrap().unwrap();
    assert_eq!(claimed.status, "resolved");
    assert_eq!(claimed.chosen_option.as_deref(), Some("allow"));

    // The second answer finds it already claimed.
    let second = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "deny"}),
        )
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(second).await["error"]["code"],
        "REQUEST_ALREADY_RESOLVED"
    );

    // The first decision stands. An approval overwritten by a later "deny" would
    // be bad; overwritten by a later "allow" would be a security hole.
    let final_row = server.state.store.get_request(&request.id).await.unwrap().unwrap();
    assert_eq!(final_row.chosen_option.as_deref(), Some("allow"));
}

/// Edited input is recorded, because "allow, but change the command" is the
/// affordance a yes/no dialog drops.
#[tokio::test]
async fn an_answer_can_carry_an_edited_version_of_the_proposed_input() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(&server, &session_id, "Bash").await;

    server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow", "input": {"command": "rm -rf build/tmp"}}),
        )
        .await;

    let stored = server.state.store.get_request(&request.id).await.unwrap().unwrap();
    let recorded = stored.resolved_input_json.expect("the edit must be recorded");
    assert!(
        recorded.contains("build/tmp"),
        "what was actually approved is the audit trail: {recorded}"
    );
}

#[tokio::test]
async fn requests_on_an_unknown_session_are_a_404() {
    let server = TestBackend::start().await;
    let response = server.get_authed("/api/v1/sessions/nope/requests").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Interrupt and steer need a live agent. Without one they report that rather
/// than appearing to succeed, so a UI does not show a turn as interrupted when
/// nothing was.
#[tokio::test]
async fn interrupt_and_steer_report_a_missing_agent_rather_than_succeeding() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let interrupted = server
        .post_authed(&format!("/api/v1/sessions/{}/interrupt", session_id), json!({}))
        .await;
    assert_eq!(interrupted.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(interrupted).await["error"]["code"], "LINK_GONE");

    let steered = server
        .post_authed(
            &format!("/api/v1/sessions/{}/steer", session_id),
            json!({"text": "also check the tests"}),
        )
        .await;
    assert_eq!(steered.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn steering_with_empty_text_is_refused_before_reaching_the_agent() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/steer", session_id),
            json!({"text": "   "}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// Capabilities come from a live handshake, so asking for them establishes a link
/// and reports what the agent actually said — not a descriptor read from disk.
#[tokio::test]
async fn capabilities_are_read_from_the_live_agent() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["attached"], true);
    assert_eq!(body["agent"], "mock");
    // Declared in the handshake, and what a UI reads before offering the control.
    let supports = body["supports"].as_array().unwrap();
    assert!(supports.iter().any(|s| s == "interrupt"));
    assert!(supports.iter().any(|s| s == "approval"));
    // The command list is the session's own, learned at open rather than declared
    // in a static descriptor.
    assert_eq!(body["capabilities"]["commands"][0]["name"], "noop");
}

/// An agent with no adapter installed reports detached rather than failing. The
/// session is still valid and its history still readable.
#[tokio::test]
async fn capabilities_report_detached_when_the_agent_has_no_adapter() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    // Remove the adapter behind an already-created session, which is what an
    // uninstalled agent looks like to a session that predates its removal. Asked
    // of the harness rather than spelled out: a literal directory name here is a
    // second place the layout is written down, and it silently stops removing
    // anything the moment that layout changes — leaving a test that asserts the
    // detached path while never producing it.
    std::fs::remove_dir_all(server.agents_dir().join("mock")).unwrap();

    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;

    assert_eq!(body["attached"], false);
    assert_eq!(body["supports"].as_array().unwrap().len(), 0);
    // Capability is a property of a live link. Reporting a remembered list would
    // promise interrupt on a session with no process to interrupt.
    assert!(body["capabilities"].is_null());
}

/// Typed session config: what a control command resolves against.
///
/// `/model` sent as prompt text comes back as a paragraph, because a headless CLI
/// has no picker to open. So the option is state a client can render, and choosing
/// one is a typed set rather than prose.
#[tokio::test]
async fn setting_a_config_option_answers_with_the_refreshed_surface() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "model", "value": "slow"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let config = body["capabilities"]["config"].as_array().unwrap();
    let model = config.iter().find(|o| o["id"] == "model").unwrap();
    // The response is the new surface, not an acknowledgement: setting one option can
    // change another's choices, so a client that trusted its own optimistic value
    // would drift from the agent exactly when it matters.
    assert_eq!(model["currentValue"], "slow");
    // And the rest of the surface is intact, which is the property the whole facet
    // design exists to guarantee.
    assert_eq!(body["capabilities"]["commands"].as_array().unwrap().len(), 2);
}

/// An option the agent never offered is refused before it reaches the adapter.
///
/// The same reasoning as a request option: an unrecognized id reaches the vendor CLI
/// as an unknown setting, and what that does there is undefined.
#[tokio::test]
async fn a_config_option_the_agent_never_offered_is_refused() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "nonesuch", "value": "x"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");
}

/// A control command is answered from typed state and must never create a turn.
///
/// Asserted through the turn list rather than the response, because "did not run a
/// turn" is the actual promise: a control command that quietly started one would put
/// a paragraph about models into the transcript.
#[tokio::test]
async fn setting_config_does_not_create_a_turn() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "model", "value": "slow"}),
        )
        .await;

    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/turns", session_id))
            .await,
    )
    .await;
    assert_eq!(body["turns"].as_array().unwrap().len(), 0);
}

/// A steer that names a turn which is no longer running is refused.
///
/// Without the precondition the text is delivered into whatever turn is running now:
/// the request succeeds, nothing errors, and the message lands in the wrong
/// conversation — a failure far harder to notice than a 409.
#[tokio::test]
async fn a_steer_naming_a_finished_turn_is_a_conflict() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/steer", session_id),
            json!({"text": "actually, stop", "turnId": "a-turn-that-never-ran"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "TURN_MOVED");
}

/// The regression this protocol version exists for, end to end.
///
/// `claude` emits its `system/init` once per *turn*, and the adapter turns that into
/// a capability update carrying only the permission mode. In v2 the server assigned
/// the whole surface from it, so the first turn of every session erased the project's
/// 46 slash commands and the composer's menu stopped opening — a symptom two layers
/// away from its cause.
///
/// Driven through a real link and a real turn rather than by calling `apply_to`
/// directly: the unit test pins the merge, and this pins that the path a turn
/// actually takes goes through it.
#[tokio::test]
async fn a_turn_does_not_erase_the_session_command_list() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let before = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;
    let commands_before = before["capabilities"]["commands"].as_array().unwrap().len();
    assert!(commands_before > 0, "the fixture must report commands to begin with");

    let turn = json_body(
        server
            .post_authed(
                &format!("/api/v1/sessions/{}/turns", session_id),
                json!({"text": "hello"}),
            )
            .await,
    )
    .await;
    server.await_turn(turn["turn"]["id"].as_str().unwrap()).await;

    let after = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;
    assert_eq!(
        after["capabilities"]["commands"].as_array().unwrap().len(),
        commands_before,
        "a turn must not change what the agent can be asked to do"
    );
    assert!(!after["capabilities"]["config"].as_array().unwrap().is_empty());
    assert!(!after["capabilities"]["subagents"].as_array().unwrap().is_empty());
}

/// A session survives losing its link, which is the ordinary path rather than an
/// error path: links idle out after thirty minutes, die with the server, and can be
/// detached deliberately.
///
/// The turn after any of those has to establish a fresh link by resuming the agent's
/// native session id — and `open` must answer with that id, because the server treats
/// a null as an unresumable session and refuses the link. The claude adapter reported
/// null on exactly this branch, so a session became permanently unlinkable the moment
/// its first link went away. Nothing caught it, because every test created a session
/// and used it immediately.
#[tokio::test]
async fn a_session_relinks_after_its_link_is_torn_down() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let first = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;
    assert_eq!(first["attached"], true);
    let commands = first["capabilities"]["commands"].as_array().unwrap().len();

    let detached = json_body(
        server
            .delete_authed(&format!("/api/v1/sessions/{}/link", session_id))
            .await,
    )
    .await;
    assert_eq!(detached["attached"], false);

    // The assertion the bug would fail: asking again must establish a new link by
    // resuming, not report a session that can never be spoken to again.
    let relinked = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;
    assert_eq!(relinked["attached"], true, "a detached session must be relinkable");
    assert_eq!(
        relinked["capabilities"]["commands"].as_array().unwrap().len(),
        commands,
        "a resumed link reports the same surface"
    );
}

/// Opening a session must not make it look like the most recently worked-in one.
///
/// `updated_at_ms` orders sessions by *user activity* — it is what the start screen's
/// "pick up where you left off" and the sidebar's sort both mean. Establishing a link
/// persists the agent's native session id, and that write used to bump the timestamp,
/// so merely *reading* an older session reordered it above a newer one. It surfaced as
/// a start-screen spec opening the wrong session, which reads as a UI bug.
#[tokio::test]
async fn establishing_a_link_does_not_reorder_a_session_as_recent() {
    let server = TestBackend::start().await;
    let older = server.create_session("mock").await;
    let newer = server.create_session("mock").await;

    let before = json_body(server.get_authed(&format!("/api/v1/sessions/{}", older)).await)
        .await["session"]["updatedAtMs"]
        .as_i64()
        .unwrap();

    // Establishing a link is what persists the native session id.
    server
        .get_authed(&format!("/api/v1/sessions/{}/capabilities", older))
        .await;

    let after = json_body(server.get_authed(&format!("/api/v1/sessions/{}", older)).await)
        .await["session"]["updatedAtMs"]
        .as_i64()
        .unwrap();
    assert_eq!(before, after, "opening a session is not activity in it");

    // And the ordering the start screen depends on still holds.
    let newer_updated =
        json_body(server.get_authed(&format!("/api/v1/sessions/{}", newer)).await).await
            ["session"]["updatedAtMs"]
            .as_i64()
            .unwrap();
    assert!(newer_updated >= after, "the newer session is still the newer one");
}
