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
