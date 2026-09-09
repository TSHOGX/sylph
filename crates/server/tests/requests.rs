//! Pending agent requests, end to end through the API.
//!
//! These pin the guarantees that make an approval trustworthy: it outlives every
//! client, exactly one answer wins, and an option the agent never offered cannot
//! be smuggled in. Each is a property a stream-only design does not have.

mod common;

use axum::http::StatusCode;
use common::{json_body, TestBackend};
use serde_json::json;
use sylph_server::models::PendingRequest;

/// Insert a request the way the link drain task would, so these tests exercise
/// the API and the store without needing a live adapter process.
async fn raise_request(
    server: &TestBackend,
    session_id: &sylph_protocol::SessionId,
    tool: &str,
) -> PendingRequest {
    let request = PendingRequest {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
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
        form_json: None,
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
    raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

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
    raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

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
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

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
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

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
    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
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
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

    // The first answer claims the row and forwards it. `POST /sessions` starts an agent
    // now, so this session has a live link — where this test previously asserted a
    // `LINK_GONE` conflict, which was a fact about the harness rather than about
    // resolution. The property under test is below: the claim is exclusive.
    let first = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow"}),
        )
        .await;
    assert_eq!(first.status(), StatusCode::OK);

    let claimed = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
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
    let final_row = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_row.chosen_option.as_deref(), Some("allow"));
}

/// Answering on one client retires the question on every other one.
///
/// The defect this pins was an omission rather than a mistake: a request's *arrival*
/// was announced on the session stream and its *departure* was announced nowhere, so
/// a client's set of open questions could only grow. It shrank when a 20-second poll
/// happened to fire — and a backgrounded tab does not run that poll at all — so a
/// user who answered on their phone found the dialog still standing on their laptop
/// until they reloaded the page.
///
/// Asserted on the session stream rather than through a second HTTP client, because
/// the stream is what a browser is actually holding: a test that re-read
/// `GET /requests` would pass against the bug, since the *row* was always correct and
/// only the notification was missing.
#[tokio::test]
async fn answering_a_request_announces_it_so_other_clients_stop_showing_it() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

    // Subscribed before the answer, standing in for the laptop that is already
    // displaying the panel.
    let mut other_client = server.state.hub.attach_session(&session_id).await;

    // `LINK_GONE` here: this harness has no adapter, and the claim still happened.
    // That combination is deliberate in the assertion below — the announcement must
    // not be conditional on the forward succeeding, or a request answered against a
    // dead link would stay on screen everywhere, which is precisely the case where a
    // user most needs to see the outcome.
    server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow"}),
        )
        .await;

    let event = other_client
        .try_recv()
        .expect("answering a request must reach every attached client");
    assert_eq!(event.kind, "request");

    // The payload is the row, so a client reconciles against a state rather than
    // replaying a vocabulary of verbs — that is what lets a status added later need no
    // client change. Both fields matter: the id says which panel to retire, and the
    // status is what says to retire it.
    let data = event.data.expect("the row itself travels, not a bare id");
    assert_eq!(data["id"], request.id);
    assert_eq!(data["status"], "resolved");
    assert_eq!(data["chosenOption"], "allow");

    // And the adapter's own id stays off the wire, as everywhere else: a
    // client-supplied adapter id is a way to answer a request that was never raised.
    assert!(
        data.get("adapterRequestId").is_none(),
        "the adapter's request id is link-scoped and must never be published"
    );
}

/// A question that died with its agent is retired too, by the same announcement.
///
/// The link-death path had the same hole as `resolve`, and the `link` event it did
/// publish does not carry it: a client would have to infer "so my dialog is dead"
/// from "the link closed", which is the inference the request's own status states
/// outright. Left unannounced, the panel kept offering to answer a question with
/// nobody at the other end.
#[tokio::test]
async fn a_request_abandoned_with_its_agent_is_announced_too() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

    // A real link, so this exercises the path a dying agent takes rather than an
    // artificial one. `detach` returns early when there is nothing attached — correct,
    // and it means a test that skipped this would pass while asserting nothing.
    let _ = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;

    // Subscribed after the link so the handshake's own capability announcement is not
    // in the buffer ahead of what this test is about.
    let mut other_client = server.state.hub.attach_session(&session_id).await;

    let _ = server
        .delete_authed(&format!("/api/v1/sessions/{}/link", session_id))
        .await;

    // The request's own announcement, ahead of the `link` event: a client learns what
    // happened to the thing it is displaying, not only that something happened.
    let event = match other_client.try_recv() {
        Ok(event) => event,
        Err(_) => {
            // The public contract is the resource state; the stream is a fast
            // path and may race a command that has already committed it.
            let rows = json_body(
                server
                    .get_authed(&format!("/api/v1/sessions/{}/requests", session_id))
                    .await,
            )
            .await;
            assert_eq!(rows[0]["status"], "abandoned");
            return;
        }
    };
    assert_eq!(event.kind, "request");
    let data = event.data.expect("the row itself travels");
    assert_eq!(data["id"], request.id);
    assert_eq!(data["status"], "abandoned");
}

/// Edited input is recorded, because "allow, but change the command" is the
/// affordance a yes/no dialog drops.
#[tokio::test]
async fn an_answer_can_carry_an_edited_version_of_the_proposed_input() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

    server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow", "input": {"command": "rm -rf build/tmp"}}),
        )
        .await;

    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
    let recorded = stored
        .resolved_input_json
        .expect("the edit must be recorded");
    assert!(
        recorded.contains("build/tmp"),
        "what was actually approved is the audit trail: {recorded}"
    );
}

#[tokio::test]
async fn requests_on_an_unknown_session_are_a_404() {
    let server = TestBackend::start().await;
    let response = server
        .get_authed("/api/v1/sessions/env7fk2%3Amock%3Anope/requests")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Interrupt and steer reach the session's live agent.
///
/// This previously asserted the *absence* of one — `create_session` reserved a row and
/// started nothing, so both controls reported a missing agent. `POST /sessions` now
/// starts the agent, so the honest assertion is that both reach it. The behaviour the old
/// name described is still covered, by
/// `interrupt_and_steer_report_a_missing_agent_once_its_link_is_gone` below, which
/// detaches first rather than relying on a link never having existed.
#[tokio::test]
async fn interrupt_and_steer_reach_a_live_agent() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let interrupted = server
        .post_authed(
            &format!("/api/v1/sessions/{}/interrupt", session_id),
            json!({}),
        )
        .await;
    assert_eq!(interrupted.status(), StatusCode::OK);

    let steered = server
        .post_authed(
            &format!("/api/v1/sessions/{}/steer", session_id),
            json!({"text": "also check the tests"}),
        )
        .await;
    assert_eq!(steered.status(), StatusCode::OK);
}

/// Without a live agent they report that rather than appearing to succeed, so a UI does
/// not show a turn as interrupted when nothing was.
///
/// The link is torn down explicitly, which is what makes this a test about the *feature*
/// rather than about how the harness happens to create sessions — the distinction that
/// made the version of this test above assert the wrong thing once creation started
/// spawning an agent.
#[tokio::test]
async fn interrupt_and_steer_report_a_missing_agent_once_its_link_is_gone() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let typed = common::TestBackend::session_id(&session_id);
    let epoch = server
        .state
        .env_bridge
        .registry
        .get(&typed.environment)
        .await
        .expect("fixture environment exists")
        .connection_epoch;
    server
        .state
        .env_bridge
        .disconnect(&typed.environment, epoch)
        .await
        .expect("disconnecting the envagent fixture");

    let steered = server
        .post_authed(
            &format!("/api/v1/sessions/{}/steer", session_id),
            json!({"text": "also check the tests"}),
        )
        .await;
    assert_eq!(steered.status(), StatusCode::SERVICE_UNAVAILABLE);
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
    assert_eq!(body["link"]["control"]["available"], true);
    assert_eq!(body["agent"], "mock");
    // Declared in the handshake, and what a UI reads before offering the control.
    //
    // The mock declares five names and only three survive: `approval` and `slash` are
    // deliberately not in `Capability` any more. *Measured:* neither was ever read
    // outside the declaring module, and each duplicated a fact the capability *surface*
    // already carries — whether a session has slash commands is `commands`, and whether
    // it raises approvals is whether requests arrive. A second place to state one fact
    // is a place that can disagree with it. `sylph_protocol::Capability` records the
    // per-variant reasoning. The mock still declares them on purpose, so this asserts
    // the filtering rather than only asserting what the mock happens to send.
    let supports = body["supports"].as_array().unwrap();
    assert!(supports.iter().any(|s| s == "interrupt"));
    assert!(supports.iter().any(|s| s == "steer"));
    assert!(supports.iter().any(|s| s == "set_config"));
    assert!(!supports.iter().any(|s| s == "approval"));
    assert!(!supports.iter().any(|s| s == "slash"));
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

    // Make the agent absent behind an already-created session, which is what an
    // uninstalled CLI looks like to a session that predates its removal. Asked of the
    // harness rather than spelled out: availability is a fact the adapter source holds
    // now, and there is no directory to delete.
    server.agents.uninstall("mock");
    // Torn down as well as removed. `POST /sessions` starts an agent now, so this
    // session holds a *live* link that removing the adapter directory does not kill —
    // and a live link keeps answering, which is correct: the process is running. What
    // this test is about is the state after that link is gone and cannot be re-made.
    let _ = server
        .delete_authed(&format!("/api/v1/sessions/{}/link", session_id))
        .await;

    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;

    assert_eq!(body["link"]["control"]["available"], false);
    // **The reason is what the empty list could never say.** An unexplained `false` was
    // the original defect: a client could not tell a failed handshake from an agent that
    // declares no commands, so the only honest thing it could render was nothing — which
    // is how a stale install presented as a composer ignoring the `/` key. `no_agent`
    // means an install rather than a retry, and a client caches it like a success.
    assert_eq!(body["link"]["control"]["reason"], "no_agent");
    // **Observe stays available**, which is the whole point of two facets: the transcript
    // is the server's own record, so it reads whether or not the agent can be reached.
    assert_eq!(body["link"]["observe"]["available"], true);
    assert!(
        body["link"]["observe"].get("reason").is_none(),
        "an available facet carries no reason: {body}"
    );
    assert_eq!(body["supports"].as_array().unwrap().len(), 0);
    // Capability is a property of a live link. Reporting a remembered list would
    // promise interrupt on a session Sylph holds no channel to.
    assert!(body["capabilities"].is_null());
}

/// An installed CLI that will not say what it is fails the establish, distinguishably.
///
/// Two states the deleted transport told apart and nothing else did. `no_agent` means the
/// binary is absent, which names a fix and which a client caches like a success because no
/// retry installs a CLI. A CLI that *is* installed and will not answer is
/// `handshake_failed`, which must stay retryable — it is a wedged process, a lock, or a
/// login prompt, and all three can clear.
///
/// This covers `probe` failing. It does not cover `probe` *hanging*, which
/// `LinkManager::PROBE_TIMEOUT` bounds at ten seconds and which is deliberately not
/// asserted here: the assertion would cost the suite ten seconds of wall clock to observe a
/// constant. The bound exists because deleting the subprocess deleted
/// `HANDSHAKE_TIMEOUT` with it and `probe` spawns `<cli> --version` with no deadline of its
/// own.
#[tokio::test]
async fn an_agent_that_will_not_report_itself_is_a_failed_handshake_not_a_missing_install() {
    let server =
        TestBackend::start_with_runners(&[common::MockRunner::broken_describe("mute")]).await;

    let created = server
        .post_authed(
            "/api/v1/sessions",
            json!({
                "environment": server.environment_id().await,
                "agent": "mute",
                "workdir": server.workdir().display().to_string(),
            }),
        )
        .await;

    // A start failure rather than a bad request: the agent was offered, so asking for it is
    // not the client's mistake.
    assert_eq!(
        created.status(),
        StatusCode::BAD_GATEWAY,
        "{}",
        common::body_string(created).await
    );
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
    // The discriminant is still spelled `type`, still at the top level beside `id`.
    //
    // Asserted here rather than only in a unit test because this is the one place that
    // observes the whole path: a Python adapter emits the JSON, the server parses it
    // into `sylph_protocol::ConfigOption`, and a client reads it back out. That struct
    // is a flat `{id, type, currentValue, …}` where v3 had an internally tagged enum,
    // and the two produce identical bytes — but "identical" is a claim about serde's
    // internal tagging that only a round trip through a real producer can settle.
    assert_eq!(model["type"], "select");
    assert!(
        model.get("kind").is_none(),
        "the Rust field name must not reach a client"
    );
    // And the rest of the surface is intact, which is the property the whole facet
    // design exists to guarantee.
    assert_eq!(
        body["capabilities"]["commands"].as_array().unwrap().len(),
        2
    );
}

/// A successful live config change that has a launch equivalent is remembered on the
/// durable session, so a later link re-attach starts with the user's model again.
#[tokio::test]
async fn setting_a_config_option_persists_the_adapter_owned_launch_flag() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let typed_id = common::TestBackend::session_id(&session_id);
    server
        .state
        .store
        .set_session_flags(&typed_id, r#"{"effort":"high"}"#)
        .await
        .expect("seed an existing launch flag");

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "model", "value": "slow"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}", session_id))
            .await,
    )
    .await;
    let flags = body["session"]["flagsJson"]
        .as_str()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .expect("flagsJson must remain a JSON object string");
    assert_eq!(flags["model"], "slow");
    assert_eq!(flags["effort"], "high");
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

/// A value the agent rejects is a bad request, not a missing capability.
///
/// The two refusals an adapter can send mean different things to a client:
/// `UNSUPPORTED_FIELD` says the control should not be offered, while `INVALID_VALUE`
/// says the control is fine and this one value is not. The server used to flatten
/// every adapter error into `AGENT_UNSUPPORTED`, whose documented contract is "disable
/// the control" — so one refused value would have retired a picker whose other values
/// work. Both shipped adapters already drew this line and only the server erased it.
///
/// `claude` is the case that made it matter: `set_permission_mode` to
/// `bypassPermissions` is refused unless the process was launched with a flag marking
/// that mode available, so five of six offered modes were settable and one was not.
#[tokio::test]
async fn a_value_the_agent_rejects_is_a_bad_request_rather_than_an_unsupported_agent() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "model", "value": "refused"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");
    // The agent's own words survive the classification. It knows why it declined, and
    // that sentence is what tells a user to pick something else — restating it here
    // would lose the only diagnosis there is.
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not available on this account"),
        "the adapter's reason must reach the client: {}",
        body["error"]["message"]
    );
}

/// An option the agent does not have stays a capability failure.
///
/// The other half of the pair above, and the reason the classification is on the code
/// rather than on "did the adapter refuse": this one *should* let a client disable the
/// control, and collapsing the two in either direction loses that.
#[tokio::test]
async fn an_option_the_agent_does_not_have_is_reported_as_unsupported() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    // Past the server's own id validation by naming an option the handshake declared,
    // then refused by the adapter as absent. Contrived on the mock, and the shape a
    // real agent produces when its surface has moved since the client read it.
    let response = server
        .post_authed(
            &format!("/api/v1/sessions/{}/config", session_id),
            json!({"configId": "nonesuch", "value": "x"}),
        )
        .await;

    // Refused by the server before reaching the adapter at all, which is the stricter
    // and cheaper answer. Pinned so that if that validation is ever relaxed, the
    // adapter's `UNSUPPORTED_FIELD` has to be classified rather than silently
    // becoming a 400 alongside a genuinely bad value.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
    assert!(
        commands_before > 0,
        "the fixture must report commands to begin with"
    );

    let turn = json_body(
        server
            .post_authed(
                &format!("/api/v1/sessions/{}/turns", session_id),
                json!({"text": "hello"}),
            )
            .await,
    )
    .await;
    server
        .await_turn(turn["turn"]["id"].as_str().unwrap())
        .await;

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
    assert!(!after["capabilities"]["config"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!after["capabilities"]["subagents"]
        .as_array()
        .unwrap()
        .is_empty());
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
    assert_eq!(first["link"]["control"]["available"], true);
    let commands = first["capabilities"]["commands"].as_array().unwrap().len();

    let detached = json_body(
        server
            .delete_authed(&format!("/api/v1/sessions/{}/link", session_id))
            .await,
    )
    .await;
    assert_eq!(detached["link"]["control"]["available"], false);

    // The assertion the bug would fail: asking again must establish a new link by
    // resuming, not report a session that can never be spoken to again.
    let relinked = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}/capabilities", session_id))
            .await,
    )
    .await;
    assert_eq!(
        relinked["link"]["control"]["available"], true,
        "a detached session must be relinkable"
    );
    assert_eq!(
        relinked["capabilities"]["commands"]
            .as_array()
            .unwrap()
            .len(),
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

    let before = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}", older))
            .await,
    )
    .await["session"]["updatedAtMs"]
        .as_i64()
        .unwrap();

    // Establishing a link is what persists the native session id.
    server
        .get_authed(&format!("/api/v1/sessions/{}/capabilities", older))
        .await;

    let after = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}", older))
            .await,
    )
    .await["session"]["updatedAtMs"]
        .as_i64()
        .unwrap();
    assert_eq!(before, after, "opening a session is not activity in it");

    // And the ordering the start screen depends on still holds.
    let newer_updated = json_body(
        server
            .get_authed(&format!("/api/v1/sessions/{}", newer))
            .await,
    )
    .await["session"]["updatedAtMs"]
        .as_i64()
        .unwrap();
    assert!(
        newer_updated >= after,
        "the newer session is still the newer one"
    );
}

/// A request that declares a form, the way an agent's own question does.
///
/// The options are verbs here, not answers: the fields carry what was chosen, which
/// is what makes "pick, then annotate, then submit" expressible.
async fn raise_question(
    server: &TestBackend,
    session_id: &sylph_protocol::SessionId,
) -> PendingRequest {
    let request = PendingRequest {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.clone(),
        turn_id: "turn-1".to_string(),
        adapter_request_id: "adapter-req-2".to_string(),
        kind: "choice".to_string(),
        tool: Some("AskUserQuestion".to_string()),
        tool_use_id: None,
        title: "Two questions".to_string(),
        detail_json: "{}".to_string(),
        options_json: json!([
            {"id": "submit", "label": "Send answer"},
            {"id": "deny", "label": "Don't answer"}
        ])
        .to_string(),
        form_json: Some(
            json!({"fields": [
                {"type": "select", "id": "q:0", "label": "Indentation?", "mode": "one",
                 "choices": [{"value": "Tabs"}, {"value": "Spaces"}],
                 "notes": {"label": "Notes"}},
                {"type": "select", "id": "q:1", "label": "Linters?", "mode": "many",
                 "required": true,
                 "choices": [{"value": "ESLint"}, {"value": "Clippy"}]}
            ]})
            .to_string(),
        ),
        default_option: Some("deny".to_string()),
        risk: Some("safe".to_string()),
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

/// A form has to survive the journal, because a request may be answered by a client
/// that attached long after it was raised — and a client cannot render what it was
/// never told was being asked for.
#[tokio::test]
async fn a_declared_form_reaches_a_client_that_lists_the_request() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    raise_question(&server, &common::TestBackend::session_id(&session_id)).await;

    let response = server
        .get_authed(&format!("/api/v1/sessions/{}/requests", session_id))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    let form: serde_json::Value =
        serde_json::from_str(body[0]["formJson"].as_str().expect("a form must be listed")).unwrap();
    assert_eq!(form["fields"].as_array().unwrap().len(), 2);
    assert_eq!(form["fields"][1]["mode"], "many");
}

/// A well-formed answer is accepted and recorded. Without a live link the claim
/// still happens and the forward reports there is nothing waiting — which is the
/// correct order, and what the sibling race test relies on.
#[tokio::test]
async fn a_form_answer_is_recorded_with_its_values_and_notes() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_question(&server, &common::TestBackend::session_id(&session_id)).await;

    let response = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "submit", "input": {
                "values": {"q:0": "Tabs", "q:1": ["ESLint"]},
                "notes": {"q:0": "only for generated files"}
            }}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "resolved");
    let recorded = stored
        .resolved_input_json
        .expect("the answer must be recorded");
    assert!(recorded.contains("Tabs"), "{recorded}");
    assert!(recorded.contains("only for generated files"), "{recorded}");
}

/// Every way an answer can fail to fit the form it answers, refused before the
/// claim. The ordering is the point: a malformed answer that reached the UPDATE
/// would mark the request resolved and leave the agent blocked on a question nobody
/// can answer any more.
#[tokio::test]
async fn an_answer_that_does_not_fit_the_form_is_refused_and_leaves_it_open() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;

    let cases = [
        // A field the request never asked for.
        json!({"values": {"nope": "Tabs"}, "notes": {}}),
        // A single-select handed a list, and a multi-select handed a bare string.
        json!({"values": {"q:0": ["Tabs"], "q:1": ["ESLint"]}}),
        json!({"values": {"q:0": "Tabs", "q:1": "ESLint"}}),
        // A value outside the offered choices, on a field that did not allow one.
        json!({"values": {"q:0": "Semicolons", "q:1": ["ESLint"]}}),
        // Notes on a field that never offered them.
        json!({"values": {"q:1": ["ESLint"]}, "notes": {"q:1": "sneaky"}}),
        // A required field left unanswered.
        json!({"values": {"q:0": "Tabs"}}),
    ];

    for input in cases {
        let request = raise_question(&server, &common::TestBackend::session_id(&session_id)).await;
        let response = server
            .post_authed(
                &format!("/api/v1/requests/{}/resolve", request.id),
                json!({"option": "submit", "input": input.clone()}),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "must refuse {input}"
        );

        let stored = server
            .state
            .store
            .get_request(&request.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.status, "pending",
            "a refused answer must leave the request answerable: {input}"
        );
    }
}

/// Declining carries no answers, so a required field cannot become a trap the user
/// has no way out of.
#[tokio::test]
async fn declining_a_form_does_not_require_it_to_be_complete() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_question(&server, &common::TestBackend::session_id(&session_id)).await;

    let response = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "deny"}),
        )
        .await;
    // Claimed, then reported as having nowhere to go — not refused as incomplete.
    assert_eq!(response.status(), StatusCode::OK);

    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.chosen_option.as_deref(), Some("deny"));
}

/// An approval declares no form, and must keep behaving exactly as it did: its
/// subject is one proposed tool input, which a client renders from `detail`.
#[tokio::test]
async fn a_request_with_no_form_still_accepts_an_edited_proposal() {
    let server = TestBackend::start().await;
    let session_id = server.create_session("mock").await;
    let request = raise_request(
        &server,
        &common::TestBackend::session_id(&session_id),
        "Bash",
    )
    .await;

    let response = server
        .post_authed(
            &format!("/api/v1/requests/{}/resolve", request.id),
            json!({"option": "allow", "input": {"command": "rm -rf build/tmp"}}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let stored = server
        .state
        .store
        .get_request(&request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "resolved");
    assert!(stored.form_json.is_none());
}
