//! Shared test harness.
//!
//! Each test gets an isolated backend: a fresh temp directory holding the
//! SQLite journal, staged attachments, and a mock runners directory. The router
//! is driven in-process via `oneshot`, so no port is bound and tests can run
//! concurrently.
//!
//! The mock runner is a real script implementing the runner protocol, which
//! keeps the supervisor under test on its production path — spawning a process,
//! reading NDJSON from its stdout, parsing the final envelope — without paying
//! for a model call.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sylph_web_backend::{config::Config, router, AppState};
use tempfile::TempDir;
use tower::ServiceExt;

pub struct TestBackend {
    router: Router,
    /// Held for the lifetime of the test: dropping it deletes the journal and
    /// every staged attachment.
    temp: TempDir,
    workdir: PathBuf,
    cookie: String,
    /// Kept so a test can rebuild the backend against the same journal, which
    /// is how a restart is simulated.
    config: Config,
    /// The live state behind the router.
    ///
    /// Exposed because some state has no endpoint that creates it: an agent
    /// request is raised by a link's drain task, so a test for the *resolve* API
    /// has to seed one directly. Driving a real adapter to produce one would make
    /// these tests depend on a live agent process to exercise a store guarantee.
    pub state: AppState,
}

impl TestBackend {
    /// Start a backend with the default mock runner (`mock`), which streams a
    /// short thinking/assistant sequence and exits successfully.
    pub async fn start() -> Self {
        Self::start_with_runners(&[MockRunner::success("mock")]).await
    }

    /// Start a backend whose runners directory contains exactly `runners`.
    pub async fn start_with_runners(runners: &[MockRunner]) -> Self {
        Self::start_with(runners, None, None).await
    }

    /// Start a backend wired to a stub `trace-db api` process.
    ///
    /// The stub speaks the real line protocol so the client stays on its
    /// production path: one JSON request per line in, one response line out.
    pub async fn start_with_tracedb(tracedb: MockTraceDb) -> Self {
        Self::start_with(&[MockRunner::success("mock")], Some(tracedb), None).await
    }

    /// Start a backend whose resolved `SOUL.md` holds `text`.
    ///
    /// The file must exist before [`Config`] is built, because resolution
    /// requires a readable file — the same order production runs in, where
    /// `SOUL.md` predates the process.
    pub async fn start_with_soul(text: &str) -> Self {
        Self::start_with(&[MockRunner::success("mock")], None, Some(text)).await
    }

    async fn start_with(
        runners: &[MockRunner],
        tracedb: Option<MockTraceDb>,
        soul: Option<&str>,
    ) -> Self {
        let temp = TempDir::new().expect("failed to create temp dir");
        let root = temp.path();

        if let Some(text) = soul {
            std::fs::write(root.join("SOUL.md"), text).unwrap();
        }

        let data_dir = root.join("data");
        let runners_dir = root.join("runners");
        let workdir = root.join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&runners_dir).unwrap();
        std::fs::create_dir_all(&workdir).unwrap();

        for runner in runners {
            runner.install(&runners_dir);
        }

        let config = Config {
            host: "127.0.0.1".to_string(),
            port: 0,
            database_path: data_dir.join("live.db").display().to_string(),
            data_dir,
            // v2 adapters live beside the v1 runners in the harness tree, so a
            // test that installs one gets it discovered without a second root to
            // set up.
            agents_dir: runners_dir.clone(),
            runners_dir,
            // Absent unless a test opts in: TraceDB-dependent paths must
            // degrade rather than make every test depend on an external binary.
            tracedb_bin: match &tracedb {
                Some(stub) => stub.install(root),
                None => root.join("nonexistent-trace-db"),
            },
            // The harness root, not `$HOME`: browsing tests must resolve inside
            // the throwaway tree so they assert the real containment rule
            // without depending on the developer's home directory.
            browse_roots: vec![root.to_path_buf()],
            // Resolved the way production does — `<runners_dir>/../SOUL.md`,
            // which is the harness root — but only when a test wrote one. Left
            // `None` otherwise, so identity-injection tests opt in and every
            // other test keeps running with no soul, as it did before.
            soul_path: {
                let candidate = root.join("SOUL.md");
                candidate.is_file().then_some(candidate)
            },
            cookie_secret: "test-secret-token".to_string(),
        };

        let state = AppState::build(config.clone())
            .await
            .expect("failed to build state");
        let cookie = format!("sylph_session={}", state.config.cookie_secret);

        Self {
            router: router(state.clone()),
            temp,
            workdir,
            cookie,
            config,
            state,
        }
    }

    /// Rebuild the backend against the same journal, as a restart would.
    ///
    /// Child processes do not survive a real restart, so anything the previous
    /// process left mid-flight is gone. This is the only way to exercise the
    /// startup recovery that reconciles those rows.
    pub async fn restart(&mut self) {
        let state = AppState::build(self.config.clone())
            .await
            .expect("failed to rebuild state");
        self.router = router(state.clone());
        // Replaced too, or a test would keep querying the pre-restart store and
        // miss exactly the recovery it is asserting on.
        self.state = state;
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The configured browse root — the throwaway tree's top level.
    ///
    /// Distinct from [`Self::workdir`], which is a directory *inside* it. Tests
    /// about the browse boundary need the root itself, because "has no parent to
    /// offer" is only true at a root.
    pub fn browse_root(&self) -> &Path {
        self.temp.path()
    }

    /// A Cookie header value that passes auth.
    pub fn cookie(&self) -> &str {
        &self.cookie
    }

    async fn send(&self, request: Request<Body>) -> Response<Body> {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("router call failed")
    }

    pub async fn get(&self, path: &str) -> Response<Body> {
        self.send(
            Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn get_authed(&self, path: &str) -> Response<Body> {
        self.send(
            Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// An authenticated GET carrying one extra request header.
    ///
    /// Exists for conditional requests: an `If-None-Match` round trip is the only
    /// way to observe that a route revalidates instead of resending its body.
    pub async fn get_authed_with_header(
        &self,
        path: &str,
        name: header::HeaderName,
        value: &str,
    ) -> Response<Body> {
        self.send(
            Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .header(name, value)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn post_authed(&self, path: &str, body: Value) -> Response<Body> {
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    pub async fn patch_authed(&self, path: &str, body: Value) -> Response<Body> {
        self.send(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    pub async fn delete_authed(&self, path: &str) -> Response<Body> {
        self.send(
            Request::builder()
                .method("DELETE")
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    /// POST with an explicit `Idempotency-Key` header.
    pub async fn post_authed_with_key(
        &self,
        path: &str,
        body: Value,
        key: &str,
    ) -> Response<Body> {
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .header(header::COOKIE, &self.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    /// Create a session and return its id, asserting the call succeeded.
    ///
    /// The status is captured before the body is consumed, so one request
    /// serves both the assertion and the id. Issuing a second request to
    /// recover the id would leave an orphan session behind and make any
    /// session-counting assertion silently wrong.
    pub async fn create_session(&self, agent: &str) -> String {
        let response = self
            .post_authed(
                "/api/v1/sessions",
                json!({
                    "agent": agent,
                    "workdir": self.workdir.display().to_string(),
                    "title": "Test session",
                }),
            )
            .await;

        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "session creation failed: {body}"
        );

        body["session"]["id"]
            .as_str()
            .expect("session id missing from response")
            .to_string()
    }

    /// Poll a turn until it reaches a terminal status, or panic on timeout.
    ///
    /// Turns run detached from the request that created them, so tests must
    /// wait for the supervisor rather than assuming completion.
    pub async fn await_turn(&self, turn_id: &str) -> Value {
        for _ in 0..100 {
            let response = self.get_authed(&format!("/api/v1/turns/{}", turn_id)).await;
            let body = json_body(response).await;
            let status = body["turn"]["status"].as_str().unwrap_or("");
            if matches!(
                status,
                "completed" | "failed" | "cancelled" | "interrupted"
            ) {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("turn {} did not reach a terminal state within 5s", turn_id);
    }
}

pub async fn body_string(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("failed to read body");
    String::from_utf8_lossy(&bytes).to_string()
}

pub async fn json_body(response: Response<Body>) -> Value {
    let text = body_string(response).await;
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("body was not JSON ({}): {}", e, text))
}

/// A generated runner script conforming to the runner protocol.
pub struct MockRunner {
    name: String,
    behavior: Behavior,
}

enum Behavior {
    /// Stream events, then a successful envelope.
    Success,
    /// Stream nothing, return a failure envelope.
    Failure,
    /// Emit no final envelope at all — a protocol violation.
    NoEnvelope,
    /// Sleep long enough for a stop request to arrive.
    Slow,
    /// Fail `--describe`, so discovery marks the runner broken.
    BrokenDescribe,
}

impl MockRunner {
    pub fn success(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Success }
    }

    pub fn failure(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Failure }
    }

    pub fn no_envelope(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::NoEnvelope }
    }

    pub fn slow(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Slow }
    }

    pub fn broken_describe(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::BrokenDescribe }
    }

    fn install(&self, runners_dir: &Path) {
        let dir = runners_dir.join(&self.name);
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("runner.sh");
        std::fs::write(&script_path, self.script()).unwrap();

        // Discovery only considers executable runners.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
    }

    fn script(&self) -> String {
        let describe = if matches!(self.behavior, Behavior::BrokenDescribe) {
            r#"  echo "describe is broken" >&2
  exit 1"#
                .to_string()
        } else {
            format!(
                r#"  cat <<'JSON'
{{
  "name": "{}",
  "description": "Mock runner for tests",
  "session_key": "{}",
  "flags": {{
    "model": {{"choices": ["fast", "slow"], "description": "Mock model"}},
    "effort": {{"choices": ["low", "high"], "description": "Mock reasoning effort"}}
  }},
  "commands": {{
    "noop": {{"description": "Does nothing", "args": {{}}}}
  }}
}}
JSON
  exit 0"#,
                self.name, self.name
            )
        };

        // The turn body mirrors what a real runner emits: stream lines only
        // when SYLPH_STREAM_FORMAT requests them, then the final envelope.
        let turn = match self.behavior {
            Behavior::Success | Behavior::BrokenDescribe => {
                r#"
if [ "${SYLPH_STREAM_FORMAT:-}" = "ndjson-v1" ]; then
  echo '{"stream":"sylph.runner.v1","event":{"kind":"thinking","phase":"started","id":"t1"}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"thinking","phase":"delta","id":"t1","text":"think"}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"thinking","phase":"completed","id":"t1","text":"thinking done"}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"tool_call","phase":"completed","id":"c1","name":"Read","data":{"path":"/tmp/x"}}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"delta","id":"a1","text":"mock "}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"a1","text":"mock reply"}}'
  echo '{"stream":"sylph.runner.v1","event":{"kind":"usage","phase":"completed","data":{"inputTokens":10,"outputTokens":3}}}'
fi
printf '%s\n' '{"ok":true,"text":"mock reply","session_id":"mock-session-1"}'
exit 0
"#
            }
            Behavior::Failure => {
                r#"
printf '%s\n' '{"ok":false,"error":"mock runner failed on purpose"}'
exit 1
"#
            }
            Behavior::NoEnvelope => {
                r#"
echo "this runner forgot its envelope" >&2
exit 3
"#
            }
            Behavior::Slow => {
                r#"
if [ "${SYLPH_STREAM_FORMAT:-}" = "ndjson-v1" ]; then
  echo '{"stream":"sylph.runner.v1","event":{"kind":"status","phase":"started","text":"working"}}'
fi
sleep 30
printf '%s\n' '{"ok":true,"text":"should not be reached","session_id":"slow-1"}'
exit 0
"#
            }
        };

        format!(
            r#"#!/usr/bin/env bash
set -u

if [ "${{1:-}}" = "--describe" ]; then
{describe}
fi

# Consume stdin so the writer never blocks on a full pipe.
cat >/dev/null
{turn}"#
        )
    }
}

// ------------------------------------------------------------- mock TraceDB

/// A stub `trace-db` binary for exercising archive-backed routes.
///
/// Payload shapes mirror the current `trace-db api` exactly, including its
/// per-operation spelling: `search` answers camelCase, `show` answers
/// snake_case. A mock that normalized the two would let a real spelling
/// mismatch pass the suite.
/// An archive holding no sessions for the requested filter.
const EMPTY_LIST: &str = r#"{"sessions": []}"#;

/// Wrap a payload as a POSIX single-quoted shell word, safe for any content.
///
/// The mock emits JSON from a `/bin/sh` script, so an apostrophe in test data —
/// `"The directory's own work"` — would otherwise close the quote early and feed
/// the client a truncated line, which surfaces as a JSON decoding error that looks
/// like a protocol bug rather than a fixture bug. The standard escape is to end
/// the quote, emit a quoted apostrophe, and reopen: `'\''`.
fn shell_single_quote(payload: &str) -> String {
    format!("'{}'", payload.replace('\'', r"'\''"))
}

pub struct MockTraceDb {
    /// Emitted verbatim as the `result` of every `search` request.
    search_result: String,
    /// Emitted verbatim as the `result` of every `show` request.
    show_result: String,
    /// Emitted verbatim as the `result` of every `list` request.
    ///
    /// Separate from `search_result` because the two shapes differ: `search`
    /// answers a bare array of hits, `list` answers
    /// `{"sessions": [...], "nextCursor": ...}`. Sharing one field would let the
    /// archive-browse endpoints pass their tests against a payload the real
    /// `trace-db` never emits.
    list_result: String,
    /// Per-id `show` payloads, for tests where lineage or locality differs by
    /// session. Keyed by TraceDB id; ids not listed fall back to `show_result`.
    sessions: Vec<(String, String)>,
    /// When true, answer `{"ok": false, ...}` instead of a result.
    fails: bool,
}

impl MockTraceDb {
    /// Returns `hits` (a JSON array literal) for any search query.
    pub fn with_hits(hits: Value) -> Self {
        Self {
            search_result: hits.to_string(),
            show_result: "null".to_string(),
            list_result: EMPTY_LIST.to_string(),
            sessions: Vec::new(),
            fails: false,
        }
    }

    /// Returns a `list` page for any archive listing request.
    ///
    /// `sessions` is a JSON array of `SessionSummary` rows in `list`'s camelCase
    /// spelling. The same instance answers `show` with `show_session`, which the
    /// listing path calls to derive a label for a row carrying no title.
    pub fn with_listing(sessions: Value, next_cursor: Option<&str>) -> Self {
        let mut page = json!({"sessions": sessions});
        if let Some(cursor) = next_cursor {
            page["nextCursor"] = json!(cursor);
        }
        Self {
            search_result: "[]".to_string(),
            show_result: "null".to_string(),
            list_result: page.to_string(),
            sessions: Vec::new(),
            fails: false,
        }
    }

    /// Answer `show` with this session, keeping whatever else is configured.
    ///
    /// Chained onto [`Self::with_listing`] when a test needs both a listing and
    /// the transcript behind one of its rows.
    pub fn and_session(mut self, metadata: Value, events: Value) -> Self {
        let mut session = json!({"id": "mock:archived", "agent": "mock"});
        for (key, value) in metadata.as_object().expect("metadata must be an object") {
            session[key] = value.clone();
        }
        self.show_result =
            json!({"session": session, "mode": "full", "events": events}).to_string();
        self
    }

    /// Register the `show` payload for one specific id.
    ///
    /// Needed wherever behaviour depends on *which* session is asked about — the
    /// lineage rollup reads each row's `parent_session_id`/`forked_from`, then asks
    /// whether that parent lives in the directory being listed. `metadata` is the
    /// session header; `id` is filled in from the key.
    pub fn and_session_for(mut self, id: &str, metadata: Value) -> Self {
        let mut session = json!({"id": id, "agent": "claude"});
        for (key, value) in metadata.as_object().expect("metadata must be an object") {
            session[key] = value.clone();
        }
        self.sessions.push((
            id.to_string(),
            json!({"session": session, "mode": "metadata", "events": []}).to_string(),
        ));
        self
    }

    /// Returns a `show` payload built from session metadata and events.
    ///
    /// `metadata` supplies the fields the import path reads; `id` and `agent`
    /// are filled in because the real archive always reports them.
    pub fn with_session(metadata: Value, events: Value) -> Self {
        Self {
            search_result: "[]".to_string(),
            show_result: "null".to_string(),
            list_result: EMPTY_LIST.to_string(),
            sessions: Vec::new(),
            fails: false,
        }
        .and_session(metadata, events)
    }

    /// Answers `show` with `null`, which is how the archive reports an id it
    /// does not have.
    pub fn without_session() -> Self {
        Self {
            search_result: "[]".to_string(),
            show_result: "null".to_string(),
            list_result: EMPTY_LIST.to_string(),
            sessions: Vec::new(),
            fails: false,
        }
    }

    /// Answers every request with a protocol-level error.
    pub fn failing() -> Self {
        Self {
            search_result: "[]".to_string(),
            show_result: "null".to_string(),
            list_result: EMPTY_LIST.to_string(),
            sessions: Vec::new(),
            fails: true,
        }
    }

    fn install(&self, root: &Path) -> PathBuf {
        let path = root.join("mock-trace-db");
        std::fs::write(&path, self.script()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }

        path
    }

    /// A line-protocol server: read a request line, write a response line.
    /// Long-lived, because the client reuses the process across calls, and
    /// branching on the request's `op` because one process serves every method.
    fn script(&self) -> String {
        if self.fails {
            return r#"#!/bin/sh
set -eu

while IFS= read -r _line; do
  printf '%s\n' '{"ok": false, "result": {"error": "index is locked"}}'
done
"#
            .to_string();
        }

        let search = shell_single_quote(&format!(
            r#"{{"ok": true, "result": {}}}"#,
            self.search_result
        ));
        let show =
            shell_single_quote(&format!(r#"{{"ok": true, "result": {}}}"#, self.show_result));
        let list =
            shell_single_quote(&format!(r#"{{"ok": true, "result": {}}}"#, self.list_result));
        let stats = shell_single_quote(
            r#"{"ok": true, "result": {"totalSessions": 2, "totalEvents": 9}}"#,
        );

        // Per-id `show` branches, matched before the catch-all. The real archive
        // answers each id differently, and the lineage rollup asks about specific
        // ids -- a mock that returned one canned session for every `show` would
        // report every parent as living in no directory at all.
        let mut per_id = String::new();
        for (id, payload) in &self.sessions {
            let body = shell_single_quote(&format!(r#"{{"ok": true, "result": {payload}}}"#));
            per_id.push_str(&format!(
                "    *'\"show\"'*'\"{id}\"'*) printf '%s\\n' {body} ;;\n"
            ));
        }

        format!(
            r#"#!/bin/sh
set -eu

while IFS= read -r line; do
  case "$line" in
{per_id}    *'"show"'*) printf '%s\n' {show} ;;
    *'"list"'*) printf '%s\n' {list} ;;
    *'"stats"'*) printf '%s\n' {stats} ;;
    *) printf '%s\n' {search} ;;
  esac
done
"#
        )
    }
}
