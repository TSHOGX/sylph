//! End-to-end API tests: auth, sessions, turns, SSE, and error contracts.

mod common;

use axum::http::{header, StatusCode};
use common::{body_string, json_body, MockRunner, MockTraceDb, TestBackend};
use serde_json::{json, Value};

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn health_is_public() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/health").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["apiVersion"], "1");
    // Migrations ran, so the recorded schema version must be present.
    assert_eq!(body["schemaVersion"], 1);
}

#[tokio::test]
async fn bootstrap_sets_a_hardened_cookie() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/auth/token").await;
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie header missing")
        .to_str()
        .unwrap()
        .to_string();

    assert!(cookie.contains("sylph_session="));
    // HttpOnly keeps the token away from page scripts; SameSite=Strict stops a
    // third-party page from riding the cookie on a cross-site request.
    assert!(cookie.contains("HttpOnly"), "cookie not HttpOnly: {cookie}");
    assert!(
        cookie.contains("SameSite=Strict"),
        "cookie not SameSite=Strict: {cookie}"
    );
}

#[tokio::test]
async fn protected_routes_require_the_cookie() {
    let backend = TestBackend::start().await;

    for path in ["/api/v1/diagnostics", "/api/v1/sessions", "/api/v1/agents"] {
        let response = backend.get(path).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} should require auth"
        );
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }
}

#[tokio::test]
async fn diagnostics_reports_runtime_state() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/diagnostics").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert!(body["uptimeSeconds"].is_number());
    assert_eq!(body["agents"], json!(["mock"]));
    assert_eq!(body["activeProcesses"], 0);
}

// ------------------------------------------------------------- sessions

#[tokio::test]
async fn create_and_read_back_a_session() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({
                "agent": "mock",
                "workdir": backend.workdir().display().to_string(),
                "title": "Feature work",
                "flags": {"model": "fast"},
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    let session = &body["session"];

    assert_eq!(session["agent"], "mock");
    assert_eq!(session["title"], "Feature work");
    // A session created through the WebApp is Sylph-managed by definition.
    assert_eq!(session["origin"], "sylph");
    assert_eq!(session["identityMode"], "sylph");
    assert_eq!(session["lifecycleMode"], "sylph");
    assert_eq!(session["status"], "idle");

    // Agent-native identifiers must never appear in an API response.
    assert!(session.get("backendSessionId").is_none());
    assert!(session.get("traceSessionId").is_none());

    let id = session["id"].as_str().unwrap();
    let response = backend.get_authed(&format!("/api/v1/sessions/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["session"]["id"], id);
}

#[tokio::test]
async fn create_session_rejects_unknown_agent_and_missing_workdir() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"agent": "nope", "workdir": backend.workdir().display().to_string()}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "INVALID_REQUEST"
    );

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"agent": "mock", "workdir": "/definitely/not/here"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn broken_runners_are_reported_not_fatal() {
    let backend = TestBackend::start_with_runners(&[
        MockRunner::success("mock"),
        MockRunner::broken_describe("broken"),
    ])
    .await;

    let response = backend.get_authed("/api/v1/agents").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let agents: Vec<&str> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();

    assert_eq!(agents, ["mock"], "broken runner must not be offered");
    assert!(
        body["broken"]["broken"].is_string(),
        "broken runner must be reported with its error"
    );
}

#[tokio::test]
async fn session_history_returns_turns_oldest_first_with_events() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // An empty session has no turns, but is still a 200 rather than a 404.
    let response = backend
        .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["turns"], json!([]));

    for text in ["first question", "second question"] {
        let response = backend
            .post_authed(
                &format!("/api/v1/sessions/{session_id}/turns"),
                json!({"text": text}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let turn_id = json_body(response).await["turn"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        // Serialize the turns: one session runs one turn at a time.
        backend.await_turn(&turn_id).await;
    }

    let response = backend
        .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let turns = body["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);

    // A transcript reads oldest-first, the opposite of the session list order.
    assert_eq!(turns[0]["turn"]["requestText"], "first question");
    assert_eq!(turns[1]["turn"]["requestText"], "second question");
    assert_eq!(turns[0]["turn"]["status"], "completed");

    // Each turn carries its own durable events, in sequence order.
    for turn in turns {
        let events = turn["events"].as_array().unwrap();
        assert!(!events.is_empty(), "turn has no events: {turn}");

        let seqs: Vec<i64> = events
            .iter()
            .map(|e| e["seq"].as_i64().unwrap())
            .collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "events out of order: {seqs:?}");

        // Deltas are transient by protocol; history must expose only the
        // stable record, or replayed text would be duplicated.
        for event in events {
            assert_ne!(
                event["phase"], "delta",
                "delta event leaked into history: {event}"
            );
        }

        assert!(
            events.iter().any(|e| e["kind"] == "assistant"),
            "no assistant event in history: {turn}"
        );
    }
}

#[tokio::test]
async fn session_history_404s_for_an_unknown_session() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/sessions/no-such-id/turns").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );
}

// ---------------------------------------------------------------- turns

#[tokio::test]
async fn turn_runs_to_completion_and_journals_its_events() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "do the thing"}),
        )
        .await;

    // The turn is accepted, not awaited: execution outlives this request.
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = json_body(response).await;
    assert_eq!(body["turn"]["status"], "queued");
    let turn_id = body["turn"]["id"].as_str().unwrap().to_string();

    let finished = backend.await_turn(&turn_id).await;
    assert_eq!(finished["turn"]["status"], "completed");
    assert_eq!(finished["turn"]["runnerExitCode"], 0);

    // Replaying from -1 yields the whole journal for the turn.
    let response = backend
        .get_authed(&format!("/api/v1/turns/{turn_id}/events?after_seq=-1"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let stream = body_string(response).await;

    // Only stable events are journaled: the runner's `completed` phases and the
    // server's own status/user events.
    assert!(stream.contains("\"kind\":\"user\""), "missing user event");
    assert!(stream.contains("thinking done"), "missing completed thinking");
    assert!(stream.contains("mock reply"), "missing assistant reply");
    assert!(stream.contains("\"kind\":\"usage\""), "missing usage event");

    // Deltas are transient projections and must not reach the journal.
    assert!(
        !stream.contains("\"text\":\"think\""),
        "delta text leaked into the journal: {stream}"
    );

    // Sequence numbers must be strictly increasing.
    let seqs: Vec<i64> = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
        .filter_map(|event| event["seq"].as_i64())
        .collect();
    assert!(!seqs.is_empty(), "no events parsed from stream");
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "sequence numbers not increasing: {seqs:?}"
    );
}

#[tokio::test]
async fn session_returns_to_idle_after_a_turn() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "hello"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;

    let response = backend.get_authed(&format!("/api/v1/sessions/{session_id}")).await;
    assert_eq!(json_body(response).await["session"]["status"], "idle");
}

#[tokio::test]
async fn idempotency_key_returns_the_original_turn() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;
    let path = format!("/api/v1/sessions/{session_id}/turns");

    let first = backend
        .post_authed_with_key(&path, json!({"text": "once"}), "key-1")
        .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_id = json_body(first).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    backend.await_turn(&first_id).await;

    // A retry must not start a second agent process.
    let retry = backend
        .post_authed_with_key(&path, json!({"text": "once"}), "key-1")
        .await;
    assert_eq!(
        retry.status(),
        StatusCode::OK,
        "a replayed key should return 200, not a fresh 202"
    );
    assert_eq!(json_body(retry).await["turn"]["id"], first_id);
}

#[tokio::test]
async fn runner_failure_is_reported_as_a_failed_turn() {
    let backend = TestBackend::start_with_runners(&[MockRunner::failure("mock")]).await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "this will fail"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let finished = backend.await_turn(&turn_id).await;
    assert_eq!(finished["turn"]["status"], "failed");
    assert_eq!(finished["turn"]["errorCode"], "TURN_FAILED");
    assert_eq!(
        finished["turn"]["errorMessage"],
        "mock runner failed on purpose"
    );
}

#[tokio::test]
async fn missing_envelope_is_a_protocol_error() {
    let backend = TestBackend::start_with_runners(&[MockRunner::no_envelope("mock")]).await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "no envelope"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let finished = backend.await_turn(&turn_id).await;
    assert_eq!(finished["turn"]["status"], "failed");
    assert_eq!(finished["turn"]["errorCode"], "RUNNER_PROTOCOL_ERROR");
    // The stderr tail is the only diagnostic available, so it must be kept.
    assert!(
        finished["turn"]["errorMessage"]
            .as_str()
            .unwrap()
            .contains("forgot its envelope"),
        "stderr tail missing: {}",
        finished["turn"]["errorMessage"]
    );
}

#[tokio::test]
async fn stopping_a_running_turn_cancels_it() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "take your time"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Wait for the process to actually exist before signaling it.
    let mut running = false;
    for _ in 0..100 {
        let response = backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await;
        if json_body(response).await["turn"]["status"] == "running" {
            running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(running, "turn never reached running");

    let response = backend
        .post_authed(&format!("/api/v1/turns/{turn_id}/stop"), json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let finished = backend.await_turn(&turn_id).await;
    assert_eq!(finished["turn"]["status"], "cancelled");
}

#[tokio::test]
async fn concurrent_turns_on_one_session_conflict() {
    let backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session_id = backend.create_session("mock").await;
    let path = format!("/api/v1/sessions/{session_id}/turns");

    let first = backend.post_authed(&path, json!({"text": "first"})).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let turn_id = json_body(first).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Let the supervisor register the process.
    for _ in 0..100 {
        let response = backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await;
        if json_body(response).await["turn"]["status"] == "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let second = backend.post_authed(&path, json!({"text": "second"})).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(second).await["error"]["code"],
        "SESSION_ALREADY_RUNNING"
    );

    // Leave nothing running behind this test.
    let _ = backend
        .post_authed(&format!("/api/v1/turns/{turn_id}/stop"), json!({}))
        .await;
}

// -------------------------------------------------------- error contract

#[tokio::test]
async fn unknown_resources_return_stable_error_codes() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/sessions/does-not-exist").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );

    let response = backend.get_authed("/api/v1/turns/does-not-exist").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await["error"]["code"], "TURN_NOT_FOUND");

    let response = backend
        .post_authed("/api/v1/turns/does-not-exist/stop", json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stopping_a_finished_turn_conflicts() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "quick"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&turn_id).await;

    let response = backend
        .post_authed(&format!("/api/v1/turns/{turn_id}/stop"), json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "STOP_ALREADY_COMPLETED"
    );
}

#[tokio::test]
async fn oversized_attachment_is_rejected() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // 9 MiB of base64 payload, over the documented 8 MiB per-attachment cap.
    let oversized = base64_of_zeros(9 * 1024 * 1024);
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({
                "text": "here is a file",
                "attachments": [{"name": "big.bin", "type": "application/octet-stream", "data": oversized}],
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "ATTACHMENT_INVALID"
    );
}

#[tokio::test]
async fn malformed_base64_attachment_is_rejected() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({
                "text": "broken file",
                "attachments": [{"name": "x.png", "type": "image/png", "data": "!!!not base64!!!"}],
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "ATTACHMENT_INVALID"
    );
}

#[tokio::test]
async fn attachments_over_the_total_cap_are_rejected() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // Three 3.5 MiB files: each is under the 8 MiB per-attachment cap, but
    // they total 10.5 MiB, over the 10 MiB per-turn cap. The cap is across the
    // turn, so this must fail even though no single file is oversized.
    //
    // The size is chosen to land inside a narrow window. base64 inflates by
    // 4/3, so 10.5 MiB of payload is 14 MiB on the wire, still under the
    // 16 MiB body limit — the request has to reach the handler for the
    // attachment cap, rather than the transport limit, to be what rejects it.
    let chunk = base64_of_zeros(7 * 512 * 1024);
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({
                "text": "three files",
                "attachments": [
                    {"name": "a.bin", "type": "application/octet-stream", "data": chunk},
                    {"name": "b.bin", "type": "application/octet-stream", "data": chunk},
                    {"name": "c.bin", "type": "application/octet-stream", "data": chunk},
                ],
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "ATTACHMENT_INVALID");
    // The message drives the UI's failure text, so it must name the limit
    // rather than just saying the request was invalid.
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("total"),
        "message does not identify the total cap: {message}"
    );
}

#[tokio::test]
async fn a_grossly_oversized_body_is_rejected_by_the_transport_limit() {
    // Above the attachment cap there is a second, coarser boundary: the 16 MiB
    // request body limit, which rejects before any handler runs and therefore
    // returns a bare 413 with no ATTACHMENT_INVALID body. A client must handle
    // both, so the two paths are pinned separately: modest overage yields a
    // 400 naming the limit, gross overage yields 413.
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let chunk = base64_of_zeros(7 * 1024 * 1024);
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({
                "text": "far too much",
                "attachments": [
                    {"name": "a.bin", "type": "application/octet-stream", "data": chunk},
                    {"name": "b.bin", "type": "application/octet-stream", "data": chunk},
                    {"name": "c.bin", "type": "application/octet-stream", "data": chunk},
                ],
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn too_many_attachments_are_rejected() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // Six tiny files, over the documented cap of 5 per turn.
    let tiny = base64_of_zeros(16);
    let attachments: Vec<Value> = (0..6)
        .map(|i| json!({"name": format!("f{i}.txt"), "type": "text/plain", "data": tiny}))
        .collect();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "too many", "attachments": attachments}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "ATTACHMENT_INVALID"
    );
}

fn base64_of_zeros(len: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(vec![0u8; len])
}

/// Reconnection replays stable journal events when the turn has finished.
///
/// This isolates the SSE handler's reconnection logic: when a client reconnects
/// with `after_seq=N`, they receive every stable event where `seq > N` from the
/// journal. The handler merges journal replay with the ephemeral projection when
/// the turn is still live; this test checks the boundary case where the turn is
/// finished and the projection is empty, so the handler must rely entirely on
/// the journal.
#[tokio::test]
async fn sse_reconnection_replays_journal_after_turn_finishes() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // Start a turn and wait for it to complete.
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{}/turns", session_id),
            json!({
                "model": {},
                "message": { "role": "user", "content": [{ "type": "text", "text": "test" }] },
                "attachments": [],
            }),
        )
        .await;
    let body = json_body(response).await;
    let turn_id = body["turn"]["id"].as_str().unwrap();

    backend.await_turn(turn_id).await;

    // Fetch the full event stream once to learn the highest stable seq.
    let response = backend
        .get_authed(&format!("/api/v1/turns/{}/events?after_seq=-1", turn_id))
        .await;
    let stream = body_string(response).await;
    let highest_seq: i32 = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter_map(|event| event["seq"].as_i64())
        .map(|seq| seq as i32)
        .max()
        .expect("no events in stream");

    // Reconnect from after_seq=0 (replay everything except the seq=0 bootstrap).
    let response = backend
        .get_authed(&format!("/api/v1/turns/{}/events?after_seq=0", turn_id))
        .await;
    let stream = body_string(response).await;
    let replayed_seqs: Vec<i32> = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .filter_map(|event| event["seq"].as_i64())
        .map(|seq| seq as i32)
        .collect();

    // The reconnected stream should contain every stable event > 0.
    assert!(
        !replayed_seqs.is_empty(),
        "reconnection returned no events"
    );
    assert!(
        replayed_seqs.contains(&highest_seq),
        "reconnection missed the highest stable event (seq={})",
        highest_seq
    );
    assert!(
        replayed_seqs.iter().all(|&seq| seq > 0),
        "reconnection included the bootstrap event (seq=0)"
    );
}

// -------------------------------------------------------------- search

#[tokio::test]
async fn search_requires_auth() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/search?q=refactor").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_reports_the_archive_unavailable_instead_of_failing() {
    // The default harness points at a nonexistent binary, which is the same
    // state as a machine without TraceDB installed. That is normal locally, so
    // the route answers 200 and lets the UI say so rather than 5xx.
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/search?q=refactor").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["available"], false);
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    assert!(
        body["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "unavailable response carried no reason: {body}"
    );
}

#[tokio::test]
async fn search_surfaces_a_tracedb_error_as_unavailable() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::failing()).await;

    let response = backend.get_authed("/api/v1/search?q=refactor").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["available"], false);
    assert!(
        body["detail"].as_str().unwrap().contains("index is locked"),
        "error detail lost: {body}"
    );
}

#[tokio::test]
async fn search_without_a_query_returns_empty_and_available() {
    // An empty box is not an error, and it must not spawn TraceDB at all.
    let backend = TestBackend::start().await;

    for path in ["/api/v1/search?q=", "/api/v1/search?q=%20%20", "/api/v1/search"] {
        let response = backend.get_authed(path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");

        let body = json_body(response).await;
        assert_eq!(body["available"], true, "{path}");
        assert_eq!(body["results"].as_array().unwrap().len(), 0, "{path}");
    }
}

#[tokio::test]
async fn search_returns_hits_and_drops_ranking_internals() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_hits(json!([
        {
            "id": "claude:abc-123/main",
            "agent": "claude",
            "ask": "refactor the store layer",
            "cwd": "/tmp/project",
            "outcome": "completed",
            "bestMatch": {"kind": "assistant", "snippet": "moved queries into store.rs"},
            "hits": 4,
            "startedAtMs": 1_700_000_000_000i64,
            "lineageRootId": "claude:abc-123",
            // Ranking mechanics TraceDB owns; the API must not leak them.
            "score": 12.5,
            "scoreBreakdown": {"bm25": 3.2}
        }
    ])))
    .await;

    let response = backend.get_authed("/api/v1/search?q=refactor").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["available"], true);

    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);

    let hit = &results[0];
    assert_eq!(hit["id"], "claude:abc-123/main");
    assert_eq!(hit["agent"], "claude");
    assert_eq!(hit["ask"], "refactor the store layer");
    assert_eq!(hit["bestMatch"]["snippet"], "moved queries into store.rs");
    assert_eq!(hit["lineageRootId"], "claude:abc-123");
    assert!(hit["score"].is_null(), "leaked TraceDB scoring: {hit}");
    assert!(hit["scoreBreakdown"].is_null(), "leaked breakdown: {hit}");
    // Nothing local mirrors this archived session yet.
    assert!(hit["sessionId"].is_null(), "unexpected sessionId: {hit}");
}

#[tokio::test]
async fn search_clamps_an_absurd_limit() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_hits(json!([]))).await;

    let response = backend.get_authed("/api/v1/search?q=a&limit=100000").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["available"], true);
}

// ------------------------------------------------------- session filters

#[tokio::test]
async fn session_list_filters_by_agent() {
    let backend =
        TestBackend::start_with_runners(&[MockRunner::success("alpha"), MockRunner::success("beta")])
            .await;

    let alpha = backend.create_session("alpha").await;
    let beta = backend.create_session("beta").await;

    let ids = |body: &Value| -> Vec<String> {
        body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    };

    let all = json_body(backend.get_authed("/api/v1/sessions").await).await;
    assert_eq!(ids(&all).len(), 2);

    let only_alpha = json_body(backend.get_authed("/api/v1/sessions?agent=alpha").await).await;
    assert_eq!(ids(&only_alpha), vec![alpha]);

    let only_beta = json_body(backend.get_authed("/api/v1/sessions?agent=beta").await).await;
    assert_eq!(ids(&only_beta), vec![beta]);

    // An agent with no sessions is an empty list, not an error.
    let response = backend.get_authed("/api/v1/sessions?agent=gamma").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(ids(&json_body(response).await).len(), 0);
}

#[tokio::test]
async fn session_list_treats_a_blank_filter_as_unfiltered() {
    // A cleared filter input serializes as an empty parameter; it must not be
    // read as "match the empty string" and hide every session.
    let backend = TestBackend::start().await;
    backend.create_session("mock").await;

    for path in [
        "/api/v1/sessions?agent=",
        "/api/v1/sessions?workdir=%20",
        "/api/v1/sessions?agent=&origin=&workdir=",
    ] {
        let body = json_body(backend.get_authed(path).await).await;
        assert_eq!(
            body["sessions"].as_array().unwrap().len(),
            1,
            "{path} hid the session"
        );
    }
}

#[tokio::test]
async fn session_list_filters_by_workdir_substring_case_insensitively() {
    let backend = TestBackend::start().await;
    backend.create_session("mock").await;

    let workdir = backend.workdir().display().to_string();
    let fragment = workdir.rsplit('/').next().unwrap().to_uppercase();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions?workdir={fragment}"))
            .await,
    )
    .await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 1);

    let body = json_body(
        backend
            .get_authed("/api/v1/sessions?workdir=definitely-not-a-real-path")
            .await,
    )
    .await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn session_list_filters_by_updated_time_range() {
    let backend = TestBackend::start().await;
    backend.create_session("mock").await;

    let far_past = 1_000_000_000_000i64;
    let far_future = 4_000_000_000_000i64;

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions?updatedAfterMs={far_past}"))
            .await,
    )
    .await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 1);

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions?updatedBeforeMs={far_past}"))
            .await,
    )
    .await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 0);

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions?updatedAfterMs={far_past}&updatedBeforeMs={far_future}"
            ))
            .await,
    )
    .await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn session_list_rejects_an_unknown_origin() {
    // Origin drives identity and lifecycle policy, so a typo must be loud
    // rather than quietly yielding an empty list.
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/sessions?origin=sylf").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");

    for origin in ["sylph", "native"] {
        let response = backend
            .get_authed(&format!("/api/v1/sessions?origin={origin}"))
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{origin} rejected");
    }
}

// ------------------------------------------------- archive availability

#[tokio::test]
async fn health_reports_the_archive_available_when_it_answers() {
    // Regression: availability was keyed on a `version` field that the TraceDB
    // protocol does not define, so a reachable archive always reported
    // unavailable and /health contradicted a search that was working. The stub
    // returns no version field, which is exactly the real payload's shape.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_hits(json!([]))).await;

    let health = json_body(backend.get("/api/v1/health").await).await;
    assert_eq!(
        health["tracedb"]["available"], true,
        "reachable archive reported unavailable: {health}"
    );

    // The unauthenticated endpoint must not leak counts or paths.
    assert!(health["tracedb"].get("sessions").is_none());
    assert!(health["tracedb"].get("path").is_none());

    let search = json_body(backend.get_authed("/api/v1/search?q=x").await).await;
    assert_eq!(
        search["available"], health["tracedb"]["available"],
        "health and search disagree about the same archive"
    );
}

#[tokio::test]
async fn health_reports_the_archive_unavailable_when_absent() {
    let backend = TestBackend::start().await;

    let health = json_body(backend.get("/api/v1/health").await).await;
    assert_eq!(health["ok"], true, "backend must stay healthy without TraceDB");
    assert_eq!(health["tracedb"]["available"], false);

    let search = json_body(backend.get_authed("/api/v1/search?q=x").await).await;
    assert_eq!(search["available"], false);
}

#[tokio::test]
async fn diagnostics_carries_archive_counts() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/diagnostics").await).await;
    // Null rather than absent: an unreachable archive is a reportable state,
    // distinct from a field the response forgot to include.
    assert!(
        body.get("tracedb").is_some() && body["tracedb"].is_null(),
        "expected a null tracedb field: {body}"
    );
}

// ------------------------------------------------- native session import

/// A `show` stub: session metadata and events in the shape the current release
/// emits.
fn archived_session(cwd: Option<&str>, title: Option<&str>, ask: &str) -> MockTraceDb {
    let mut metadata = json!({});
    if let Some(cwd) = cwd {
        metadata["cwd"] = json!(cwd);
    }
    if let Some(title) = title {
        metadata["title"] = json!(title);
    }
    let events = json!([
        {"kind": "system", "text": "{}", "created_at_ms": 0, "idx": 0},
        {"kind": "user", "text": ask, "created_at_ms": 1_700_000_000_000i64, "idx": 2},
        {"kind": "assistant", "text": "done", "created_at_ms": 1_700_000_100_000i64, "idx": 3},
    ]);
    MockTraceDb::with_session(metadata, events)
}

/// Build a backend whose stubbed archive describes a session rooted in a
/// directory that really exists.
///
/// Import validates that the recorded working directory is present, and each
/// harness owns a fresh temp directory that is deleted when it drops. So a
/// throwaway harness is created first to borrow a live path, and returned
/// alongside the one under test to keep that path alive for the test's
/// duration.
async fn backend_with_archived_session(ask: &str) -> (TestBackend, TestBackend, String) {
    let host = TestBackend::start().await;
    let workdir = host.workdir().display().to_string();
    let backend = TestBackend::start_with_tracedb(archived_session(Some(&workdir), None, ask))
        .await;
    (backend, host, workdir)
}

#[tokio::test]
async fn importing_a_native_session_preserves_its_identity() {
    let (backend, _host, workdir) = backend_with_archived_session("审查冗余的代码").await;

    let response = backend
        .post_authed(
            "/api/v1/sessions/import",
            json!({"traceSessionId": "mock:f45ba2ed-3866-4dba-9769-801029b16aa3"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let session = json_body(response).await["session"].clone();

    // The product invariant: a native session keeps its originating agent's
    // identity and lifecycle. Anything else here would mean Sylph policy had
    // been applied to someone else's session.
    assert_eq!(session["origin"], "native");
    assert_eq!(session["identityMode"], "preserve");
    assert_eq!(session["lifecycleMode"], "preserve");
    assert_eq!(session["agent"], "mock");
    assert_eq!(session["status"], "idle");

    // Workdir comes from the archive, not the request.
    assert_eq!(session["workdir"], workdir);
    // Title falls back to the opening ask, and multi-byte text must survive.
    assert_eq!(session["title"], "审查冗余的代码");
    // Archive identifiers stay server-side.
    assert!(session.get("traceSessionId").is_none());
}

#[tokio::test]
async fn importing_reads_the_workdir_from_session_metadata() {
    // A session with no recorded cwd is unresumable (the import path rejects
    // it), so to verify metadata reaches the import path we need a title. A
    // session whose metadata carries a title never scans events to find the
    // opening user text, so it is evidence that metadata was read and not
    // merely constructed.
    let host = TestBackend::start().await;
    let workdir = host.workdir().display().to_string();
    let backend = TestBackend::start_with_tracedb(archived_session(
        Some(&workdir),
        Some("Recorded title"),
        "the ask",
    ))
    .await;

    let response = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:archived"}))
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let session = json_body(response).await["session"].clone();
    assert_eq!(session["workdir"], workdir);
    // The archive's own title wins over the opening ask when it has one.
    assert_eq!(session["title"], "Recorded title");
}

#[tokio::test]
async fn importing_an_archived_session_without_a_workdir_is_unprocessable() {
    // Well-formed request, unusable record: the archive has the session but
    // never recorded where it ran. That is not a client mistake, so it must not
    // report as INVALID_REQUEST, and the code has to be distinguishable so the
    // UI can explain the actual remedy.
    let backend = TestBackend::start_with_tracedb(archived_session(None, None, "no cwd")).await;

    let response = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:archived"}))
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "TRACE_SESSION_NOT_RESUMABLE");
    // The id makes it clear which archived session is unusable.
    assert!(
        body["error"]["message"].as_str().unwrap().contains("mock:archived"),
        "id missing from message: {body}"
    );
}

#[tokio::test]
async fn importing_the_same_archived_session_twice_conflicts() {
    let (backend, _host, _workdir) = backend_with_archived_session("hi").await;

    let body = json!({"traceSessionId": "mock:abc-123"});
    let first = backend.post_authed("/api/v1/sessions/import", body.clone()).await;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = backend.post_authed("/api/v1/sessions/import", body).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(second).await["error"]["code"],
        "SESSION_ALREADY_IMPORTED"
    );
}

#[tokio::test]
async fn importing_an_unknown_archived_session_is_not_found() {
    // `null` is how the archive reports an id it does not have.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::without_session()).await;

    let response = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:nope"}))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "TRACE_SESSION_NOT_FOUND");
    // The id is echoed so a stale id is diagnosable.
    assert!(body["error"]["message"].as_str().unwrap().contains("mock:nope"));
}

#[tokio::test]
async fn importing_rejects_a_malformed_trace_id() {
    let backend = TestBackend::start().await;

    for id in ["no-colon", ":leading", "trailing:", ""] {
        let response = backend
            .post_authed("/api/v1/sessions/import", json!({"traceSessionId": id}))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{id:?} should be rejected"
        );
        assert_eq!(
            json_body(response).await["error"]["code"],
            "INVALID_REQUEST"
        );
    }
}

#[tokio::test]
async fn importing_requires_a_runner_for_the_originating_agent() {
    // A native session that cannot be resumed by its own agent would be inert:
    // listable but never continuable. Refuse rather than create it.
    let (backend, _host, _workdir) = backend_with_archived_session("hi").await;

    let response = backend
        .post_authed(
            "/api/v1/sessions/import",
            json!({"traceSessionId": "someagent:abc"}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = json_body(response).await["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("someagent"),
        "message should name the missing agent: {message}"
    );
}

#[tokio::test]
async fn importing_reports_an_unreachable_archive() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::failing()).await;

    let response = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:abc"}))
        .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "TRACEDB_UNAVAILABLE"
    );
}

#[tokio::test]
async fn importing_rejects_an_archive_session_whose_workdir_is_gone() {
    let backend = TestBackend::start_with_tracedb(archived_session(
        Some("/definitely/not/a/real/path"),
        None,
        "hi",
    ))
    .await;

    let response = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:archived"}))
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let message = json_body(response).await["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("/definitely/not/a/real/path"),
        "message should name the missing directory: {message}"
    );
}

#[tokio::test]
async fn imported_sessions_are_listable_by_origin() {
    let (backend, _host, _workdir) = backend_with_archived_session("hi").await;

    backend.create_session("mock").await;
    let imported = backend
        .post_authed("/api/v1/sessions/import", json!({"traceSessionId": "mock:abc"}))
        .await;
    assert_eq!(imported.status(), StatusCode::CREATED);

    let native = json_body(backend.get_authed("/api/v1/sessions?origin=native").await).await;
    let sessions = native["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["origin"], "native");

    let sylph = json_body(backend.get_authed("/api/v1/sessions?origin=sylph").await).await;
    assert_eq!(sylph["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(sylph["sessions"][0]["origin"], "sylph");
}

#[tokio::test]
async fn a_command_turn_needs_no_message_text() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    // Commands are invoked instead of a message, so the composer sends no text.
    // This has to be accepted: rejecting an empty `text` would make every
    // command dialog unusable.
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"command": "model", "args": {"name": "opus"}}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let body = json_body(response).await;
    let turn_id = body["turn"]["id"].as_str().unwrap().to_string();
    // The invocation is recorded on the turn, which is what lets the transcript
    // show a command rather than an empty message.
    assert_eq!(body["turn"]["commandInvocationId"], "model");
    assert_eq!(body["turn"]["requestText"], "");

    let turn = backend.await_turn(&turn_id).await;
    assert_eq!(turn["turn"]["status"], "completed");
}

/// Wait for `turn_id` to report `status`, or panic. Polls because the runner is
/// a real subprocess whose startup is not synchronous with the HTTP response.
async fn await_turn_status(backend: &TestBackend, turn_id: &str, status: &str) {
    for _ in 0..100 {
        let response = backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await;
        if json_body(response).await["turn"]["status"] == status {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("turn never reached {status}");
}

#[tokio::test]
async fn a_restart_interrupts_a_turn_that_was_still_running() {
    // Child processes do not survive a restart, so a turn left marked running
    // would otherwise appear to hang forever with no way to learn it is dead.
    let mut backend = TestBackend::start_with_runners(&[MockRunner::slow("mock")]).await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "take your time"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    await_turn_status(&backend, &turn_id, "running").await;

    backend.restart().await;

    let turn = json_body(backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await).await;
    assert_eq!(turn["turn"]["status"], "interrupted");
    assert_eq!(turn["turn"]["errorCode"], "TURN_INTERRUPTED");
    assert!(
        turn["turn"]["completedAtMs"].as_i64().unwrap_or(0) > 0,
        "an interrupted turn is terminal and must carry a completion time"
    );
}

#[tokio::test]
async fn a_session_is_usable_again_after_a_restart_interrupts_its_turn() {
    // The important half: a session left running would reject every subsequent
    // turn as a conflict, stranding it permanently.
    let mut backend =
        TestBackend::start_with_runners(&[MockRunner::slow("slow"), MockRunner::success("mock")])
            .await;
    let session_id = backend.create_session("slow").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "hang"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    await_turn_status(&backend, &turn_id, "running").await;

    let before = json_body(backend.get_authed(&format!("/api/v1/sessions/{session_id}")).await).await;
    assert_eq!(before["session"]["status"], "running");

    backend.restart().await;

    let after = json_body(backend.get_authed(&format!("/api/v1/sessions/{session_id}")).await).await;
    assert_eq!(
        after["session"]["status"], "idle",
        "a session cannot be running when no turn is"
    );

    // And it actually accepts work rather than merely reporting idle.
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "again"}),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "a recovered session must accept a new turn"
    );
}

#[tokio::test]
async fn a_restart_leaves_finished_turns_alone() {
    // Recovery must be surgical: rewriting a completed turn would corrupt
    // durable history on every ordinary restart.
    let mut backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "hello"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    await_turn_status(&backend, &turn_id, "completed").await;

    let before = json_body(backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await).await;

    backend.restart().await;

    let after = json_body(backend.get_authed(&format!("/api/v1/turns/{turn_id}")).await).await;
    assert_eq!(after["turn"]["status"], "completed");
    assert!(after["turn"]["errorCode"].is_null());
    assert_eq!(
        after["turn"]["completedAtMs"], before["turn"]["completedAtMs"],
        "a completed turn must not be re-stamped by recovery"
    );
}

#[tokio::test]
async fn a_restart_preserves_journalled_events() {
    // Recovery touches turn and session status only. The event journal is the
    // durable record and must survive untouched.
    let mut backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "hello"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    await_turn_status(&backend, &turn_id, "completed").await;

    let before = json_body(backend.get_authed(&format!("/api/v1/sessions/{session_id}/turns")).await).await;
    let before_events = before["turns"][0]["events"].as_array().unwrap().len();
    assert!(before_events > 0, "the turn journalled nothing to compare");

    backend.restart().await;

    let after = json_body(backend.get_authed(&format!("/api/v1/sessions/{session_id}/turns")).await).await;
    assert_eq!(
        after["turns"][0]["events"].as_array().unwrap().len(),
        before_events,
        "restart must not drop or duplicate journalled events"
    );
    assert_eq!(after["turns"].as_array().unwrap().len(), 1);
}
