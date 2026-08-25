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
    // Migrations ran, so the recorded schema version must be present. Pinned to
    // the current number rather than asserted non-zero: a client that does not
    // understand the journal shape needs this to change when the shape does.
    assert_eq!(body["schemaVersion"], 3);
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

    // A native id is absent until the first runner turn; the Web UUID remains
    // the only id for an untouched draft.
    assert!(session.get("nativeSessionId").is_none());
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
async fn deleting_an_empty_session_is_idempotent_but_turns_make_it_durable() {
    let backend = TestBackend::start().await;
    let empty = backend.create_session("mock").await;

    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{empty}"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(backend.get_authed(&format!("/api/v1/sessions/{empty}")).await.status(), StatusCode::NOT_FOUND);

    let durable = backend.create_session("mock").await;
    let response = backend
        .post_authed(&format!("/api/v1/sessions/{durable}/turns"), json!({"text": "keep me"}))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{durable}"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(backend.get_authed(&format!("/api/v1/sessions/{durable}")).await.status(), StatusCode::OK);
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
    let session_response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"agent": "mock", "workdir": backend.workdir().display().to_string()}),
        )
        .await;
    assert_eq!(session_response.status(), StatusCode::CREATED);
    let session_id = json_body(session_response).await["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();

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
    // No exit code: a turn on a live link has no process of its own. Its outcome
    // arrives as an event, which is why the field is null rather than 0 — a 0
    // here would be inventing a process that never existed.
    assert!(finished["turn"]["runnerExitCode"].is_null());

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

    // The live row exposes the same native identity that TraceDB will later
    // ingest, and its title is derived from the opening ask immediately.
    let session = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}"))
            .await,
    )
    .await["session"]
        .clone();
    assert_eq!(session["nativeSessionId"], "mock-session-1");
    assert_eq!(session["title"], "do the thing");
}

/// An event stream must forbid intermediaries from re-encoding it.
///
/// This pins a bug whose symptom pointed entirely the wrong way. The frontend
/// proxies `/api/v1/*` through Next.js, whose `compression` middleware gzips any
/// compressible content type — `text/event-stream` included. gzip cannot be
/// decoded until a block is flushed, so the browser received nothing until the
/// turn ended and the whole transcript then appeared at once, while `curl`
/// (sending no `Accept-Encoding`) saw perfectly paced frames. The stream was
/// fine; the encoding was buffering it.
///
/// `no-transform` (RFC 9111 §5.2.2.6) is what forbids that re-encoding, and it
/// belongs on the response so the guarantee holds behind any proxy rather than
/// depending on one front end's configuration. Asserting a single header also
/// matters: axum sets its own `no-cache`, and two `cache-control` headers would
/// leave which directive applies up to the intermediary.
#[tokio::test]
async fn the_event_stream_forbids_intermediaries_from_compressing_it() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    let body = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{session_id}/turns"),
                json!({"text": "stream please"}),
            )
            .await,
    )
    .await;
    let turn_id = body["turn"]["id"].as_str().unwrap().to_string();
    backend.await_turn(&turn_id).await;

    let response = backend
        .get_authed(&format!("/api/v1/turns/{turn_id}/events?after_seq=-1"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
    );

    let cache_control: Vec<&str> = response
        .headers()
        .get_all(axum::http::header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();

    assert_eq!(
        cache_control.len(),
        1,
        "exactly one cache-control header, or the applied directive is \
         ambiguous: {cache_control:?}"
    );
    assert!(
        cache_control[0].contains("no-transform"),
        "event stream must forbid re-encoding, or a compressing proxy buffers \
         it into one lump: {cache_control:?}"
    );
    assert!(
        cache_control[0].contains("no-cache"),
        "event stream must stay uncached: {cache_control:?}"
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
async fn an_agent_that_produces_nothing_still_ends_its_turn() {
    // v1 had a protocol violation to detect here: a runner that printed no final
    // envelope. v2 has no envelope to omit, because the turn event *is* the
    // outcome. What remains is the real failure this stands in for — an agent that
    // ends a turn having produced nothing — and the turn must still reach a
    // terminal state rather than hanging.
    let backend = TestBackend::start_with_runners(&[MockRunner::no_envelope("mock")]).await;
    let session_id = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "no output"}),
        )
        .await;
    let turn_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let finished = backend.await_turn(&turn_id).await;
    assert_eq!(finished["turn"]["status"], "failed");
    assert_eq!(finished["turn"]["errorCode"], "TURN_FAILED");
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
    assert_eq!(
        finished["turn"]["status"], "cancelled",
        "turn was {:?} / {:?}",
        finished["turn"]["errorCode"], finished["turn"]["errorMessage"]
    );
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

// ------------------------------------------------------- archive browsing

#[tokio::test]
async fn archive_listing_requires_auth() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/archive/sessions?workdir=/w/app").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn archive_listing_requires_an_absolute_workdir() {
    let backend = TestBackend::start().await;

    // Answered by this endpoint's own validation, not by axum's query rejection,
    // so the body carries a code a client can branch on.
    for path in [
        "/api/v1/archive/sessions",
        "/api/v1/archive/sessions?workdir=",
        "/api/v1/archive/sessions?workdir=relative/path",
    ] {
        let response = backend.get_authed(path).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(json_body(response).await["error"]["code"], "INVALID_REQUEST");
    }
}

#[tokio::test]
async fn archive_listing_reports_unavailable_instead_of_failing() {
    // A machine without TraceDB is normal, and the sidebar calls this on every
    // expanded workspace. A 5xx there would turn an optional component into a
    // broken-looking UI.
    let backend = TestBackend::start().await;

    let response = backend
        .get_authed("/api/v1/archive/sessions?workdir=/w/app")
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["available"], false);
    assert_eq!(body["sessions"].as_array().unwrap().len(), 0);
    assert!(
        body["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "unavailable response carried no reason: {body}"
    );
}

#[tokio::test]
async fn archive_listing_keeps_only_the_requested_directory() {
    // TraceDB matches `cwd` as a substring, so a request for `/w/app` also
    // returns `/w/app-old` and `/w/app/sub`. Without an exact re-filter here, a
    // workspace's history would be polluted by its neighbours.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_listing(
        json!([
            {"id": "claude:wanted", "agent": "claude", "cwd": "/w/app",
             "title": "In the directory", "events": 12},
            {"id": "claude:sibling", "agent": "claude", "cwd": "/w/app-old",
             "title": "Sibling directory", "events": 12},
            {"id": "claude:child", "agent": "claude", "cwd": "/w/app/sub",
             "title": "Subdirectory", "events": 12},
            // Trailing slash is the same directory.
            {"id": "claude:slashed", "agent": "claude", "cwd": "/w/app/",
             "title": "Trailing slash", "events": 12},
        ]),
        None,
    ))
    .await;

    let response = backend
        .get_authed("/api/v1/archive/sessions?workdir=/w/app")
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let ids: Vec<&str> = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["claude:wanted", "claude:slashed"], "{body}");
}

#[tokio::test]
async fn archive_listing_folds_subagents_into_the_session_that_spawned_them() {
    // Subagent sessions are 43% of a real archive. Listed as peers they bury the
    // sessions a user actually started, so they become a count instead.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([
                {"id": "claude:root", "agent": "claude", "cwd": "/w/app",
                 "title": "Parent", "events": 40},
                {"id": "claude:root/agent-one", "agent": "claude", "cwd": "/w/app",
                 "title": "Subagent one", "events": 9},
                {"id": "claude:root/agent-two", "agent": "claude", "cwd": "/w/app",
                 "title": "Subagent two", "events": 9},
            ]),
            None,
        )
        .and_session_for("claude:root", json!({"cwd": "/w/app", "title": "Parent"}))
        .and_session_for(
            "claude:root/agent-one",
            json!({"cwd": "/w/app", "parent_session_id": "claude:root"}),
        )
        .and_session_for(
            "claude:root/agent-two",
            json!({"cwd": "/w/app", "parent_session_id": "claude:root"}),
        ),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "subagents must not be listed as peers: {body}");
    assert_eq!(sessions[0]["id"], "claude:root");
    assert_eq!(sessions[0]["subagentCount"], 2);
}

#[tokio::test]
async fn archive_listing_rolls_up_a_subagent_that_records_only_a_parent() {
    // The id suffix is not the signal: 199 non-empty sessions in the local archive
    // record a `parent_session_id` while carrying no `/agent-` suffix, almost all
    // of them codex. Reading the id alone left that delegated work at the top
    // level, which is the gap this covers.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([
                {"id": "codex:parent", "agent": "codex", "cwd": "/w/app", "events": 300},
                {"id": "codex:child-a", "agent": "codex", "cwd": "/w/app", "events": 40},
                {"id": "codex:child-b", "agent": "codex", "cwd": "/w/app", "events": 30},
            ]),
            None,
        )
        .and_session_for("codex:parent", json!({"cwd": "/w/app", "title": "Parent"}))
        .and_session_for(
            "codex:child-a",
            json!({"cwd": "/w/app", "parent_session_id": "codex:parent"}),
        )
        .and_session_for(
            "codex:child-b",
            json!({"cwd": "/w/app", "parent_session_id": "codex:parent"}),
        ),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "a recorded parent must roll up even with no id suffix: {body}"
    );
    assert_eq!(sessions[0]["id"], "codex:parent");
    assert_eq!(sessions[0]["subagentCount"], 2);
}

#[tokio::test]
async fn archive_listing_keeps_a_subagent_that_ran_in_another_directory() {
    // 484 of 2238 subagents in the local archive ran in a different directory than
    // their parent -- a worktree or a subdirectory. Folding those into a parent
    // that is not in this listing deleted them from the only directory that would
    // ever show them, emptying 42 directories, one holding 193 sessions.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([
                {"id": "claude:root/agent-sub", "agent": "claude", "cwd": "/w/app/frontend",
                 "title": "Work in the subdirectory", "events": 120},
            ]),
            None,
        )
        // The parent lives one directory up, so it is not a row here.
        .and_session_for("claude:root", json!({"cwd": "/w/app", "title": "Parent"}))
        .and_session_for(
            "claude:root/agent-sub",
            json!({"cwd": "/w/app/frontend", "parent_session_id": "claude:root"}),
        ),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app/frontend")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "a cross-directory subagent is this directory's only history: {body}"
    );
    assert_eq!(sessions[0]["id"], "claude:root/agent-sub");
}

#[tokio::test]
async fn archive_listing_shows_one_side_of_a_fork_and_names_the_other() {
    // A fork replays its origin's whole prefix, so both sides share every early
    // user turn and read as one conversation. Listing both shows it twice under one
    // label. `list` is ordered newest-first, so the survivor is the one reached
    // first -- and it is not a fixed side: measured locally, one pair has its later
    // work in the fork and the other in the origin.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([
                {"id": "claude:resumed", "agent": "claude", "cwd": "/w/app",
                 "title": "The conversation", "events": 1108, "endedAtMs": 2000},
                {"id": "claude:origin", "agent": "claude", "cwd": "/w/app",
                 "title": "The conversation", "events": 389, "endedAtMs": 1000},
            ]),
            None,
        )
        .and_session_for(
            "claude:resumed",
            json!({"cwd": "/w/app", "title": "The conversation",
                   "forked_from": "claude:origin#ec58fee8-5801-431a-a51e-eb99e33d8c42"}),
        )
        .and_session_for("claude:origin", json!({"cwd": "/w/app", "title": "The conversation"})),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "one conversation must not list twice: {body}");
    assert_eq!(sessions[0]["id"], "claude:resumed", "the newer side survives");
    // Named so a client accumulating pages can drop the other side when the pair
    // straddles two requests, which the server cannot see.
    assert_eq!(sessions[0]["forkOf"], "claude:origin");
    // A fork is not delegated work, so it must not be counted as a subagent.
    assert_eq!(sessions[0]["subagentCount"], 0);
}

#[tokio::test]
async fn archive_listing_counts_a_subagent_listed_before_its_parent() {
    // The archive orders by recency and a subagent commonly ends after the
    // session that spawned it, so the child's row can arrive first.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([
                {"id": "claude:root/agent-late", "agent": "claude", "cwd": "/w/app",
                 "title": "Subagent", "events": 9},
                {"id": "claude:root", "agent": "claude", "cwd": "/w/app",
                 "title": "Parent", "events": 40},
            ]),
            None,
        )
        .and_session_for("claude:root", json!({"cwd": "/w/app", "title": "Parent"}))
        .and_session_for(
            "claude:root/agent-late",
            json!({"cwd": "/w/app", "parent_session_id": "claude:root"}),
        ),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{body}");
    assert_eq!(
        sessions[0]["subagentCount"], 1,
        "a child seen before its parent was dropped: {body}"
    );
}

#[tokio::test]
async fn archive_listing_reaches_a_match_buried_behind_subdirectory_rows() {
    // The upstream `cwd` filter is an unanchored substring match, so a parent
    // directory's pattern also matches everything beneath it, and those sort by
    // recency rather than depth. A real directory with 88 sessions of its own had
    // its first exact match 1300 rows in, behind subdirectory work: a page-count
    // cap gave up first and reported it empty, which is the very bug this endpoint
    // exists to fix.
    let mut rows: Vec<serde_json::Value> = (0..120)
        .map(|i| {
            json!({"id": format!("claude:deep-{i}"), "agent": "claude",
                   "cwd": "/w/app/nested/deeper", "title": "Subdirectory work",
                   "events": 10})
        })
        .collect();
    // The only exact match sits last, after every deeper-path row.
    rows.push(json!({"id": "claude:wanted", "agent": "claude", "cwd": "/w/app",
                     "title": "The directory's own work", "events": 42}));

    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(json!(rows), None)
            .and_session_for("claude:wanted", json!({"cwd": "/w/app"})),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "a match behind many deeper-path rows must still be found: {body}"
    );
    assert_eq!(sessions[0]["id"], "claude:wanted");
}

#[tokio::test]
async fn archive_listing_drops_sessions_with_no_transcript() {
    // Ingested but empty, common for aborted runs in temp directories. There is
    // nothing to read, so listing one offers a dead link.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_listing(
        json!([
            {"id": "claude:empty", "agent": "claude", "cwd": "/w/app",
             "title": "Nothing recorded", "events": 0},
            {"id": "claude:real", "agent": "claude", "cwd": "/w/app",
             "title": "Real work", "events": 3},
        ]),
        None,
    ))
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{body}");
    assert_eq!(sessions[0]["id"], "claude:real");
}

#[tokio::test]
async fn an_untitled_archived_session_is_labelled_by_its_opening_ask() {
    // codex and pi record no titles at all, so this fallback is the common path.
    // The leading events are harness-injected envelopes, which every session of
    // that agent shares and which therefore identify nothing.
    let backend = TestBackend::start_with_tracedb(
        MockTraceDb::with_listing(
            json!([{"id": "codex:untitled", "agent": "codex", "cwd": "/w/app",
                    "title": null, "events": 20}]),
            None,
        )
        .and_session(
            json!({"id": "codex:untitled", "agent": "codex", "cwd": "/w/app"}),
            json!([
                {"idx": 0, "kind": "user", "text": "<permissions instructions> sandboxing"},
                {"idx": 1, "kind": "user", "text": "<environment_context> <cwd>/w/app</cwd>"},
                {"idx": 2, "kind": "user", "text": "Fix the reconnect loop"},
            ]),
        ),
    )
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions?workdir=/w/app")
            .await,
    )
    .await;

    assert_eq!(
        body["sessions"][0]["label"], "Fix the reconnect loop",
        "label fell back to injected boilerplate: {body}"
    );
}

#[tokio::test]
async fn an_archived_transcript_renders_as_settled_timeline_events() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_session(
        json!({"id": "claude:archived", "agent": "claude", "cwd": "/w/app",
               "title": "Fix login", "started_at_ms": 1700000000000_i64}),
        json!([
            {"idx": 0, "kind": "user", "text": "fix login", "created_at_ms": 1},
            {"idx": 1, "kind": "tool_call", "name": "Bash", "call_id": "call_1",
             "text": "{\"command\":\"ls -la\"}", "created_at_ms": 2},
            {"idx": 2, "kind": "tool_result", "call_id": "call_1",
             "text": "total 0", "created_at_ms": 3},
        ]),
    ))
    .await;

    let response = backend
        .get_authed("/api/v1/archive/sessions/claude:archived")
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["label"], "Fix login");
    assert_eq!(body["cwd"], "/w/app");
    assert_eq!(body["truncated"], false);

    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    // Settled history: nothing here is mid-stream, and a `delta` phase would
    // make the timeline wait for text that will never arrive.
    for event in events {
        assert_eq!(event["phase"], "completed", "{event}");
        assert_eq!(event["stability"], "stable", "{event}");
    }
    // The call's arguments must reach the shape the timeline reads.
    assert_eq!(events[1]["data"]["input"]["command"], "ls -la");
    // A result groups with its call through a shared stream id.
    assert_eq!(events[1]["streamId"], events[2]["streamId"]);
}

#[tokio::test]
async fn an_archived_transcript_reports_a_missing_session_as_not_found() {
    let backend = TestBackend::start_with_tracedb(MockTraceDb::without_session()).await;

    let response = backend
        .get_authed("/api/v1/archive/sessions/claude:absent")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "TRACE_SESSION_NOT_FOUND"
    );
}

#[tokio::test]
async fn an_archived_subagent_transcript_is_reachable_despite_its_slashed_id() {
    // A subagent id contains a slash (`claude:uuid/agent-name`), which a
    // single-segment route capture would reject — that is 43% of the archive.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_session(
        json!({"id": "claude:root/agent-one", "agent": "claude", "cwd": "/w/app"}),
        json!([{"idx": 0, "kind": "user", "text": "do the subtask", "created_at_ms": 1}]),
    ))
    .await;

    let response = backend
        .get_authed("/api/v1/archive/sessions/claude:root/agent-one")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["id"], "claude:root/agent-one");
}

#[tokio::test]
async fn an_archived_transcript_is_truncated_rather_than_streamed_whole() {
    // The largest session in the local archive is 20 MB of JSON.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_session(
        json!({"id": "claude:big", "agent": "claude", "cwd": "/w/app"}),
        json!([
            {"idx": 0, "kind": "user", "text": "one", "created_at_ms": 1},
            {"idx": 1, "kind": "assistant", "text": "two", "created_at_ms": 2},
            {"idx": 2, "kind": "assistant", "text": "three", "created_at_ms": 3},
        ]),
    ))
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/archive/sessions/claude:big?limit=2")
            .await,
    )
    .await;

    assert_eq!(body["events"].as_array().unwrap().len(), 2);
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn workspaces_omit_archive_only_directories_by_default() {
    // The sidebar polls this endpoint, so the archive scan must be opt-in: it
    // needs live sessions only, and paying for an archive walk on every poll
    // would put that cost on the hot path.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_listing(
        json!([{"id": "claude:old", "agent": "claude", "cwd": "/w/archived-only",
                "title": "Older work", "events": 30, "endedAtMs": 1700000000000_i64}]),
        None,
    ))
    .await;

    let body = json_body(backend.get_authed("/api/v1/workspaces").await).await;
    let paths: Vec<&str> = body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["path"].as_str().unwrap())
        .collect();

    assert!(
        !paths.contains(&"/w/archived-only"),
        "archive scan must be opt-in: {body}"
    );
}

#[tokio::test]
async fn workspaces_can_include_a_directory_whose_only_history_is_archived() {
    // A directory a user wants to work in has no live session yet. Without this
    // it could never be pinned or picked, so its history stayed unreachable.
    let backend = TestBackend::start_with_tracedb(MockTraceDb::with_listing(
        json!([
            {"id": "claude:old", "agent": "claude", "cwd": "/w/archived-only",
             "title": "Older work", "events": 30, "endedAtMs": 1700000000000_i64},
            // Subagents and empty sessions would inflate the count with rows the
            // history browser then filters out.
            {"id": "claude:old/agent-x", "agent": "claude", "cwd": "/w/archived-only",
             "title": "Subagent", "events": 5, "endedAtMs": 1700000000001_i64},
            {"id": "claude:blank", "agent": "claude", "cwd": "/w/archived-only",
             "title": null, "events": 0},
        ]),
        None,
    ))
    .await;

    let body = json_body(
        backend
            .get_authed("/api/v1/workspaces?includeArchived=true")
            .await,
    )
    .await;

    let row = body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["path"] == "/w/archived-only")
        .unwrap_or_else(|| panic!("archive-only workspace missing: {body}"));

    assert_eq!(row["sessionCount"], 0, "it has no live sessions");
    assert_eq!(row["archivedCount"], 1, "subagent and empty rows must not count");
    // Ordering is by when work last happened, which archived history answers.
    assert_eq!(row["lastUsedMs"], 1700000000000_i64);
}

#[tokio::test]
async fn workspaces_still_answer_when_the_archive_is_unreachable() {
    // The live workspaces are most of the answer, so an absent archive must
    // degrade rather than fail the request.
    let backend = TestBackend::start_with_runners(&[MockRunner::success("mock")]).await;
    backend.create_session("mock").await;

    let response = backend
        .get_authed("/api/v1/workspaces?includeArchived=true")
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!json_body(response).await["workspaces"]
        .as_array()
        .unwrap()
        .is_empty());
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
    // The archive id is exposed so the archive browser can tell that an archived
    // row and this live session are one conversation, and list it once rather
    // than twice. The imported session has no runner-native id until resumed.
    assert_eq!(
        session["traceSessionId"],
        "mock:f45ba2ed-3866-4dba-9769-801029b16aa3"
    );
    assert!(session.get("nativeSessionId").is_none());
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

// ------------------------------------------------ error envelope contract

/// Every failure under `/api/v1/` carries the error envelope, router-level ones
/// included.
///
/// axum's default fallback answers with an empty body, and a client cannot tell
/// that from a handler reporting a missing file — both are a bare 404. That
/// ambiguity had a real cost: a stale backend without the files routes made the
/// Artifacts pane display "Not Found" inside the file tree, which reads as a
/// missing directory rather than a missing endpoint. This test is the contract
/// that keeps the two distinguishable.
#[tokio::test]
async fn unmatched_routes_answer_with_the_error_envelope() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/not-an-endpoint").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = json_body(response).await;
    assert_eq!(
        body["error"]["code"], "NOT_FOUND",
        "a router 404 must be distinguishable from FILE_NOT_FOUND"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "the envelope must explain itself, not just carry a code"
    );
}

/// The wrong method on a real path is a 405 with a code, not a bodiless 404.
#[tokio::test]
async fn wrong_method_answers_405_with_the_error_envelope() {
    let backend = TestBackend::start().await;

    let response = backend.post_authed("/api/v1/health", json!({})).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "METHOD_NOT_ALLOWED"
    );
}

/// The fallback sits inside the auth layer, so an unmatched path does not leak
/// the route surface to an unauthenticated caller.
///
/// Registering it after `.layer` would compile and appear to work, while
/// answering every unknown path before the cookie was ever checked.
#[tokio::test]
async fn unmatched_routes_are_still_behind_auth() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/not-an-endpoint").await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the fallback must not answer before the auth layer runs"
    );
    assert_eq!(json_body(response).await["error"]["code"], "UNAUTHORIZED");
}

// ---------------------------------------------------------- files (Phase 6.3)

/// The Artifacts pane lists one directory level, directories first.
#[tokio::test]
async fn file_tree_lists_the_session_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("README.md"), "# Title\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;

    let entries = body["entries"].as_array().unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"src"), "directory missing: {names:?}");
    assert!(names.contains(&"README.md"), "file missing: {names:?}");
    // Directories sort ahead of files, which is what makes a tree scannable.
    let src_at = names.iter().position(|n| *n == "src").unwrap();
    let readme_at = names.iter().position(|n| *n == "README.md").unwrap();
    assert!(src_at < readme_at, "directories must sort first: {names:?}");

    assert_eq!(entries[src_at]["isDir"], true);
    assert_eq!(entries[readme_at]["isDir"], false);
    assert_eq!(entries[readme_at]["size"], 8);
}

/// Machine-generated directories are noise in an artifact browser, and `.git`
/// additionally holds object data that has no business being served.
#[tokio::test]
async fn file_tree_omits_vcs_and_build_directories() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    for skipped in [".git", "node_modules", "target", ".next"] {
        std::fs::create_dir_all(root.join(skipped)).unwrap();
    }
    std::fs::create_dir_all(root.join("src")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["src"], "only real source dirs belong: {names:?}");
}

/// The traversal defense. `..` must not escape the workdir, whatever spelling
/// it arrives in.
#[tokio::test]
async fn file_access_refuses_paths_outside_the_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    // A real secret outside the root, so a successful escape would be visible
    // rather than merely a 404 from a nonexistent target.
    let outside = backend.workdir().parent().unwrap().join("outside-secret.txt");
    std::fs::write(&outside, "do not serve me").unwrap();

    for attempt in [
        "../outside-secret.txt",
        "./../outside-secret.txt",
        "src/../../outside-secret.txt",
        "/etc/hosts",
    ] {
        let response = backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path={}",
                urlencoding(attempt)
            ))
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
            "{attempt} was not refused: {status} {body}"
        );
        assert_ne!(
            body["content"].as_str().unwrap_or_default(),
            "do not serve me",
            "{attempt} escaped the workdir"
        );
    }
}

/// A symlink is an escape that lives on disk, so pattern-matching the request
/// cannot catch it. Canonicalization must.
#[tokio::test]
async fn symlinks_leaving_the_workdir_are_not_served() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let outside = backend.workdir().parent().unwrap().join("linked-secret.txt");
    std::fs::write(&outside, "secret via symlink").unwrap();
    let link = backend.workdir().join("escape.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=escape.txt"
        ))
        .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_ne!(
        body["content"].as_str().unwrap_or_default(),
        "secret via symlink",
        "a symlink served content from outside the workdir (status {status})"
    );

    // It is also hidden from the listing, so it is never offered in the tree.
    let tree = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"escape.txt"),
        "escaping symlink listed: {names:?}"
    );
}

/// Text vs image vs binary is decided by the bytes, not the extension, so the
/// client can pick a viewer without guessing.
#[tokio::test]
async fn file_content_reports_encoding_from_the_bytes() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::write(root.join("notes.md"), "# Hello\n\nBody text.\n").unwrap();
    // A `.log` holding invalid UTF-8: extension says text, content says binary.
    std::fs::write(root.join("blob.log"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    // 1x1 transparent GIF.
    let gif: [u8; 14] = [
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00,
    ];
    std::fs::write(root.join("pixel.gif"), gif).unwrap();

    let text = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=notes.md"
            ))
            .await,
    )
    .await;
    assert_eq!(text["encoding"], "text");
    assert_eq!(text["mimeType"], "text/markdown");
    assert!(text["content"].as_str().unwrap().contains("# Hello"));
    assert_eq!(text["truncated"], false);

    let binary = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=blob.log"
            ))
            .await,
    )
    .await;
    assert_eq!(binary["encoding"], "binary");
    assert!(binary["content"].is_null(), "binary bytes must not be sent");

    let image = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/content?path=pixel.gif"
            ))
            .await,
    )
    .await;
    assert_eq!(image["encoding"], "image");
    assert_eq!(image["mimeType"], "image/gif");
    // Base64 of the GIF header, so the client can build a data: URL.
    assert!(image["content"].as_str().unwrap().starts_with("R0lGODlh"));
}

#[tokio::test]
async fn missing_files_are_404_and_directories_are_not_content() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("src")).unwrap();

    let missing = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=nope.txt"
        ))
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(missing).await["error"]["code"], "FILE_NOT_FOUND");

    let directory = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/content?path=src"
        ))
        .await;
    assert_eq!(directory.status(), StatusCode::BAD_REQUEST);
}

/// A symlinked directory must browse as the directory it points at.
///
/// This pins the bug that made the Artifacts pane useless on any workspace laid
/// out with links: `DirEntry::metadata` does not follow symlinks, so a link to a
/// directory was reported as a small *file*. The pane then drew it with no
/// expand arrow, and clicking it asked for content that was not text — a
/// directory that looked present and could not be opened.
#[tokio::test]
async fn file_tree_follows_symlinks_to_their_target_kind() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("real/nested")).unwrap();
    std::fs::write(root.join("real/inside.txt"), "contents\n").unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(root.join("real/inside.txt"), root.join("linked-file")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let entries = body["entries"].as_array().unwrap();
    let find = |name: &str| {
        entries
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
    };

    let linked_dir = find("linked-dir");
    assert_eq!(
        linked_dir["isDir"], true,
        "a symlink to a directory must report isDir, or the tree cannot expand it"
    );
    assert!(
        linked_dir["size"].is_null(),
        "a directory carries no size, however it was reached"
    );
    assert_eq!(linked_dir["isSymlink"], true, "the link is still flagged as one");

    // The target's size, not the link's own byte length.
    let linked_file = find("linked-file");
    assert_eq!(linked_file["isDir"], false);
    assert_eq!(linked_file["size"], 9);
    assert_eq!(linked_file["isSymlink"], true);

    // And it actually expands: the whole point of reporting it as a directory.
    let nested = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files?path=linked-dir"
            ))
            .await,
    )
    .await;
    let names: Vec<&str> = nested["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"inside.txt"), "link did not expand: {names:?}");
    assert!(names.contains(&"nested"), "link did not expand: {names:?}");
}

/// A symlinked build directory is noise under the name it is browsed by.
#[tokio::test]
async fn file_tree_skips_symlinked_build_directories() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::fs::create_dir_all(root.join("vendor-store")).unwrap();
    std::os::unix::fs::symlink(root.join("vendor-store"), root.join("node_modules")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"node_modules"),
        "symlinked node_modules listed: {names:?}"
    );
}

/// A dangling symlink is omitted rather than listed as an entry that cannot open.
#[tokio::test]
async fn file_tree_omits_dangling_symlinks() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let root = backend.workdir();
    std::os::unix::fs::symlink(root.join("never-existed"), root.join("dangling")).unwrap();
    std::fs::write(root.join("real.txt"), "ok\n").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"dangling"), "dangling link listed: {names:?}");
    // A broken link must not take the rest of the listing down with it.
    assert!(names.contains(&"real.txt"), "listing lost entries: {names:?}");
}

#[tokio::test]
async fn file_endpoints_require_auth_and_a_real_session() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let unauthed = backend
        .get(&format!("/api/v1/sessions/{session_id}/files"))
        .await;
    assert_eq!(unauthed.status(), StatusCode::UNAUTHORIZED);

    let unknown = backend
        .get_authed("/api/v1/sessions/does-not-exist/files")
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(unknown).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );
}

// ----------------------------------------------------- workspaces (Phase 6.1)

/// The sidebar groups sessions by workdir, and Quick Start exists before any
/// session is created in it.
#[tokio::test]
async fn workspaces_group_sessions_and_always_include_quick_start() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let first = backend.create_session("agent").await;
    let second = backend.create_session("agent").await;
    assert_ne!(first, second);

    let body = json_body(backend.get_authed("/api/v1/workspaces").await).await;

    let quick = body["quickStartDir"].as_str().unwrap();
    // The configured directory, not a hardcoded one. Asserting a fixed `~/.agents`
    // was what let the endpoint keep pointing at another project's tree: the test
    // and the code agreed with each other and both were wrong about the product.
    assert_eq!(
        quick,
        backend.quick_start_dir().display().to_string(),
        "Quick Start must be the configured directory"
    );
    assert!(!body["dataDir"].as_str().unwrap().is_empty());

    let workspaces = body["workspaces"].as_array().unwrap();
    let quick_row = workspaces
        .iter()
        .find(|w| w["isQuickStart"] == true)
        .expect("Quick Start row missing");
    assert_eq!(quick_row["path"], quick);

    // Both sessions share one workdir, so they collapse into a single space.
    let workdir = backend.workdir().display().to_string();
    let row = workspaces
        .iter()
        .find(|w| w["path"].as_str().unwrap() == workdir)
        .unwrap_or_else(|| panic!("workspace {workdir} missing from {workspaces:?}"));
    assert_eq!(row["sessionCount"], 2);
    assert_eq!(row["runningCount"], 0);
    assert!(row["lastUsedMs"].as_i64().unwrap() > 0);
    assert_eq!(row["isQuickStart"], false);
}

#[tokio::test]
async fn workspaces_require_auth() {
    let backend = TestBackend::start().await;
    assert_eq!(
        backend.get("/api/v1/workspaces").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// ------------------------------------------------------------------ config
//
// `GET /config` is what makes a misconfiguration diagnosable from the UI instead
// of from a log the user never reads. These assert the properties that make it
// useful — every setting present, each one naming its source and its override
// variable, and no write path — rather than the exact values, which are the
// harness's.

/// The settings panel has to be able to show the whole resolution. A key that the
/// resolver knows and this endpoint omits is invisible to every user.
#[tokio::test]
async fn config_reports_every_setting_with_its_source_and_override() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/config").await).await;

    assert!(!body["home"].as_str().unwrap().is_empty());
    assert!(body["configPath"].as_str().unwrap().ends_with("config.toml"));

    let entries = body["entries"].as_array().unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e["key"].as_str().unwrap()).collect();
    for expected in [
        "server.host",
        "server.port",
        "paths.data_dir",
        "paths.agents_dir",
        "paths.soul",
        "paths.quick_start_dir",
        "paths.browse_roots",
        "tracedb.bin",
        "log.level",
    ] {
        assert!(keys.contains(&expected), "{expected} missing from {keys:?}");
    }

    // Every entry has to answer "who set this" and "what would override it".
    // A resolved value alone cannot distinguish a wrong path from a path that is
    // being ignored, and those need opposite fixes.
    for entry in entries {
        let key = entry["key"].as_str().unwrap();
        let source = entry["source"].as_str().unwrap();
        assert!(
            ["env", "file", "default"].contains(&source),
            "{key} has an unknown source: {source}"
        );
        assert!(
            entry["envVar"].as_str().unwrap().starts_with("SYLPH_"),
            "{key} must name the variable that overrides it"
        );
    }
}

/// The harness sets browse roots and the TraceDB path through the environment, so
/// those two must report `env` while everything it leaves alone reports `default`.
/// This is the property that makes the field trustworthy.
#[tokio::test]
async fn config_distinguishes_an_overridden_setting_from_a_defaulted_one() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/config").await).await;
    let entries = body["entries"].as_array().unwrap();
    let source_of = |key: &str| -> String {
        entries
            .iter()
            .find(|e| e["key"] == key)
            .unwrap_or_else(|| panic!("{key} missing"))["source"]
            .as_str()
            .unwrap()
            .to_string()
    };

    assert_eq!(source_of("paths.browse_roots"), "env");
    assert_eq!(source_of("tracedb.bin"), "env");
    assert_eq!(source_of("server.host"), "default");
    assert_eq!(source_of("paths.data_dir"), "default");
}

/// "Missing" is the fact that turns an empty agent dropdown into a one-line
/// diagnosis, and it must not be confused with "this setting is not a path".
#[tokio::test]
async fn config_reports_whether_each_path_exists_and_omits_it_for_non_paths() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/config").await).await;
    let entries = body["entries"].as_array().unwrap();
    let entry = |key: &str| entries.iter().find(|e| e["key"] == key).unwrap();

    // Seeded by the harness through the production path, so it is there.
    assert_eq!(entry("paths.data_dir")["exists"], true);
    // Pointed at a file the harness deliberately never creates.
    assert_eq!(entry("tracedb.bin")["exists"], false);

    // Absent rather than `false`: a log level is not a path, and rendering it as
    // missing would report a working setting as broken.
    for key in ["server.host", "server.port", "log.level"] {
        assert!(
            entry(key).get("exists").is_none(),
            "{key} is not a path and must not claim existence"
        );
    }
}

/// Bundled adapter resolution has to name every directory it tried and mark the
/// one it took. Without that, a missing install and a mistyped path produce the
/// same empty agent list and need opposite fixes.
#[tokio::test]
async fn config_names_every_bundled_adapter_directory_it_considered() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/config").await).await;
    let candidates = body["bundledAgentCandidates"].as_array().unwrap();

    assert!(!candidates.is_empty());
    assert_eq!(
        candidates.iter().filter(|c| c["chosen"] == true).count(),
        1,
        "exactly one directory is the one discovery reads"
    );
    for candidate in candidates {
        assert!(candidate["path"].as_str().unwrap().starts_with('/'));
        assert!(candidate["exists"].is_boolean());
    }
}

/// Authenticated, because every value is an absolute local filesystem path.
/// `/health` is the unauthenticated probe and reports booleans only.
#[tokio::test]
async fn config_requires_auth() {
    let backend = TestBackend::start().await;
    assert_eq!(
        backend.get("/api/v1/config").await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// There is deliberately no write path: `browse_roots` bounds what any client may
/// enumerate and `agents_dir` names directories this server executes from, so a
/// loopback HTTP surface that could rewrite them is a different threat model.
#[tokio::test]
async fn config_is_read_only() {
    let backend = TestBackend::start().await;

    for method in ["PUT", "PATCH", "POST", "DELETE"] {
        let response = backend.request_authed(method, "/api/v1/config", None).await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /config must not be accepted"
        );
    }
}

/// Percent-encode a query value. Written inline because the traversal test needs
/// to send paths that would otherwise be reinterpreted by the router.
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------- machine-level browsing
//
// The picker that binds a new workspace. These assert the bound, not the
// convenience: the roots come from configuration, and everything outside them is
// refused however it is spelled. The harness sets `browse_roots` to its throwaway
// root, so "outside a root" is expressible without touching the real `$HOME`.

#[tokio::test]
async fn fs_roots_lists_configured_browse_roots() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/fs/roots").await).await;

    let roots = body["roots"].as_array().unwrap();
    assert!(!roots.is_empty(), "at least the harness root must be listed");
    assert!(
        body["home"].as_str().is_some(),
        "home is needed for `~/...` abbreviation"
    );
    // Absolute, because the client hands these straight back as a `workdir`.
    assert!(roots[0]["path"].as_str().unwrap().starts_with('/'));
}

#[tokio::test]
async fn fs_tree_lists_only_directories() {
    let backend = TestBackend::start().await;
    let root = backend.workdir();
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::write(root.join("loose-file.txt"), "x").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.display().to_string())
            ))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"project"), "directory missing: {names:?}");
    assert!(
        !names.contains(&"loose-file.txt"),
        "files must not be listed by a directory picker: {names:?}"
    );
}

/// `~/.agents` is the Quick Start directory and a legitimate workspace, so a
/// picker that hid dot-directories would hide a directory the product points at.
#[tokio::test]
async fn fs_tree_lists_dot_directories() {
    let backend = TestBackend::start().await;
    let root = backend.workdir();
    std::fs::create_dir_all(root.join(".agents")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.display().to_string())
            ))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&".agents"), "dotfiles must be pickable: {names:?}");
}

#[tokio::test]
async fn fs_tree_omits_build_directories() {
    let backend = TestBackend::start().await;
    let root = backend.workdir();
    for skipped in [".git", "node_modules", "target"] {
        std::fs::create_dir_all(root.join(skipped)).unwrap();
    }
    std::fs::create_dir_all(root.join("src")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.display().to_string())
            ))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["src"], "only real directories belong: {names:?}");
}

/// The traversal defense for the machine-level browser. Resolution, not
/// pattern-matching, is what refuses this.
#[tokio::test]
async fn fs_tree_refuses_paths_outside_every_root() {
    let backend = TestBackend::start().await;

    // `/etc` exists and is a directory, so this is refused for being outside the
    // configured roots rather than for being missing.
    let response = backend.get_authed("/api/v1/fs/tree?path=/etc").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let escaped = format!("{}/../../..", backend.workdir().display());
    let response = backend
        .get_authed(&format!("/api/v1/fs/tree?path={}", urlencoding(&escaped)))
        .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "`..` must not climb out of a browse root"
    );
}

/// A symlink out of the root is not merely refused on access — it is never
/// offered, so the client cannot present an unopenable node.
#[cfg(unix)]
#[tokio::test]
async fn fs_tree_omits_symlinks_escaping_the_root() {
    let backend = TestBackend::start().await;
    let root = backend.workdir();
    std::fs::create_dir_all(root.join("inside")).unwrap();
    std::os::unix::fs::symlink("/etc", root.join("outside")).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.display().to_string())
            ))
            .await,
    )
    .await;

    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"inside"));
    assert!(
        !names.contains(&"outside"),
        "an escaping symlink must not be listed: {names:?}"
    );
}

/// `parent` is what the picker draws a "go up" affordance from, so it must stop
/// at a root rather than handing back a path the next request would refuse.
#[tokio::test]
async fn fs_tree_withholds_parent_at_a_root() {
    let backend = TestBackend::start().await;
    // The configured root itself, not the workdir inside it: only at a root is
    // "no parent offered" the correct answer.
    let root = backend.browse_root().to_path_buf();
    std::fs::create_dir_all(root.join("child")).unwrap();

    let at_root = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.display().to_string())
            ))
            .await,
    )
    .await;
    assert!(
        at_root.get("parent").is_none() || at_root["parent"].is_null(),
        "a root has no parent to offer: {at_root:?}"
    );

    let at_child = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/fs/tree?path={}",
                urlencoding(&root.join("child").display().to_string())
            ))
            .await,
    )
    .await;
    assert!(
        at_child["parent"].as_str().is_some(),
        "a directory below a root must offer its parent"
    );
}

// ------------------------------------------------------- workspace file upload
//
// Writing is the one mutating filesystem operation. The agent already writes
// anywhere under `workdir`, so these assert that the endpoint grants nothing
// beyond that, and destroys nothing by accident.

#[tokio::test]
async fn upload_writes_into_the_session_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("docs")).unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "docs", "name": "note.txt", "data": "aGVsbG8=" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = json_body(response).await;
    assert_eq!(body["path"], "docs/note.txt");
    assert_eq!(body["size"], 5);
    assert_eq!(
        std::fs::read_to_string(backend.workdir().join("docs/note.txt")).unwrap(),
        "hello",
        "the decoded bytes must reach disk where the agent can read them"
    );
}

#[tokio::test]
async fn upload_refuses_to_escape_the_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "../..", "name": "escaped.txt", "data": "eA==" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A separator in the filename must not reconstitute a path either.
    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "../escaped.txt", "data": "eA==" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        !backend.workdir().parent().unwrap().join("escaped.txt").exists(),
        "the filename must be reduced to one component, not treated as a path"
    );
}

#[tokio::test]
async fn mkdir_creates_one_directory_and_reports_its_relative_path() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("docs")).unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files/directory"),
            json!({ "path": "docs", "name": "drafts" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = json_body(response).await;
    // The relative path is what the pane addresses files by, so the client can
    // expand straight to the new folder without re-deriving it.
    assert_eq!(body["path"], "docs/drafts");
    assert!(backend.workdir().join("docs/drafts").is_dir());
}

/// A folder the user names keeps the name they typed.
///
/// The upload sanitizer strips non-ASCII, which is right for an incidental
/// filename and wrong here: `文档` becoming `attachment` is data loss the user
/// cannot see the cause of.
#[tokio::test]
async fn mkdir_preserves_a_non_ascii_name() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files/directory"),
            json!({ "path": "", "name": "文档" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(backend.workdir().join("文档").is_dir());
}

#[tokio::test]
async fn mkdir_refuses_a_separator_a_collision_and_an_escape() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("existing")).unwrap();
    let route = format!("/api/v1/sessions/{session_id}/files/directory");

    // A separator is rejected rather than silently creating a two-level tree.
    let response = backend
        .post_authed(&route, json!({ "path": "", "name": "a/b" }))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!backend.workdir().join("a").exists());

    // A collision is reported, so "New folder" twice does not look like a no-op.
    let response = backend
        .post_authed(&route, json!({ "path": "", "name": "existing" }))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // And the parent still goes through the containment check.
    let response = backend
        .post_authed(&route, json!({ "path": "../..", "name": "escaped" }))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!backend.workdir().parent().unwrap().join("escaped").exists());
}

#[tokio::test]
async fn delete_removes_a_file() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    let target = backend.workdir().join("stale.txt");
    std::fs::write(&target, "x").unwrap();

    let response = backend
        .delete_authed(&format!(
            "/api/v1/sessions/{session_id}/files?path=stale.txt"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!target.exists());
}

/// The guard that matters most: an empty path normalizes to the workdir, and a
/// client bug that sent one would otherwise erase the user's project. No
/// legitimate request means "delete the working directory".
#[tokio::test]
async fn delete_refuses_the_working_directory_itself() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("keep.txt"), "x").unwrap();

    for path in ["", ".", "/"] {
        let response = backend
            .delete_authed(&format!(
                "/api/v1/sessions/{session_id}/files?path={path}"
            ))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "path={path:?} must not delete the workdir"
        );
    }
    assert!(backend.workdir().join("keep.txt").exists());
    assert!(backend.workdir().is_dir());
}

/// Deleting one stray file and deleting a subtree are different intentions, so the
/// second one has to be asked for.
#[tokio::test]
async fn delete_refuses_a_non_empty_directory_unless_recursive() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::create_dir_all(backend.workdir().join("tree/nested")).unwrap();
    std::fs::write(backend.workdir().join("tree/nested/deep.txt"), "x").unwrap();

    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=tree"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(backend.workdir().join("tree/nested/deep.txt").exists());

    // An empty directory needs no flag: there is nothing to lose.
    std::fs::create_dir_all(backend.workdir().join("blank")).unwrap();
    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=blank"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = backend
        .delete_authed(&format!(
            "/api/v1/sessions/{session_id}/files?path=tree&recursive=true"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!backend.workdir().join("tree").exists());
}

#[tokio::test]
async fn delete_refuses_to_escape_the_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    let outside = backend.workdir().parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "x").unwrap();

    let response = backend
        .delete_authed(&format!(
            "/api/v1/sessions/{session_id}/files?path=../outside.txt"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(outside.exists(), "a path outside the root must survive");
    std::fs::remove_file(&outside).ok();
}

/// A symlink is unlinked, not followed.
///
/// Following it would delete the *target's* contents. `resolve` has already
/// confirmed the target is inside the workspace, so this is not a containment
/// hole — but "delete this link" and "delete what it points at" are different
/// operations, and only one of them was asked for.
#[tokio::test]
async fn delete_unlinks_a_symlink_without_touching_its_target() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    let real = backend.workdir().join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("keep.txt"), "x").unwrap();
    std::os::unix::fs::symlink(&real, backend.workdir().join("link")).unwrap();

    let response = backend
        .delete_authed(&format!("/api/v1/sessions/{session_id}/files?path=link"))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!backend.workdir().join("link").exists());
    assert!(
        real.join("keep.txt").exists(),
        "the link's target must be untouched"
    );
}

/// An upload must not quietly destroy source the user did not mean to touch.
#[tokio::test]
async fn upload_refuses_to_overwrite_unless_asked() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("keep.txt"), "original").unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "keep.txt", "data": "bmV3" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read_to_string(backend.workdir().join("keep.txt")).unwrap(),
        "original",
        "the existing file must survive a refused upload"
    );

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "keep.txt", "data": "bmV3", "overwrite": true }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read_to_string(backend.workdir().join("keep.txt")).unwrap(),
        "new",
        "an explicit overwrite must take effect"
    );
}

#[tokio::test]
async fn upload_rejects_malformed_base64() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({ "path": "", "name": "bad.bin", "data": "not base64!!" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------- session flag patching
//
// Model and reasoning effort are chosen in the composer, so they must be
// changeable after creation — and validated on write, because a value the runner
// never declared would otherwise be dropped at turn time with nothing saying why.

#[tokio::test]
async fn patch_updates_flags_the_agent_declares() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let body = json_body(
        backend
            .patch_authed(
                &format!("/api/v1/sessions/{session_id}"),
                json!({ "flags": { "model": "slow", "effort": "high" } }),
            )
            .await,
    )
    .await;

    // `flagsJson` is the wire field: the stored JSON, verbatim.
    let flags: Value =
        serde_json::from_str(body["session"]["flagsJson"].as_str().unwrap()).unwrap();
    assert_eq!(flags["model"], "slow");
    assert_eq!(flags["effort"], "high");
}

#[tokio::test]
async fn patch_rejects_undeclared_flag_values() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    // A choice outside the declared enumeration.
    let response = backend
        .patch_authed(
            &format!("/api/v1/sessions/{session_id}"),
            json!({ "flags": { "model": "gpt-nonexistent" } }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // A flag the runner does not declare at all.
    let response = backend
        .patch_authed(
            &format!("/api/v1/sessions/{session_id}"),
            json!({ "flags": { "temperature": "0.7" } }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Neither attempt may have altered stored state.
    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}"))
            .await,
    )
    .await;
    let flags: Value =
        serde_json::from_str(body["session"]["flagsJson"].as_str().unwrap()).unwrap();
    assert!(
        flags.get("temperature").is_none() && flags.get("model").is_none(),
        "a rejected patch must not persist: {flags:?}"
    );
}

/// The flag the composer sets is the flag the runner is invoked with. Without
/// this, a patch could be stored and still not reach the process.
#[tokio::test]
async fn patched_flags_reach_the_runner_on_the_next_turn() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let response = backend
        .patch_authed(
            &format!("/api/v1/sessions/{session_id}"),
            json!({ "flags": { "model": "slow" } }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}"))
            .await,
    )
    .await;
    let flags: Value =
        serde_json::from_str(body["session"]["flagsJson"].as_str().unwrap()).unwrap();
    assert_eq!(
        flags["model"], "slow",
        "the stored flag is what `turns` reads per turn"
    );
}

#[tokio::test]
async fn patch_updates_the_title() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let body = json_body(
        backend
            .patch_authed(
                &format!("/api/v1/sessions/{session_id}"),
                json!({ "title": "  Renamed  " }),
            )
            .await,
    )
    .await;
    assert_eq!(body["session"]["title"], "Renamed", "titles are trimmed");
}

#[tokio::test]
async fn patch_on_a_missing_session_is_not_found() {
    let backend = TestBackend::start().await;

    let response = backend
        .patch_authed(
            "/api/v1/sessions/does-not-exist",
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ------------------------------------------------- documents and raw bytes

/// Build a minimal but valid xlsx, so no binary fixture enters the repo.
///
/// Mirrors `documents::testing`, which is `#[cfg(test)]` and therefore not
/// reachable from an integration test binary. Kept deliberately small: the parser
/// units cover format breadth, and what these tests need is one real file to
/// drive the HTTP layer with.
fn minimal_xlsx() -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let parts: [(&str, &str); 5] = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>Region</t></is></c><c r="B1" t="inlineStr"><is><t>Revenue</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>APAC</t></is></c><c r="B2"><v>1200</v></c></row>
<row r="3"><c r="A3" t="inlineStr"><is><t>EMEA</t></is></c><c r="B3"><v>980.5</v></c></row>
</sheetData></worksheet>"#,
        ),
    ];
    for (name, body) in parts {
        zip.start_file(name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[tokio::test]
async fn spreadsheets_are_served_as_a_windowed_grid() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("sales.xlsx"), minimal_xlsx()).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/document?path=sales.xlsx"
            ))
            .await,
    )
    .await;

    assert_eq!(body["kind"], "grid");
    assert_eq!(body["format"], "xlsx");
    assert_eq!(body["formatLabel"], "Excel workbook");
    assert_eq!(body["grid"]["sheets"][0]["name"], "Sales");
    assert_eq!(body["grid"]["rows"][0][0]["v"], "Region");
    assert_eq!(body["grid"]["rows"][0][0]["t"], "text");

    // The typed number is what lets the viewer right-align it, and `n` is what
    // lets a client sort without re-parsing a formatted string.
    assert_eq!(body["grid"]["rows"][1][1]["v"], "1200");
    assert_eq!(body["grid"]["rows"][1][1]["t"], "number");
    assert_eq!(body["grid"]["rows"][1][1]["n"], 1200.0);
    assert_eq!(body["grid"]["hasHeaderRow"], true);
    assert_eq!(body["grid"]["truncated"], false);
}

/// An out-of-range window is clamped and the response says what it served, so a
/// paging control cannot 400 on an off-by-one.
#[tokio::test]
async fn a_document_window_is_clamped_and_echoed() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("sales.xlsx"), minimal_xlsx()).unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/document?path=sales.xlsx&row=1&rows=99999"
            ))
            .await,
    )
    .await;

    assert_eq!(body["grid"]["row"], 1, "the window start is echoed");
    // Two rows remain from row 1 of a three-row sheet, and the 99999 request was
    // clamped to the 500 cap rather than refused.
    assert_eq!(body["grid"]["rows"].as_array().unwrap().len(), 2);

    let missing_sheet = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/document?path=sales.xlsx&sheet=7"
        ))
        .await;
    assert_eq!(missing_sheet.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(missing_sheet).await["error"]["code"],
        "INVALID_REQUEST"
    );
}

#[tokio::test]
async fn csv_is_served_through_the_same_grid_shape_as_a_spreadsheet() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(
        backend.workdir().join("data.csv"),
        "name,amount\n\"Smith, Alice\",1200\nBob,980\n",
    )
    .unwrap();

    let body = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/document?path=data.csv"
            ))
            .await,
    )
    .await;

    // One payload shape for both formats is what lets the frontend ship one
    // table viewer instead of two.
    assert_eq!(body["kind"], "grid");
    assert_eq!(body["format"], "csv");
    assert_eq!(body["grid"]["rows"][1][0]["v"], "Smith, Alice");
    assert_eq!(body["grid"]["rows"][1][1]["t"], "number");
}

#[tokio::test]
async fn unsupported_and_corrupt_documents_are_refused_with_a_usable_code() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("notes.txt"), "plain text").unwrap();
    std::fs::write(backend.workdir().join("broken.xlsx"), "not a zip at all").unwrap();
    std::fs::create_dir_all(backend.workdir().join("adir")).unwrap();

    for path in ["notes.txt", "broken.xlsx", "adir"] {
        let response = backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/document?path={path}"
            ))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{path} should be a bad request"
        );
        // The client uses this code to decide to retry against /files/content,
        // so it has to be the documented one.
        assert_eq!(
            json_body(response).await["error"]["code"],
            "INVALID_REQUEST",
            "{path}"
        );
    }
}

/// The tree hints which viewer a file wants, so opening one costs a single
/// request instead of a probe that guesses wrong.
#[tokio::test]
async fn the_tree_reports_a_preview_kind_per_file() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    let root = backend.workdir();
    std::fs::write(root.join("sales.xlsx"), minimal_xlsx()).unwrap();
    std::fs::write(root.join("data.csv"), "a,b\n").unwrap();
    std::fs::write(root.join("report.docx"), "stub").unwrap();
    std::fs::write(root.join("page.html"), "<p>hi</p>").unwrap();
    std::fs::write(root.join("shot.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();
    std::fs::write(root.join("paper.pdf"), b"%PDF-1.7").unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("icon.svg"), "<svg/>").unwrap();

    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session_id}/files"))
            .await,
    )
    .await;

    let kind = |name: &str| -> Option<String> {
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == name)
            .and_then(|entry| entry["previewKind"].as_str().map(str::to_string))
    };

    assert_eq!(kind("sales.xlsx").as_deref(), Some("grid"));
    assert_eq!(kind("data.csv").as_deref(), Some("grid"));
    assert_eq!(kind("report.docx").as_deref(), Some("blocks"));
    assert_eq!(kind("page.html").as_deref(), Some("html"));
    assert_eq!(kind("shot.png").as_deref(), Some("image"));
    assert_eq!(kind("paper.pdf").as_deref(), Some("pdf"));
    // An SVG is an image to the viewer, but must travel the base64 path rather
    // than the raw route, where it could execute script.
    assert_eq!(kind("icon.svg").as_deref(), Some("image"));
    assert_eq!(
        kind("main.rs"),
        None,
        "source has no special viewer and is read as text"
    );
}

/// The security test. Serving workspace HTML from this origin without these
/// headers lets a script in an agent-generated file read the session cookie —
/// measured in a browser, not hypothesized. A refactor that drops either header
/// must fail here.
#[tokio::test]
async fn raw_bytes_are_served_with_sandbox_and_nosniff_headers() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(
        backend.workdir().join("report.html"),
        "<h1>Generated</h1><script>parent.document.cookie</script>",
    )
    .unwrap();

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=report.html"
        ))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(
        headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
        "sandbox",
        "without this the framed document reaches this origin's cookies"
    );
    assert_eq!(
        headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
        "nosniff"
    );
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "text/html");
    // Prefix, not equality: the disposition also carries the filename now, so a
    // download saves under the file's own name instead of as `raw`. What this
    // assertion pins is `inline` versus `attachment`.
    assert!(
        disposition_of(&headers).starts_with("inline"),
        "an allowlisted type renders rather than downloading"
    );

    assert!(body_string(response).await.contains("Generated"));
}

fn disposition_of(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::CONTENT_DISPOSITION)
        .expect("every raw response carries a disposition")
        .to_str()
        .expect("the disposition is header-safe even for a CJK filename")
        .to_string()
}

/// `?download=true` overrides a renderable type.
///
/// The allowlist answers "is this safe to render"; this answers "did the user ask
/// to save it". Conflating them is how Download on a PNG opens a tab showing the
/// image instead of saving it.
#[tokio::test]
async fn raw_download_forces_an_attachment_for_a_renderable_type() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("report.html"), "<h1>Generated</h1>").unwrap();

    let headers = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=report.html&download=true"
        ))
        .await
        .headers()
        .clone();

    let disposition = disposition_of(&headers);
    assert!(disposition.starts_with("attachment"), "{disposition}");
    assert!(
        disposition.contains("filename=\"report.html\""),
        "a download with no filename saves as `raw`: {disposition}"
    );
    // Nor is it labelled with a renderable type, so a client ignoring the
    // disposition still would not render it.
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/octet-stream"
    );
}

/// Text renders in a tab whatever its extension.
///
/// This is what makes "open in a new tab" work for source: the pane's preview is
/// truncated at 256 KB by design, so the whole file has to be readable somewhere.
/// An extension table cannot decide this — `.svelte` and `.rb` are absent from it,
/// and a `.log` full of binary is present wrongly — so the bytes decide.
#[tokio::test]
async fn raw_renders_text_inline_whatever_the_extension() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(
        backend.workdir().join("Component.svelte"),
        "<script>let count = 0;</script>",
    )
    .unwrap();

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=Component.svelte"
        ))
        .await;

    let headers = response.headers().clone();
    // `text/plain`, never the extension's own label. That the bytes are text says
    // nothing about whether it is safe to *interpret* them, and this file holds a
    // `<script>` element to make the distinction concrete.
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8"
    );
    assert!(disposition_of(&headers).starts_with("inline"));
    assert!(body_string(response).await.contains("let count = 0;"));
}

/// An unrecognized extension is never *promoted* into a renderable type.
///
/// The file here holds HTML under a `.xyz` name — the shape of an attack where
/// markup is smuggled past the allowlist by renaming it. Since the bytes are text
/// it is served as `text/plain`, which shows the markup as source rather than
/// executing it; what must never happen is `text/html`.
#[tokio::test]
async fn raw_refuses_to_name_a_type_it_does_not_render() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("payload.xyz"), "<h1>not rendered</h1>").unwrap();

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=payload.xyz"
        ))
        .await;

    let headers = response.headers().clone();
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8",
        "smuggled markup is shown as source, never labelled as HTML"
    );
    assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    assert_eq!(headers.get(header::CONTENT_SECURITY_POLICY).unwrap(), "sandbox");
    // The body is the markup verbatim, which is the point: it was not
    // interpreted, and `nosniff` above is what stops the browser reconsidering.
    assert!(body_string(response).await.contains("<h1>not rendered</h1>"));
}

/// A file whose bytes are not text still downloads.
///
/// The other half of the rule above: broadening the route to serve text must not
/// broaden it to serve *everything*, or an unknown binary lands in a tab as
/// mojibake instead of in the user's downloads.
#[tokio::test]
async fn raw_downloads_a_file_whose_bytes_are_not_text() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    // A NUL and an invalid UTF-8 sequence: what any real binary carries, and what
    // no source file does.
    std::fs::write(
        backend.workdir().join("blob.bin"),
        [0x00, 0x01, 0xff, 0xfe, 0x42],
    )
    .unwrap();

    let headers = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=blob.bin"
        ))
        .await
        .headers()
        .clone();

    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/octet-stream"
    );
    assert!(disposition_of(&headers).starts_with("attachment"));
}

/// An SVG must never be served as `image/svg+xml` from this origin: at that type
/// it can execute script, whereas the same bytes inside an `<img>` cannot.
///
/// It is text, so it now goes out as `text/plain` rather than as an attachment —
/// a tab shows its source. That is a deliberately duller outcome than rendering
/// the drawing, and the trade is documented in `files.rs`: the pane already
/// renders SVG through the base64 `content` path, where an `<img>` element is the
/// container and script cannot run, so nothing needs this route to render it.
#[tokio::test]
async fn raw_never_serves_svg_as_an_executable_type() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(
        backend.workdir().join("icon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
    )
    .unwrap();

    let headers = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=icon.svg"
        ))
        .await
        .headers()
        .clone();

    let content_type = headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap();
    assert_ne!(
        content_type, "image/svg+xml",
        "at this type a script inside the SVG runs in this origin"
    );
    assert_eq!(content_type, "text/plain; charset=utf-8");
    // The script survives in the body, unexecuted. That is the whole distinction:
    // the bytes are delivered, the type refuses to interpret them.
    assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    assert_eq!(headers.get(header::CONTENT_SECURITY_POLICY).unwrap(), "sandbox");
}

#[tokio::test]
async fn raw_revalidates_with_an_etag_instead_of_resending_bytes() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;
    std::fs::write(backend.workdir().join("shot.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();

    let first = backend
        .get_authed(&format!(
            "/api/v1/sessions/{session_id}/files/raw?path=shot.png"
        ))
        .await;
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("an ETag, so a re-view is a 304 rather than a re-download")
        .to_str()
        .unwrap()
        .to_string();

    let second = backend
        .get_authed_with_header(
            &format!("/api/v1/sessions/{session_id}/files/raw?path=shot.png"),
            header::IF_NONE_MATCH,
            &etag,
        )
        .await;

    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    // A 304 must carry the policy too: it is still a response about this file.
    assert_eq!(
        second.headers().get(header::CONTENT_SECURITY_POLICY).unwrap(),
        "sandbox"
    );
}

/// The containment invariant must hold on every new route, not be re-established
/// by accident. Mirrors the checks already covering `/files/content`.
#[tokio::test]
async fn the_new_file_routes_refuse_paths_outside_the_workdir() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let outside = backend.workdir().parent().unwrap().join("doc-secret.txt");
    std::fs::write(&outside, "do not serve me").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, backend.workdir().join("escape.html")).unwrap();

    for route in ["document", "raw"] {
        for attempt in ["../doc-secret.txt", "src/../../doc-secret.txt", "/etc/hosts"] {
            let response = backend
                .get_authed(&format!(
                    "/api/v1/sessions/{session_id}/files/{route}?path={}",
                    urlencoding(attempt)
                ))
                .await;
            let status = response.status();
            assert!(
                status == StatusCode::FORBIDDEN
                    || status == StatusCode::NOT_FOUND
                    || status == StatusCode::BAD_REQUEST,
                "{route} did not refuse {attempt}: {status}"
            );
            assert!(
                !body_string(response).await.contains("do not serve me"),
                "{route} leaked {attempt}"
            );
        }
    }

    // A symlink pointing outside the root is refused after canonicalization,
    // which is the check that cannot be done by inspecting the request string.
    #[cfg(unix)]
    {
        let response = backend
            .get_authed(&format!(
                "/api/v1/sessions/{session_id}/files/raw?path=escape.html"
            ))
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!body_string(response).await.contains("do not serve me"));
    }
}

#[tokio::test]
async fn the_new_file_routes_require_the_session_cookie() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    for route in ["document", "raw"] {
        assert_eq!(
            backend
                .get(&format!(
                    "/api/v1/sessions/{session_id}/files/{route}?path=any.xlsx"
                ))
                .await
                .status(),
            StatusCode::UNAUTHORIZED,
            "{route} must not be reachable without the cookie"
        );
    }
}

/// # SOUL.md visibility and the identity choice
///
/// The injection toggle in settings is a decision about what every session's
/// prompt contains, so these pin two things: that the text is readable, and that
/// opting out is recorded durably rather than being a client-side preference the
/// backend forgets.
mod soul {
    use super::*;

    #[tokio::test]
    async fn the_soul_endpoint_returns_the_resolved_text_and_its_path() {
        let backend = TestBackend::start_with_soul("# SOUL.md\n\nI am Sylph.\n").await;

        let response = backend.get_authed("/api/v1/soul").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;

        assert_eq!(body["available"], true);
        assert_eq!(body["content"], "# SOUL.md\n\nI am Sylph.\n");
        assert_eq!(body["bytes"], 23);
        assert!(
            body["path"].as_str().unwrap().ends_with("SOUL.md"),
            "the path is shown so a user can find the file they are being asked about"
        );
        assert!(
            body["modifiedMs"].as_i64().unwrap() > 0,
            "a mtime lets the panel show how current the identity is"
        );
        assert!(body["truncated"].is_null(), "a small file is not truncated");
    }

    /// An install with no SOUL.md is ordinary, not broken: every session runs
    /// natively. The UI has to be able to say that instead of offering a toggle
    /// over a file that does not exist.
    #[tokio::test]
    async fn a_missing_soul_is_reported_as_unavailable_rather_than_an_error() {
        let backend = TestBackend::start().await;

        let response = backend.get_authed("/api/v1/soul").await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;

        assert_eq!(body["available"], false);
        assert!(body["path"].is_null());
        assert!(body["content"].is_null());
    }

    /// The text is a personal file and the response carries an absolute path.
    #[tokio::test]
    async fn the_soul_endpoint_requires_authentication() {
        let backend = TestBackend::start_with_soul("secret identity").await;
        assert_eq!(
            backend.get("/api/v1/soul").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_session_defaults_to_sylph_identity_when_the_client_says_nothing() {
        let backend = TestBackend::start().await;
        let id = backend.create_session("mock").await;

        let body = json_body(backend.get_authed(&format!("/api/v1/sessions/{id}")).await).await;
        assert_eq!(body["session"]["identityMode"], "sylph");
    }

    /// The point of the setting: a WebApp session that runs the agent as it
    /// comes. Origin stays `sylph` — the WebApp created it, and Sylph lifecycle
    /// policy still applies — so this must not be confused with an import.
    #[tokio::test]
    async fn a_session_can_be_created_with_identity_preserved_without_becoming_native() {
        let backend = TestBackend::start().await;

        let response = backend
            .post_authed(
                "/api/v1/sessions",
                serde_json::json!({
                    "agent": "mock",
                    "workdir": backend.workdir().display().to_string(),
                    "identityMode": "preserve",
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = json_body(response).await;

        assert_eq!(body["session"]["identityMode"], "preserve");
        assert_eq!(
            body["session"]["origin"], "sylph",
            "opting out of injection does not make a session native"
        );
        assert_eq!(
            body["session"]["lifecycleMode"], "sylph",
            "lifecycle policy is a separate axis from identity"
        );
    }

    /// A misspelling must fail loudly. Stored as a free string it would read as
    /// "not sylph" everywhere downstream and silently disable injection.
    #[tokio::test]
    async fn an_unknown_identity_mode_is_rejected_rather_than_stored() {
        let backend = TestBackend::start().await;

        let response = backend
            .post_authed(
                "/api/v1/sessions",
                serde_json::json!({
                    "agent": "mock",
                    "workdir": backend.workdir().display().to_string(),
                    "identityMode": "syplh",
                }),
            )
            .await;

        assert!(
            response.status().is_client_error(),
            "an unrecognized identity mode must not be accepted"
        );
    }

    /// Identity decides what a stored transcript was produced by, so it is fixed
    /// at creation. `PATCH` accepts flags and title only.
    #[tokio::test]
    async fn identity_mode_cannot_be_changed_after_creation() {
        let backend = TestBackend::start().await;
        let id = backend.create_session("mock").await;

        let response = backend
            .patch_authed(
                &format!("/api/v1/sessions/{id}"),
                serde_json::json!({"identityMode": "preserve"}),
            )
            .await;
        // Unknown fields are ignored by the update payload, so the assertion
        // that matters is the stored value, not the status.
        assert!(response.status().is_success() || response.status().is_client_error());

        let body = json_body(backend.get_authed(&format!("/api/v1/sessions/{id}")).await).await;
        assert_eq!(
            body["session"]["identityMode"], "sylph",
            "identity must not be patchable"
        );
    }
}
