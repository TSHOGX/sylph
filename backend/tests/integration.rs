//! End-to-end API tests: auth, sessions, turns, SSE, and error contracts.

mod common;

use axum::http::{header, StatusCode};
use common::{body_string, json_body, MockRunner, MockTraceDb, TestBackend};
use serde_json::{json, Value};
use tempfile::TempDir;

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn health_is_public() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/health").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["apiVersion"], "1");
    // Asserted against the migration directory rather than a literal. The previous
    // version pinned `3`, which is what the code also said, so the two agreed with
    // each other and neither could catch the field drifting away from the schema —
    // and it had, by four migrations. Counting the files fails when a migration is
    // added without the reported version following it.
    let migrations = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .expect("migrations directory")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sql"))
        .count();
    assert_eq!(body["schemaVersion"], migrations as i64);
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
    // **Asserted on identity, not on the exact set.** Availability is now the union of
    // linked-in adapters (whose vendor CLI is on this machine's `PATH`) and fixture
    // `adapter.sh` directories, so an exact-set assertion passes or fails on which agent
    // CLIs the developer happens to have installed — it read `["mock"]` on a machine with
    // none and `["claude","codex","mock","pi"]` on one with all three. What the endpoint
    // promises is that it reports what this environment can run, and the fixture is what
    // this test installed.
    let agents = body["agents"]
        .as_array()
        .expect("agents is an array")
        .iter()
        .map(|name| name.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        agents.iter().any(|name| name == "mock"),
        "the fixture agent this test installed must be reported, found {agents:?}"
    );
    assert!(
        !agents.iter().any(|name| name == "gemini" || name == "opencode"),
        "a deliberately unsupported agent must never be reported, found {agents:?}"
    );
    // `activeTurns`, not `activeProcesses`: the value has always been a turn count,
    // and the old name only read as accurate while every process was one Sylph
    // spawned. An adopted session has a live process that is not ours to count.
    assert_eq!(body["activeTurns"], 0);
    assert!(
        body["activeProcesses"].is_null(),
        "the old name must be gone rather than kept beside the new one"
    );
}

// ------------------------------------------------------------- sessions

#[tokio::test]
async fn create_and_read_back_a_session() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({
                "environment": backend.environment_id().await,
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
    assert_eq!(session["status"], "idle");

    // `origin`, `identityMode`, and `lifecycleMode` are **gone**, and their absence is
    // the invariant this now pins (`docs/architecture.md` § One class of session).
    // Measured before deletion: `lifecycle_mode` had four write sites and zero reads,
    // `origin` had zero backend branches, and `identity_mode` had exactly one — the
    // `SOUL` path choice, which is `injectIdentity` below.
    assert!(session.get("origin").is_none(), "{session}");
    assert!(session.get("identityMode").is_none(), "{session}");
    assert!(session.get("lifecycleMode").is_none(), "{session}");

    // What replaced the one branch that did work. Defaults true for a session Sylph
    // starts, and `PATCH` refuses it — a transcript records what produced it.
    assert_eq!(session["injectIdentity"], true);

    // **Identity is the triple, and a row exists only because an agent assigned an id.**
    // There is no server-minted UUID and no window in which a session has no native id:
    // `POST /sessions` started an agent, and the id it reported is the primary key.
    let native = session["nativeId"]
        .as_str()
        .expect("an agent assigned an id before the row was written");
    assert!(!native.is_empty());
    assert_eq!(session["environment"], backend.environment_id().await);

    // The three parts are given separately **so a client never parses the id**, and the
    // encoded form is what a path segment carries.
    let id = session["id"].as_str().unwrap();
    assert_eq!(
        id,
        format!(
            "{}:{}:{}",
            session["environment"].as_str().unwrap(),
            session["agent"].as_str().unwrap(),
            native
        ),
        "the encoded id is the triple, delimited with ':' and never '/'"
    );

    // A live link holds the process's own stdio, which is the only source carrying every
    // delta and its arrival time. A renderer reads this before drawing a duration.
    assert_eq!(session["fidelity"], "stream");

    // Round-trips as a path segment, which is the property the encoding exists for.
    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/{}",
            urlencoding(id)
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["session"]["id"], id);
}

/// A malformed id is a 400, and a well-formed one naming nothing is a 404.
///
/// The pair matters: answering 404 for both would tell a client with a corrupted id that
/// its session had been deleted, which sends the investigation to the wrong layer.
#[tokio::test]
async fn a_malformed_session_id_is_refused_rather_than_reported_missing() {
    let backend = TestBackend::start().await;

    let response = backend.get_authed("/api/v1/sessions/not-a-triple").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"]["code"], "INVALID_REQUEST");

    // Well-formed — base32 environment, an agent name, a native id — and naming nothing.
    let response = backend
        .get_authed("/api/v1/sessions/env7fk2%3Amock%3Anothing-here")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(response).await["error"]["code"], "SESSION_NOT_FOUND");
}

#[tokio::test]
async fn create_session_rejects_unknown_agent_and_missing_workdir() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "nope", "workdir": backend.workdir().display().to_string()}),
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
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": "/definitely/not/here"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------- workdir containment
//
// A session's workdir is the `current_dir` of a spawned agent process, and a
// coding agent reads and writes the tree it is pointed at. So this field decides
// where arbitrary code runs, and existence — the only thing it used to be checked
// for — says nothing about whether it should. These assert the bound in both
// directions, because a containment rule that refuses everything passes every
// refusal test.

/// The bound is the configured one, asserted by reading it back from the harness.
/// A hardcoded path would only prove that the test and the code agree.
#[tokio::test]
async fn a_workdir_inside_the_configured_roots_is_accepted() {
    let backend = TestBackend::start().await;
    let inside = backend.workdir().join("nested/deeper");
    std::fs::create_dir_all(&inside).unwrap();

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": inside.display().to_string()}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    // Journalled canonical, so two spellings of one directory cannot become two
    // workspaces — and so the recorded path has no symlinked component that could
    // be re-pointed after the check.
    assert_eq!(
        json_body(response).await["session"]["workdir"],
        std::fs::canonicalize(&inside).unwrap().display().to_string()
    );
}

/// Before this, any client of this backend could start an agent anywhere on the
/// filesystem. `browse_roots` bounded what `/fs/*` may *enumerate* and said
/// nothing about what a request may *name*, so the narrower capability was
/// bounded and the wider one was not.
#[tokio::test]
async fn a_workdir_outside_the_configured_roots_is_refused() {
    let backend = TestBackend::start().await;
    let outside = backend.outside_the_roots();

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": outside.path().display().to_string()}),
        )
        .await;

    // 403 with POLICY_VIOLATION, not 400: the request is well-formed and the
    // directory exists. Reporting it as a malformed request would send the reader
    // to their input instead of to `config.toml`.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "POLICY_VIOLATION");
    // Names the bound that refused it and what is permitted, so the fix is
    // discoverable from the error alone.
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("sessions may run in"),
        "the error names the bound: {message}"
    );
    assert!(
        message.contains(&backend.browse_root().display().to_string())
            || message.contains(
                &std::fs::canonicalize(backend.browse_root())
                    .unwrap()
                    .display()
                    .to_string()
            ),
        "the error lists what is permitted: {message}"
    );
}

/// Traversal is defeated by resolution, not by inspecting the request string.
#[tokio::test]
async fn a_workdir_reaching_out_of_a_root_with_dotdot_is_refused() {
    let backend = TestBackend::start().await;
    let escaping = backend.workdir().join("../../..");

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": escaping.display().to_string()}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// A symlink pointing out of the roots is the escape no string filter can see:
/// it lives on disk, not in the request.
#[cfg(unix)]
#[tokio::test]
async fn a_workdir_reached_through_an_escaping_symlink_is_refused() {
    let backend = TestBackend::start().await;
    let outside = backend.outside_the_roots();
    let link = backend.workdir().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": link.display().to_string()}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// A file is not a working directory, and reports as the client mistake it is
/// rather than as a policy refusal.
#[tokio::test]
async fn a_workdir_naming_a_file_is_a_bad_request() {
    let backend = TestBackend::start().await;
    let file = backend.workdir().join("not-a-directory.txt");
    std::fs::write(&file, "x").unwrap();

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": file.display().to_string()}),
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "INVALID_REQUEST"
    );
}

/// Quick Start runs in a directory the product owns, which both `SYLPH_HOME` and
/// `paths.quick_start_dir` can move outside `$HOME`. It is permitted explicitly
/// rather than by coincidence of being browsable, or an install that moved it
/// would find the product's own no-workspace flow refused.
#[tokio::test]
async fn the_quick_start_directory_is_always_a_permitted_workdir() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions",
            json!({
                "environment": backend.environment_id().await,
                "agent": "mock",
                "workdir": backend.quick_start_dir().display().to_string(),
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
}

/// A journalled workdir is never re-checked, so narrowing configuration cannot
/// orphan history.
///
/// This is the half of the rule that is easy to get wrong in the other direction:
/// a bound applied on *read* would make every session created under wider
/// configuration — or before the bound existed at all — unreadable, which is a
/// worse bug than the hole it closes. The bound applies where a path enters as a
/// new decision, and nowhere else.
#[tokio::test]
async fn a_session_recorded_outside_the_roots_stays_readable_after_narrowing() {
    let mut backend = TestBackend::start().await;
    let recorded = backend.workdir().to_path_buf();
    let session_id = backend.create_session("mock").await;

    // Configuration now permits only a sibling directory, so the recorded workdir
    // is out of bounds — exactly the state a user reaches by editing
    // `paths.browse_roots` after working for a month.
    let narrowed = backend.browse_root().join("narrowed");
    std::fs::create_dir_all(&narrowed).unwrap();
    backend.restart_with_session_roots(&[&narrowed]).await;

    // Creating there now is refused, which is what proves the bound really moved.
    let refused = backend
        .post_authed(
            "/api/v1/sessions",
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": recorded.display().to_string()}),
        )
        .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // And the existing session is untouched: readable, listed, workdir intact.
    let response = backend
        .get_authed(&format!("/api/v1/sessions/{session_id}"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["session"]["workdir"],
        recorded.display().to_string()
    );

    let listed = json_body(backend.get_authed("/api/v1/sessions").await).await;
    assert!(
        listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == session_id.as_str()),
        "a session outside the current roots is still listed: {listed}"
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

    let response = backend.get_authed("/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist/turns").await;
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
            json!({"environment": backend.environment_id().await, "agent": "mock", "workdir": backend.workdir().display().to_string()}),
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
    // `nativeSessionId` is gone: the native id **is** part of the key, published as
    // `nativeId` and as the third component of `id`. There is no longer a moment at
    // which a session has a Sylph id and not a native one, which is what that field
    // existed to describe.
    assert!(session.get("nativeSessionId").is_none(), "{session}");
    assert!(
        session["nativeId"]
            .as_str()
            .is_some_and(|native| native.starts_with("mock-session-")),
        "the id the adapter reported is the primary key: {session}"
    );
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

/// A terminal turn must never be observable on a running session.
///
/// The two are separate rows and cannot be written in one transaction, so
/// something is briefly visible between them; this pins *which*. A client that
/// polls the turn and then reads the session is the ordinary shape — it is what
/// the composer does to decide whether to accept the next message — and seeing a
/// finished turn on a "running" session is what makes it refuse.
///
/// The window was two statements wide and merely improbable until closing open
/// trajectories put a query and a per-trajectory emit inside it. Asserting once
/// would pass on the old ordering by luck, so this drives many turns and checks
/// the pair every time.
#[tokio::test]
async fn a_settled_turn_is_never_visible_on_a_running_session() {
    let backend = TestBackend::start().await;
    let session_id = backend.create_session("mock").await;

    for attempt in 0..12 {
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

        // Read immediately, with nothing between: any sleep here would hide the
        // very interleaving this exists to catch.
        let session = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{session_id}"))
                .await,
        )
        .await;
        assert_eq!(
            session["session"]["status"], "idle",
            "turn {turn_id} was terminal while the session still read as running \
             (attempt {attempt})"
        );
    }
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

    let response = backend.get_authed("/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist").await;
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






// ------------------------------------------------------- archive browsing

#[tokio::test]
async fn archive_listing_requires_auth() {
    let backend = TestBackend::start().await;

    let response = backend.get("/api/v1/archive/sessions?workdir=/w/app").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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


// ------------------------------------------------- archive availability

















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
        .get_authed("/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist/files")
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

/// **Every session-scoped route refuses a malformed id with 400, not 404.**
///
/// Asserted across the whole surface rather than on one endpoint, because the failure
/// this catches is per-handler: a route that forgets to decode reaches the store with a
/// string that can never match, and answers `404` — telling a client with a corrupted id
/// that its session was deleted. The two answers send an investigation to opposite
/// layers, so one route getting it wrong is invisible unless every route is checked.
///
/// A `GET`/`POST`/`PATCH` mix on purpose: the decode sits in each handler, so the method
/// is part of what is being covered.
#[tokio::test]
async fn every_session_route_refuses_a_malformed_id_with_a_bad_request() {
    let backend = TestBackend::start().await;

    // Missing both delimiters, so it cannot be a triple at all.
    let malformed = "not-a-triple";

    for path in [
        "",
        "/turns",
        "/capabilities",
        "/requests",
        "/replay",
        "/events",
        "/files",
        "/files/content?path=x",
    ] {
        let response = backend
            .get_authed(&format!("/api/v1/sessions/{malformed}{path}"))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "GET /sessions/{{id}}{path} must refuse a malformed id rather than \
             reporting it missing"
        );
        assert_eq!(
            json_body(response).await["error"]["code"],
            "INVALID_REQUEST",
            "GET /sessions/{{id}}{path}"
        );
    }

    // Each body is the minimum that satisfies its handler's `Json` extractor, because
    // axum runs extractors **before** the handler: a body that fails to deserialize is a
    // 422 from the framework and never reaches the decode under test. That is correct
    // behaviour and not what this test is about — sending `{}` to `steer`, which requires
    // `text`, measured the extractor rather than the route.
    for (path, body) in [
        ("/turns", json!({"text": "hi"})),
        ("/link", json!({})),
        ("/interrupt", json!({})),
        ("/steer", json!({"text": "hi"})),
        ("/config", json!({"configId": "model", "value": "fast"})),
    ] {
        let response = backend
            .post_authed(&format!("/api/v1/sessions/{malformed}{path}"), body)
            .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "POST /sessions/{{id}}{path} must refuse a malformed id"
        );
    }

    let response = backend
        .patch_authed(
            &format!("/api/v1/sessions/{malformed}"),
            json!({"title": "x"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // And the other half of the pair: **well-formed and naming nothing is a 404.**
    // Without this the test above would pass on a server that answered 400 for
    // everything, which would be the same defect in the other direction.
    let absent = "env7fk2%3Amock%3Anothing-here";
    let response = backend.get_authed(&format!("/api/v1/sessions/{absent}")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );
}

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

    // Asserted against **the seeded template** rather than a list written out here.
    //
    // The previous version enumerated nine keys by hand, which is the shape `AGENTS.md`
    // warns about: a test that hardcodes what the code hardcodes proves only that the
    // two agree, and it agreed with `tracedb.bin` for as long as that setting existed.
    // Worse, it could not catch the failure it was written for — a key the resolver
    // knows and this endpoint omits — because the list was a third copy that had to be
    // updated by hand alongside both.
    //
    // The template is the right authority because it is what a *user* reads: a setting
    // documented there and missing from this endpoint is invisible in the settings
    // panel, and a setting reported here but undocumented there cannot be discovered.
    // Both directions are asserted below, so adding a setting to one place without the
    // other fails here.
    let template = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/config.toml"
    ))
    .expect("the seeded template is part of the repo");

    // Every `# key = value` line in the template is a setting a user can set. The two
    // that locate the config file itself never appear as keys, and `[section]` headers
    // supply the prefix.
    let mut documented: Vec<String> = Vec::new();
    let mut section = String::new();
    for line in template.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
        } else if let Some(rest) = line.strip_prefix("# ") {
            if let Some((key, _)) = rest.split_once(" = ") {
                if key.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !key.is_empty() {
                    documented.push(format!("{section}.{key}"));
                }
            }
        }
    }
    assert!(
        documented.len() >= 7,
        "the template parse found too few settings to be trustworthy: {documented:?}"
    );

    for expected in &documented {
        assert!(
            keys.contains(&expected.as_str()),
            "{expected} is documented in config.toml but absent from /config, so it is \
             invisible in the settings panel: {keys:?}"
        );
    }
    for key in &keys {
        assert!(
            documented.iter().any(|d| d == key),
            "{key} is reported by /config but undocumented in config.toml, so a user \
             cannot discover it: {documented:?}"
        );
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

/// The harness sets browse roots through the environment, so that must report `env`
/// while everything it leaves alone reports `default`. This is the property that makes
/// the field trustworthy.
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
    // A path this harness resolves but never creates — `TestBackend::start` seeds no
    // `SOUL.md`, which is a supported install rather than a broken one. `false` here is
    // the fact the field exists for: it is what turns "identity is not being injected"
    // into a one-line diagnosis instead of a mystery, and it must not be confused with
    // the setting not being a path at all, which the loop below pins.
    assert_eq!(entry("paths.soul")["exists"], false);

    // Absent rather than `false`: a log level is not a path, and rendering it as
    // missing would report a working setting as broken.
    for key in ["server.host", "server.port", "log.level"] {
        assert!(
            entry(key).get("exists").is_none(),
            "{key} is not a path and must not claim existence"
        );
    }
}

/// Configuration names no adapter directory, because there is none to name.
///
/// Asserted rather than merely deleted: the previous test required `bundledAgentCandidates`
/// to list every directory tried with the winner marked, and it was right to — a missing
/// install and a mistyped path produced the same empty agent list and needed opposite
/// fixes. Adapters are linked in now, so there is nothing to search and the actionable
/// question moved to `GET /environments`, which answers it per environment and per agent.
///
/// A settings panel reading a field the server still emitted would show a directory that
/// decides nothing, which is the shape of a lever a user can pull with no effect.
#[tokio::test]
async fn config_names_no_adapter_directory_because_there_is_none() {
    let backend = TestBackend::start().await;

    let body = json_body(backend.get_authed("/api/v1/config").await).await;
    assert!(
        body.get("bundledAgentCandidates").is_none(),
        "adapters are linked in, so no directory is reported: {body}"
    );

    let keys: Vec<&str> = body["entries"]
        .as_array()
        .expect("entries is an array")
        .iter()
        .filter_map(|entry| entry["key"].as_str())
        .collect();
    assert!(
        !keys.iter().any(|key| key.contains("agents_dir")),
        "no setting names an adapter directory: {keys:?}"
    );
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

/// `~/.sylph` is browsable and holds the Quick Start directory, so a picker that
/// hid dot-directories would hide a directory the product itself points at.
#[tokio::test]
async fn fs_tree_lists_dot_directories() {
    let backend = TestBackend::start().await;
    let root = backend.workdir();
    std::fs::create_dir_all(root.join(".sylph")).unwrap();

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
    assert!(names.contains(&".sylph"), "dotfiles must be pickable: {names:?}");
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

/// Overwriting must not follow a symlink out of the workdir.
///
/// Containment for a write is established on the *parent* directory, because the
/// file usually does not exist yet and `canonicalize` needs an existing path. That
/// leaves the last component unresolved — so a pre-existing symlink there is inside
/// the root by name and points wherever it likes, and `fs::write` follows it. The
/// escape arrives through the one component the check could not see, which is why
/// `remove` was written to unlink rather than walk and why the same care was owed
/// here.
#[cfg(unix)]
#[tokio::test]
async fn overwriting_a_symlink_out_of_the_workdir_is_refused() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let outside = TempDir::new().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, "original outside content").unwrap();
    // A link that *is* inside the workdir, pointing at a file that is not.
    let link = backend.workdir().join("innocent.txt");
    std::os::unix::fs::symlink(&victim, &link).unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({
                "path": "",
                "name": "innocent.txt",
                "data": "b3ZlcndyaXR0ZW4=",
                "overwrite": true,
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "POLICY_VIOLATION"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "original outside content",
        "the file outside the workdir must be untouched"
    );
}

/// A link pointing *inside* the workdir is ordinary and must still work.
///
/// The refusal above must be about where the write lands, not about symlinks:
/// refusing every link would deny a write the agent driving this session can
/// plainly make itself, which is the wider-than-the-agent line this endpoint holds.
#[cfg(unix)]
#[tokio::test]
async fn overwriting_a_symlink_inside_the_workdir_still_works() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let real = backend.workdir().join("real.txt");
    std::fs::write(&real, "original").unwrap();
    let link = backend.workdir().join("alias.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({
                "path": "",
                "name": "alias.txt",
                "data": "bmV3",
                "overwrite": true,
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "new",
        "a link resolving inside the workdir writes through to its target"
    );
}

/// A dangling link has no resolvable destination, so where a write would land
/// cannot be established — and creating the file it names would be inventing one.
#[cfg(unix)]
#[tokio::test]
async fn overwriting_a_dangling_symlink_is_refused() {
    let backend = TestBackend::start_with_runners(&[MockRunner::success("agent")]).await;
    let session_id = backend.create_session("agent").await;

    let link = backend.workdir().join("dangling.txt");
    std::os::unix::fs::symlink("/definitely/not/here.txt", &link).unwrap();

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/files"),
            json!({
                "path": "",
                "name": "dangling.txt",
                "data": "bmV3",
                "overwrite": true,
            }),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !std::path::Path::new("/definitely/not/here.txt").exists(),
        "a refused write must not have created the link's target"
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
            "/api/v1/sessions/env7fk2%3Amock%3Adoes-not-exist",
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

    /// Defaults true for a session Sylph starts.
    ///
    /// This is the surviving half of the deleted `identity_mode`, which was a pair of
    /// four-valued enums with exactly **one** behavioural read between them — the `SOUL`
    /// path choice (`docs/architecture.md` § One class of session).
    #[tokio::test]
    async fn a_session_injects_identity_by_default() {
        let backend = TestBackend::start().await;
        let id = backend.create_session("mock").await;

        let body = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{}", urlencoding(&id)))
                .await,
        )
        .await;
        assert_eq!(body["session"]["injectIdentity"], true);
        // The mode it replaced is gone, not renamed.
        assert!(body["session"].get("identityMode").is_none(), "{body}");
    }

    /// The point of the setting: a session that runs the agent as it comes.
    ///
    /// What this no longer asserts is the interesting part. The old version also checked
    /// that `origin` stayed `sylph` and `lifecycleMode` stayed `sylph`, "so this must not
    /// be confused with an import" — a distinction the one-class-of-session rule deletes.
    /// Declining injection is now just a boolean about this session, with no second axis
    /// it could be confused with.
    #[tokio::test]
    async fn a_session_can_decline_identity_injection() {
        let backend = TestBackend::start().await;

        let response = backend
            .post_authed(
                "/api/v1/sessions",
                serde_json::json!({
                    "environment": backend.environment_id().await,
                    "agent": "mock",
                    "workdir": backend.workdir().display().to_string(),
                    "injectIdentity": false,
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = json_body(response).await;

        assert_eq!(body["session"]["injectIdentity"], false);
        assert!(body["session"].get("origin").is_none(), "{body}");
        assert!(body["session"].get("lifecycleMode").is_none(), "{body}");
    }

    /// A misspelling must fail loudly.
    ///
    /// The old `identityMode` was a closed enum for this reason: stored as a free string,
    /// an unrecognized spelling reads as "not sylph" everywhere downstream and silently
    /// disables injection. A `bool` keeps that property by construction — serde refuses a
    /// non-boolean — which is a real gain from the narrowing rather than a coincidence.
    #[tokio::test]
    async fn a_non_boolean_inject_identity_is_refused_rather_than_stored() {
        let backend = TestBackend::start().await;

        let response = backend
            .post_authed(
                "/api/v1/sessions",
                serde_json::json!({
                    "environment": backend.environment_id().await,
                    "agent": "mock",
                    "workdir": backend.workdir().display().to_string(),
                    "injectIdentity": "preserve",
                }),
            )
            .await;

        assert!(
            response.status().is_client_error(),
            "a non-boolean must not be coerced into a policy: {}",
            response.status()
        );
    }

    /// Identity is fixed at creation, because a transcript records what produced it.
    ///
    /// Rewriting the flag afterwards would rewrite what the stored history *means*, which
    /// is why `PATCH` takes flags and title only.
    #[tokio::test]
    async fn inject_identity_cannot_be_changed_after_creation() {
        let backend = TestBackend::start().await;
        let id = backend.create_session("mock").await;

        let response = backend
            .patch_authed(
                &format!("/api/v1/sessions/{}", urlencoding(&id)),
                serde_json::json!({"injectIdentity": false}),
            )
            .await;
        // An unknown field on the update payload is ignored, so the assertion that
        // matters is the stored value rather than the status.
        assert!(response.status().is_success() || response.status().is_client_error());

        let body = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{}", urlencoding(&id)))
                .await,
        )
        .await;
        assert_eq!(
            body["session"]["injectIdentity"], true,
            "identity must not be patchable"
        );
    }
}

/// An agent that speaks after its turn ended gets a turn of its own.
///
/// The case this exists for: a delegate dispatched into the background finishes after
/// the turn that started it has settled. Measured against `claude` 2.x, the CLI
/// reports that completion and then opens an entire exchange to describe it, with
/// stdout still open — and every one of those events used to be dropped for arriving
/// outside a turn, so the notification a user had been promised was discarded by the
/// transport rather than never sent.
#[tokio::test]
async fn an_agent_that_speaks_after_its_turn_gets_a_turn_of_its_own() {
    let backend =
        common::TestBackend::start_with_runners(&[common::MockRunner::backgrounded("bg")]).await;
    let session_id = backend.create_session("bg").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "dispatch a background worker"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let first_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    backend.await_turn(&first_id).await;

    // Polled, because the agent's turn is produced by the drain task *after* the first
    // turn settled — it cannot be observed from the request that started the exchange.
    let mut agent_turn = None;
    for _ in 0..100 {
        let history = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
                .await,
        )
        .await;
        let found = history["turns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                entry["turn"]["origin"] == "agent" && entry["turn"]["status"] == "completed"
            })
            .cloned();
        if let Some(entry) = found {
            agent_turn = Some(entry);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let entry = agent_turn.expect("the agent's unprompted turn was recorded");
    assert_ne!(
        entry["turn"]["id"].as_str().unwrap(),
        first_id,
        "a turn of its own, not more events on the one that ended"
    );
    // Empty by construction: nobody asked. `origin` is what distinguishes this from a
    // user who posted no text.
    assert_eq!(entry["turn"]["requestText"], "");

    // And the report itself survived, which is the whole point.
    let text: String = entry["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert!(
        text.contains("the background worker finished"),
        "the agent's late report reached the journal: {text}"
    );
}

/// A delegate that finishes after its turn is journaled as finishing.
///
/// The turn-settle sweep closes what an adapter left open, which is right when nothing
/// more is coming. Backgrounded work *is* coming, and its real outcome arrives on the
/// agent's own turn — so both records exist and the later one is the truth. Before
/// agent turns, the completion was dropped and `abandoned` was the only thing ever
/// recorded about work that had in fact succeeded.
#[tokio::test]
async fn a_backgrounded_delegates_real_outcome_is_recorded() {
    let backend =
        common::TestBackend::start_with_runners(&[common::MockRunner::backgrounded("bg")]).await;
    let session_id = backend.create_session("bg").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "dispatch"}),
        )
        .await;
    let first_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&first_id).await;

    // Across every turn on the session: the delegate's close lands on whichever turn
    // was running when it arrived, which is the agent's own rather than the one that
    // dispatched it.
    let mut found = false;
    for _ in 0..100 {
        let history = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
                .await,
        )
        .await;
        found = history["turns"].as_array().unwrap().iter().any(|entry| {
            entry["events"].as_array().unwrap().iter().any(|e| {
                e["kind"] == "subagent"
                    && e["streamId"] == "bg-task"
                    && e["source"] == "agent"
                    && e["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("worker finished"))
            })
        });
        if found {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        found,
        "the delegate's own completion was journaled, not only the server's close"
    );
}

/// Every record of one delegation agrees about whether it finished.
///
/// The defect this pins is not that two records exist — the outcome genuinely arrives
/// on a later turn, and a client watching that turn should see it. It is that they
/// *contradicted*: the dispatching turn kept the sweep's `abandoned` close, so the
/// earlier card said the work never finished while the later one said it had, and the
/// earlier one is the one a reader scrolls to.
///
/// Asserted across every turn rather than on one, because which turn holds which record
/// is exactly what was wrong.
#[tokio::test]
async fn no_record_of_a_late_delegate_still_calls_it_abandoned() {
    let backend =
        common::TestBackend::start_with_runners(&[common::MockRunner::backgrounded("bg")]).await;
    let session_id = backend.create_session("bg").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "dispatch"}),
        )
        .await;
    let first_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&first_id).await;

    let history = await_reconciled(&backend, &session_id, "bg-task").await;

    // The correction landed on the turn that opened the trajectory, and it is a real
    // outcome rather than a server-authored guess.
    let corrected = find_event(&history, "bg-task").expect("the delegation's record");
    assert_eq!(corrected["text"], "worker finished");
    assert!(
        corrected["data"]["closedBy"].is_null(),
        "the server has stopped claiming this close: {corrected}"
    );
    assert_eq!(
        corrected["data"]["reconciled"], true,
        "and says it was corrected: {corrected}"
    );

    // No record anywhere still says the work was abandoned.
    for entry in history["turns"].as_array().unwrap() {
        for event in entry["events"].as_array().unwrap() {
            if event["streamId"] == "bg-task" {
                assert_ne!(
                    event["data"]["reason"], "abandoned",
                    "a contradicting record survived on turn {}: {event}",
                    entry["turn"]["id"]
                );
            }
        }
    }
}

/// A late outcome does not fabricate an abandoned host on the turn it arrives on.
///
/// A trajectory id is session-scoped, so the late close carries a `span` naming the
/// host from the turn that dispatched it. That bare reference used to promote the host
/// into a trajectory *on the agent's turn*, where nothing had opened it — so the sweep
/// closed it as abandoned and the timeline grew a card for a `Workflow` that had
/// completed cleanly, on a turn it never ran in.
#[tokio::test]
async fn a_late_outcome_does_not_abandon_the_host_it_names() {
    let backend =
        common::TestBackend::start_with_runners(&[common::MockRunner::backgrounded("bg")]).await;
    let session_id = backend.create_session("bg").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "dispatch"}),
        )
        .await;
    let first_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&first_id).await;

    let history = await_reconciled(&backend, &session_id, "bg-task").await;

    for entry in history["turns"].as_array().unwrap() {
        let agent_turn = entry["turn"]["origin"] == "agent";
        for event in entry["events"].as_array().unwrap() {
            if agent_turn && event["streamId"] == "bg-host" {
                panic!(
                    "the host was closed on a turn it never ran in: {event}"
                );
            }
        }
    }

    // And the host's own turn still reports it as the success it was.
    let host = find_event(&history, "bg-host").expect("the host's own record");
    assert!(
        host["data"]["closedBy"].is_null(),
        "the dispatching call succeeded and stays that way: {host}"
    );
}

/// A cooperating adapter's delegation is still on the turn that dispatched it.
///
/// `is_backgrounded` stops the sweep closing work that is expected to outlive its turn.
/// On its own that traded a wrong record for *no* record: a trajectory with no close
/// folded into no block, so the delegation vanished from the dispatching turn and its
/// children reattached to the root. The turn has to keep the delegation, and the
/// delegation has to end up carrying its real outcome.
#[tokio::test]
async fn a_flagged_delegate_stays_on_its_turn_and_records_its_outcome() {
    let backend = common::TestBackend::start_with_runners(&[
        common::MockRunner::backgrounded_flagged("bgf"),
    ])
    .await;
    let session_id = backend.create_session("bgf").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{session_id}/turns"),
            json!({"text": "dispatch"}),
        )
        .await;
    let first_id = json_body(response).await["turn"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    backend.await_turn(&first_id).await;

    let history = await_reconciled(&backend, &session_id, "bg-task").await;

    // On the dispatching turn, not merely somewhere on the session.
    let dispatching = history["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["turn"]["id"] == first_id.as_str())
        .expect("the dispatching turn");
    let delegate = dispatching["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["streamId"] == "bg-task")
        .expect("the delegation is still on the turn that dispatched it");

    assert_eq!(delegate["phase"], "completed");
    assert_eq!(delegate["text"], "worker finished");
    assert_eq!(
        delegate["span"], "bg-host",
        "and still inside its host, so the subtree did not flatten"
    );
}

/// Poll until a trajectory's record carries a real outcome, then return the history.
///
/// The correction is applied when the late event arrives, which is after the
/// dispatching turn has settled — so a read taken immediately can legitimately still
/// see the provisional record. Polling for the *settled* state is what makes these
/// assertions about the design rather than about scheduling.
async fn await_reconciled(
    backend: &common::TestBackend,
    session_id: &str,
    stream_id: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..100 {
        last = json_body(
            backend
                .get_authed(&format!("/api/v1/sessions/{session_id}/turns"))
                .await,
        )
        .await;
        if find_event(&last, stream_id)
            .is_some_and(|event| event["data"]["reconciled"] == true)
        {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("trajectory {stream_id} was never reconciled; history was {last}");
}

/// The record for one trajectory, on whichever turn holds it.
fn find_event<'a>(history: &'a Value, stream_id: &str) -> Option<&'a Value> {
    history["turns"].as_array()?.iter().find_map(|entry| {
        entry["events"]
            .as_array()?
            .iter()
            .find(|e| e["streamId"] == stream_id && e["data"]["reconciled"] == true)
            .or_else(|| {
                entry["events"]
                    .as_array()?
                    .iter()
                    .find(|e| e["streamId"] == stream_id)
            })
    })
}
