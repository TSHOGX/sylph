//! The production envagent path admits capture bytes before exposing normalized reads.
mod common;
use axum::http::StatusCode;
use common::{json_body, FixtureAgents, TestBackend};
use serde_json::json;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn first_command_uses_native_identity_and_keeps_the_submitted_revision_immutable() {
    let backend = TestBackend::start().await;
    let draft = backend.prepare(backend.workdir().to_str().unwrap()).await;
    let list = json_body(backend.get_authed("/api/v1/sessions").await).await;
    assert!(list["sessions"].as_array().unwrap().is_empty());
    let url = format!(
        "/api/v1/sessions/draft/{}/commands",
        draft["draftId"].as_str().unwrap()
    );
    let input = json!({"text":"exact request\r\n你好","idempotencyKey":"same"});
    let first = backend.post_authed(&url, input.clone()).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first = json_body(first).await;
    let session = first["session"]["id"].as_str().unwrap();
    assert!(session.contains(":codex:fixture-native-"));
    assert_eq!(
        first["session"]["traceAgentRef"],
        sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(session)).encode()
    );
    let retry = json_body(backend.post_authed(&url, input).await).await;
    assert_eq!(
        retry["command"]["acceptance"]["commandId"],
        first["command"]["acceptance"]["commandId"]
    );
    assert_eq!(backend.source.sends.load(Ordering::SeqCst), 1);
    {
        let inputs = backend.source.inputs.lock().unwrap();
        assert_eq!(inputs[0].text, "exact request\r\n你好");
    }
    assert!(backend.state.trace.evidence().evidence_cut().await.unwrap() > 0);
    backend.state.trace.project_pending().await.unwrap();
    let response = backend
        .get_authed(&format!(
            "/api/v1/trace/agents/{}",
            first["session"]["traceAgentRef"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::body_string(response).await
    );
}

#[tokio::test]
async fn rejected_initial_input_never_creates_a_fake_session() {
    let backend = TestBackend::with_source(FixtureAgents {
        refuse_send: true,
        ..Default::default()
    })
    .await;
    let draft = backend.prepare(backend.workdir().to_str().unwrap()).await;
    let response = backend
        .post_authed(
            &format!(
                "/api/v1/sessions/draft/{}/commands",
                draft["draftId"].as_str().unwrap()
            ),
            json!({"text":"will fail","idempotencyKey":"fail"}),
        )
        .await;
    assert_eq!(response.status(), 202);
    let body = json_body(response).await;
    assert!(body["session"].is_null());
    assert_eq!(body["command"]["state"], "rejected");
    let list = json_body(backend.get_authed("/api/v1/sessions").await).await;
    assert!(list["sessions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn changed_bytes_under_the_same_command_key_are_refused_without_redispatch() {
    let backend = TestBackend::start().await;
    let draft = backend.prepare(backend.workdir().to_str().unwrap()).await;
    let url = format!(
        "/api/v1/sessions/draft/{}/commands",
        draft["draftId"].as_str().unwrap()
    );
    assert_eq!(
        backend
            .post_authed(&url, json!({"text":"first","idempotencyKey":"key"}))
            .await
            .status(),
        202
    );
    assert_eq!(
        backend
            .post_authed(&url, json!({"text":"changed","idempotencyKey":"key"}))
            .await
            .status(),
        400
    );
    assert_eq!(backend.source.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_embedded_read_target_cannot_dispatch_to_its_anchor_session() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let child = sylph_protocol::trace::AgentRef::Embedded {
        anchor_session: TestBackend::session_id(&id),
        namespace: "child".into(),
        native_child_id: "native-child".into(),
    };
    let response=backend.post_authed("/api/v1/trace/commands",json!({"target":{"kind":"embedded","agent":child.encode(),"hostSessionId":id,"namespace":"child","nativeChildId":"native-child","route":"unproven"},"action":"send","lease":null,"affectedExecution":null,"idempotencyKey":"send-child","text":"Only the child should receive this","attachments":[],"config":null,"answer":null})).await;
    assert_eq!(response.status(), 409);
    assert_eq!(backend.source.sends.load(Ordering::SeqCst), 1);
}

/// Stop at an embedded target must reach no vendor process at all.
///
/// `docs/unified-trace-conformance.md` § Storage, query, and failure acceptance
/// names Stop and input separately for this row, and they fail differently: input
/// misrouted to the anchor writes a turn, while Stop misrouted to the anchor kills
/// a session the caller never addressed — a destructive action on an unrelated
/// agent. The sibling test above covers input; this covers Stop and Interrupt.
///
/// A 409 alone would not prove it: the refusal has to happen before dispatch, so
/// the assertion is on the vendor interrupt counter, not only the status.
///
/// The anchor is given a live execution lease and the request carries it. Without
/// that, Stop is refused by the lease check before target resolution is reached,
/// and the test passes whether the embedded guard exists or not — a check that
/// cannot fail. With the lease held and matching, the embedded guard is the only
/// thing left that can refuse.
#[tokio::test]
async fn an_embedded_read_target_cannot_stop_or_interrupt_its_anchor_session() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let session = TestBackend::session_id(&id);
    backend
        .state
        .store
        .set_control_state(&session, Some("live-lease"), Some("native-live"), true, 1)
        .await
        .unwrap();
    let child = sylph_protocol::trace::AgentRef::Embedded {
        anchor_session: session,
        namespace: "child".into(),
        native_child_id: "native-child".into(),
    };
    for action in ["stop", "interrupt"] {
        let response = backend
            .post_authed(
                "/api/v1/trace/commands",
                json!({"target":{"kind":"embedded","agent":child.encode(),"hostSessionId":id,
                    "namespace":"child","nativeChildId":"native-child","route":"unproven"},
                    "action":action,"lease":"live-lease","affectedExecution":null,
                    "idempotencyKey":format!("{action}-child"),"text":null,"attachments":[],
                    "config":null,"answer":null}),
            )
            .await;
        assert_eq!(
            response.status(),
            409,
            "{action} at an embedded target must be refused"
        );
        assert_eq!(
            backend.source.interrupts.load(Ordering::SeqCst),
            0,
            "{action} at an embedded target reached the anchor session's vendor process"
        );
    }
}

#[tokio::test]
async fn deleting_a_session_blocks_retained_input_and_old_trace_views() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    backend.state.trace.project_pending().await.unwrap();
    let address = sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(&id)).encode();
    let bootstrap = json_body(
        backend
            .get_authed(&format!("/api/v1/trace/agents/{address}"))
            .await,
    )
    .await;
    assert_eq!(
        backend
            .delete_authed(&format!("/api/v1/sessions/{id}"))
            .await
            .status(),
        204
    );
    assert_eq!(
        backend
            .get_authed(&format!("/api/v1/sessions/{id}"))
            .await
            .status(),
        404
    );
    let response = backend
        .get_authed(&format!(
            "/api/v1/trace/agents/{address}?at_snapshot={}",
            bootstrap["view"]["token"].as_str().unwrap()
        ))
        .await;
    assert_eq!(response.status(), 410);
}

#[tokio::test]
async fn a_registered_subject_needs_an_explicit_environment_grant_for_metadata() {
    let backend = TestBackend::start().await;
    let session = backend.create_session("codex").await;
    let registration = backend
        .post("/api/v1/auth/register", json!({"name":"Reader"}))
        .await;
    let cookie = registration
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let who = json_body(registration).await;
    assert_eq!(
        backend
            .get_with_cookie(&format!("/api/v1/sessions/{session}"), &cookie)
            .await
            .status(),
        403
    );
    backend
        .state
        .store
        .issue_grant(
            who["id"].as_str().unwrap(),
            &backend.environment_id().await,
            "observe",
            who["id"].as_str().unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .get_with_cookie(&format!("/api/v1/sessions/{session}"), &cookie)
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn queued_input_edits_and_cancellation_preserve_accepted_bytes_without_vendor_dispatch() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let session = TestBackend::session_id(&id);
    backend
        .state
        .store
        .set_control_state(&session, Some("held-lease"), Some("native-held"), true, 1)
        .await
        .unwrap();
    let agent = sylph_protocol::trace::AgentRef::Session(session).encode();
    let queued=backend.post_authed("/api/v1/trace/commands",json!({"target":{"kind":"session","agent":agent,"sessionId":id},"action":"queue","lease":null,"affectedExecution":null,"idempotencyKey":"queued","text":"original queued bytes","attachments":[],"config":null,"answer":null})).await;
    assert_eq!(queued.status(), 202);
    let queued = json_body(queued).await;
    assert_eq!(queued["state"], "queued");
    assert_eq!(queued["editable"], true);
    let command = queued["commandId"].as_str().unwrap();
    let edited = backend
        .patch_authed(
            &format!("/api/v1/trace/commands/{command}"),
            json!({"revision":0,"text":"revised queued bytes"}),
        )
        .await;
    assert_eq!(edited.status(), 200);
    let edited = json_body(edited).await;
    assert_eq!(edited["revision"], 1);
    assert_eq!(edited["text"], "revised queued bytes");
    let retained = backend.state.store.command(command).await.unwrap().unwrap();
    assert_eq!(retained.acceptance.input.bytes, b"original queued bytes");
    assert_eq!(retained.current_input.bytes, b"revised queued bytes");
    assert_eq!(
        backend
            .delete_authed(&format!("/api/v1/trace/commands/{command}?revision=0"))
            .await
            .status(),
        400
    );
    let cancelled = backend
        .delete_authed(&format!("/api/v1/trace/commands/{command}?revision=1"))
        .await;
    assert_eq!(cancelled.status(), 200);
    assert_eq!(json_body(cancelled).await["state"], "cancelled");
    assert_eq!(backend.source.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn identity_injection_cannot_change_after_a_native_session_is_recorded() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let response = backend
        .patch_authed(
            &format!("/api/v1/sessions/{id}"),
            json!({"injectIdentity":true}),
        )
        .await;
    assert_eq!(response.status(), 422);
    assert!(
        !backend
            .state
            .store
            .get_session(&TestBackend::session_id(&id))
            .await
            .unwrap()
            .unwrap()
            .inject_identity
    );
}

async fn pending_request(backend: &TestBackend, id: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let requests = backend
                .state
                .store
                .current_requests(&TestBackend::session_id(id))
                .await
                .unwrap();
            if let Some(request) = requests.first() {
                return request["id"].as_str().unwrap().to_owned();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual envagent must publish its scoped request")
}

#[tokio::test]
async fn approval_retry_returns_its_original_receipt_after_the_request_stops_waiting() {
    let backend = TestBackend::with_source(FixtureAgents {
        emit_request: true,
        ..Default::default()
    })
    .await;
    let id = backend.create_session("codex").await;
    let request_id = pending_request(&backend, &id).await;
    let agent = sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(&id)).encode();
    let body = json!({"target":{"kind":"session","agent":agent,"sessionId":id},"action":"approval","idempotencyKey":"resolution-one","attachments":[],"answer":{"requestId":request_id,"optionId":"allow"}});
    let first = backend
        .post_authed("/api/v1/trace/commands", body.clone())
        .await;
    assert_eq!(first.status(), 202, "{}", common::body_string(first).await);
    assert_eq!(backend.source.resolutions.load(Ordering::SeqCst), 1);
    let replay = backend
        .post_authed("/api/v1/trace/commands", body.clone())
        .await;
    assert_eq!(
        replay.status(),
        202,
        "{}",
        common::body_string(replay).await
    );
    assert_eq!(backend.source.resolutions.load(Ordering::SeqCst), 1);
    let mut second = body.clone();
    second["idempotencyKey"] = json!("resolution-two");
    assert!(backend
        .post_authed("/api/v1/trace/commands", second)
        .await
        .status()
        .is_client_error());
    let mut changed = body;
    changed["answer"]["optionId"] = json!("deny");
    assert_eq!(
        backend
            .post_authed("/api/v1/trace/commands", changed)
            .await
            .status(),
        400
    );
    assert_eq!(backend.source.resolutions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_unconfirmed_approval_never_returns_to_the_actionable_request_list() {
    let backend = TestBackend::with_source(FixtureAgents {
        emit_request: true,
        unknown_resolution: true,
        ..Default::default()
    })
    .await;
    let id = backend.create_session("codex").await;
    let request_id = pending_request(&backend, &id).await;
    let agent = sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(&id)).encode();
    let body = json!({"target":{"kind":"session","agent":agent,"sessionId":id},"action":"approval","idempotencyKey":"unconfirmed-answer","attachments":[],"answer":{"requestId":request_id,"optionId":"allow"}});
    let response = backend
        .post_authed("/api/v1/trace/commands", body.clone())
        .await;
    assert_eq!(response.status(), 202);
    assert_eq!(json_body(response).await["state"], "unconfirmed");
    assert!(backend
        .state
        .store
        .current_requests(&TestBackend::session_id(&id))
        .await
        .unwrap()
        .is_empty());
    let replay = json_body(backend.post_authed("/api/v1/trace/commands", body).await).await;
    assert_eq!(replay["state"], "unconfirmed");
    assert_eq!(backend.source.resolutions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_lost_initial_vendor_response_keeps_an_unknown_command_without_a_phantom_session() {
    let backend = TestBackend::with_source(FixtureAgents {
        unknown_send: true,
        ..Default::default()
    })
    .await;
    let draft = backend.prepare(backend.workdir().to_str().unwrap()).await;
    let response = backend
        .post_authed(
            &format!(
                "/api/v1/sessions/draft/{}/commands",
                draft["draftId"].as_str().unwrap()
            ),
            json!({"text":"delivery uncertain","idempotencyKey":"lost-initial"}),
        )
        .await;
    assert_eq!(response.status(), 202);
    let body = json_body(response).await;
    assert_eq!(body["command"]["state"], "unknown");
    assert!(body["session"].is_null());
}

#[tokio::test]
async fn product_periodic_workers_release_the_archive_after_the_last_client_closes() {
    let backend = TestBackend::start().await;
    let trace = std::sync::Arc::downgrade(&backend.state.trace);
    let bridge = std::sync::Arc::downgrade(&backend.state.env_bridge);
    drop(backend);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while trace.strong_count() != 0 || bridge.strong_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("periodic import, dispatch, export and health work must release their owner");
}

#[tokio::test]
async fn deletion_rebuilds_current_search_without_an_additional_capture() {
    let backend = TestBackend::start().await;
    let deleted = backend.create_session("codex").await;
    let retained = backend.create_session("codex").await;
    backend.state.trace.project_pending().await.unwrap();
    backend.state.trace.index_pending().await.unwrap();
    let before = json_body(backend.get_authed("/api/v1/trace/search?q=answer").await).await;
    assert_eq!(before["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        backend
            .delete_authed(&format!("/api/v1/sessions/{deleted}"))
            .await
            .status(),
        204
    );
    let cut = backend.state.trace.evidence().evidence_cut().await.unwrap();
    backend.state.trace.project_pending().await.unwrap();
    backend.state.trace.index_pending().await.unwrap();
    assert_eq!(
        backend.state.trace.evidence().evidence_cut().await.unwrap(),
        cut
    );
    let response = backend.get_authed("/api/v1/trace/search?q=answer").await;
    assert_eq!(
        response.status(),
        200,
        "{}",
        common::body_string(response).await
    );
    let after = json_body(backend.get_authed("/api/v1/trace/search?q=answer").await).await;
    assert_eq!(after["items"].as_array().unwrap().len(), 1);
    let retained_agent =
        sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(&retained)).encode();
    assert_eq!(after["items"][0]["agent"]["address"], retained_agent);
    let old_view = before["view"]["token"].as_str().unwrap();
    let deleted_agent =
        sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(&deleted)).encode();
    assert_eq!(
        backend
            .get_authed(&format!(
                "/api/v1/trace/agents/{deleted_agent}?at_snapshot={old_view}"
            ))
            .await
            .status(),
        410
    );
}
