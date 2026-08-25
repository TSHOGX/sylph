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

        // The harness root *is* the Sylph home, so every path resolves the way
        // production resolves it — from one root — rather than being assembled
        // field by field here. A hand-built `Config` was how the harness came to
        // describe a layout the resolver no longer produced.
        let agents_dir = root.join("agents");
        let workdir = root.join("workspace");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::create_dir_all(&workdir).unwrap();

        if let Some(text) = soul {
            std::fs::write(root.join("SOUL.md"), text).unwrap();
        }

        for runner in runners {
            runner.install(&agents_dir);
        }

        // Resolved from an explicit environment rather than the process's own, so
        // tests run concurrently: `std::env::set_var` is process-global and unsafe
        // in a threaded test binary.
        let env: sylph_web_backend::config::resolve::Env = [
            ("HOME", root.display().to_string()),
            ("SYLPH_HOME", root.display().to_string()),
            // The harness root, not `$HOME`: browsing tests must resolve inside the
            // throwaway tree so they assert the real containment rule without
            // depending on the developer's home directory.
            ("SYLPH_BROWSE_ROOTS", root.display().to_string()),
            (
                // Absent unless a test opts in, so TraceDB-dependent paths degrade
                // rather than making every test need an external binary.
                "SYLPH_TRACEDB_BIN",
                match &tracedb {
                    Some(stub) => stub.install(root).display().to_string(),
                    None => root.join("nonexistent-trace-db").display().to_string(),
                },
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

        let resolved = sylph_web_backend::config::Resolved::from_parts(
            &env,
            &sylph_web_backend::config::resolve::FileConfig::default(),
            false,
        );
        // Seeded like a first run, so the harness exercises the same directory
        // creation production does — and so `data_dir` exists before the store
        // opens. `SOUL.md` written above survives, since seeding never overwrites.
        sylph_web_backend::config::seed(&resolved).unwrap();
        // A first run also *creates* a `SOUL.md`, which for the harness would
        // silently turn identity injection on for every test. Only the tests that
        // ask for one should have one, so the seeded copy is removed again — after
        // seeding rather than instead of it, so the seeding path stays exercised.
        if soul.is_none() {
            std::fs::remove_file(root.join("SOUL.md")).ok();
        }

        let mut config = Config::from_resolved(&resolved);
        config.port = 0;
        // Only the harness tree, so a diagnostics assertion cannot pick up a path
        // that depends on where this checkout happens to live.
        config.bundled_agents_dir = agents_dir.clone();
        config.bundled_agents_candidates = vec![agents_dir];
        config.cookie_secret = "test-secret-token".to_string();

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

    /// Wait until a settled turn's events have moved into history.
    ///
    /// Consolidation is scheduled on settle and runs detached, so a test cannot
    /// simply read afterwards. This polls the observable end state — journal empty,
    /// history populated — and sweeps once as a fallback, which covers the case
    /// where the scheduled attempt deferred.
    pub async fn await_consolidation(&self, turn_id: &str) {
        for attempt in 0..50 {
            let journalled = self
                .state
                .store
                .turn_events_after(turn_id, -1)
                .await
                .unwrap_or_default();
            let blocks = self
                .state
                .history
                .turn_blocks(turn_id)
                .await
                .unwrap_or_default();
            if journalled.is_empty() && !blocks.is_empty() {
                return;
            }
            // Halfway through, do the work ourselves rather than waiting out the
            // whole budget on an attempt that already failed.
            if attempt == 25 {
                self.state.consolidate_now().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("turn {} was never consolidated", turn_id);
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

    /// Where Quick Start sessions run, as configuration resolved it.
    ///
    /// Exposed so a test asserts against the *configured* directory rather than a
    /// literal path. A test that hardcoded one could only ever confirm that the
    /// endpoint agreed with the test, which is how the endpoint kept reporting a
    /// directory belonging to another project.
    pub fn quick_start_dir(&self) -> &Path {
        &self.config.quick_start_dir
    }

    /// Where this backend's adapters live.
    ///
    /// Exposed for the tests that remove one to simulate an uninstalled agent.
    /// Spelling the directory out at the call site puts the layout in two places,
    /// and the copy in the test stops removing anything the moment the layout
    /// moves — leaving a test that still asserts the detached path but no longer
    /// produces it.
    pub fn agents_dir(&self) -> &Path {
        &self.config.agents_dir
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

    /// An authenticated request with an arbitrary method.
    ///
    /// Exists for the read-only assertions: proving a route rejects `PUT` needs a
    /// method the typed helpers do not offer, and adding a helper per verb would be
    /// four functions to prove one property.
    pub async fn request_authed(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Response<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1")
            .header(header::COOKIE, &self.cookie);
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        self.send(builder.body(body).unwrap()).await
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

/// A generated adapter conforming to `sylph.agent.v3`.
///
/// Python rather than bash: an adapter holds a duplex read loop and parses JSON,
/// which bash does badly enough that a fixture written in it would be testing the
/// fixture. Named `MockRunner` still, because every spec refers to it that way and
/// renaming it would obscure a protocol change behind a diff of test churn.
pub struct MockRunner {
    name: String,
    behavior: Behavior,
}

enum Behavior {
    /// Stream a full turn, then report it completed.
    Success,
    /// Report the turn failed.
    Failure,
    /// Complete the turn with no assistant output at all.
    ///
    /// Under v1 this was "emit no final envelope", a protocol violation the
    /// supervisor had to detect. v2 has no envelope to omit: the turn event *is*
    /// the outcome. The nearest real failure is an agent that ends a turn having
    /// produced nothing, which is what this now exercises.
    NoOutput,
    /// Dispatch a delegate, end the turn, then report the delegate's completion
    /// *after* the turn has settled — and open a turn of its own to say so.
    ///
    /// What `claude` 2.x actually does with backgrounded work, measured: after a
    /// turn's `result` it emits the delegate's completion and then an entire
    /// exchange (`init`, assistant, `result`) reporting it, with stdout still open.
    /// Every one of those events used to be dropped for arriving outside a turn.
    Backgrounded,
    /// The same shape, with the delegate declaring `is_backgrounded` on every phase.
    ///
    /// What a *cooperating* adapter sends: `claude` reports the flag only on
    /// `task_started`, so its adapter re-attaches it to each later phase. The
    /// turn-settle sweep skips such a trajectory, which is the branch
    /// [`Behavior::Backgrounded`] cannot reach — that one exercises every adapter
    /// that reports nothing, which is all of them except `claude`.
    BackgroundedFlagged,
    /// Accept the turn and never finish it, so a stop or interrupt can arrive.
    Slow,
    /// Fail the handshake, so the agent is unusable.
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
        Self { name: name.to_string(), behavior: Behavior::NoOutput }
    }

    pub fn slow(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Slow }
    }

    /// An adapter that reports a delegate finishing after its turn ended.
    pub fn backgrounded(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::Backgrounded }
    }

    pub fn backgrounded_flagged(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::BackgroundedFlagged }
    }

    pub fn broken_describe(name: &str) -> Self {
        Self { name: name.to_string(), behavior: Behavior::BrokenDescribe }
    }

    fn install(&self, agents_dir: &Path) {
        let dir = agents_dir.join(&self.name);
        std::fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("adapter.sh");
        std::fs::write(&script_path, self.script()).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
    }

    /// What this adapter does when a turn arrives.
    fn on_send(&self) -> &'static str {
        match self.behavior {
            Behavior::Success | Behavior::BrokenDescribe => {
                r#"
        ok(mid)
        # A per-turn capability update that speaks about ONE facet, which is what
        # `claude` actually does: its `system/init` arrives once per turn and carries
        # only the permission mode. This line is the fixture's whole reason for
        # existing — without it the suite cannot observe a partial update erasing the
        # facets it never mentioned, and that bug shipped once already.
        ev("capabilities", facets={"permissionMode": "default"})
        ev("thinking", "started", streamId="t1")
        ev("thinking", "delta", streamId="t1", text="think")
        ev("thinking", "completed", streamId="t1", text="thinking done")
        ev("tool_call", "completed", streamId="c1", name="Read",
           data={"path": "/tmp/x"})
        # Deltas must concatenate to the completed text, exactly as a real agent's
        # do — it builds the completed event by accumulating them. A fixture whose
        # deltas disagree makes `fold` refuse, which is correct behaviour on bad
        # input and useless as a test of the good path.
        ev("assistant", "delta", streamId="a1", text="mock ")
        ev("assistant", "delta", streamId="a1", text="reply")
        ev("assistant", "completed", streamId="a1", text="mock reply")
        ev("usage", "completed", data={"inputTokens": 10, "outputTokens": 3})
        ev("turn", "completed", text="mock reply")
"#
            }
            Behavior::Failure => {
                r#"
        ok(mid)
        ev("turn", "completed", text="mock runner failed on purpose", isError=True)
"#
            }
            Behavior::NoOutput => {
                r#"
        ok(mid)
        sys.stderr.write("this adapter produced nothing\n")
        sys.stderr.flush()
        ev("turn", "completed", text="", isError=True)
"#
            }
            Behavior::Backgrounded => {
                r#"
        ok(mid)
        # A host that dispatches and returns. Its delegate outlives the turn.
        ev("tool_call", "completed", streamId="bg-host", name="Workflow",
           text='{"script": "background(worker)"}')
        ev("subagent", "started", streamId="bg-task", span="bg-host",
           name="Background: worker")
        ev("tool_result", "completed", streamId="bg-host", name="Workflow",
           text="dispatched")
        ev("assistant", "completed", streamId="bg-a1", text="dispatched")
        ev("turn", "completed", text="dispatched")

        # Everything below arrives with the turn already settled. Before agent
        # turns existed these were dropped, so the completion never reached a
        # client and the delegate stayed abandoned forever.
        time.sleep(0.4)
        ev("subagent", "completed", streamId="bg-task", span="bg-host",
           text="worker finished")
        ev("turn", "started")
        ev("assistant", "completed", streamId="bg-a2",
           text="the background worker finished")
        ev("turn", "completed", text="the background worker finished")
"#
            }
            Behavior::BackgroundedFlagged => {
                r#"
        ok(mid)
        ev("tool_call", "completed", streamId="bg-host", name="Workflow",
           text='{"script": "background(worker)"}')
        # The flag rides on `started` only, which is where the vendor reports it —
        # `claude` sends `is_backgrounded` on `task_started` and on no later
        # notification. The sweep therefore has to find it from whichever phase
        # carried it, not from the phase it happens to be looking at.
        ev("subagent", "started", streamId="bg-task", span="bg-host",
           name="Background: worker", data={"is_backgrounded": True})
        ev("tool_result", "completed", streamId="bg-host", name="Workflow",
           text="dispatched")
        ev("assistant", "completed", streamId="bg-a1", text="dispatched")
        ev("turn", "completed", text="dispatched")

        # The real outcome, on a turn of the agent's own, with the flag *absent* —
        # exactly as the vendor sends a close.
        time.sleep(0.4)
        ev("subagent", "completed", streamId="bg-task", span="bg-host",
           text="worker finished")
        ev("turn", "started")
        ev("assistant", "completed", streamId="bg-a2",
           text="the background worker finished")
        ev("turn", "completed", text="the background worker finished")
"#
            }
            Behavior::Slow => {
                r#"
        ok(mid)
        ev("status", "started", text="working")
        # Never completes: the turn stays in flight until interrupted or the
        # link is torn down, which is what a stop test needs.
"#
            }
        }
    }

    fn script(&self) -> String {
        // A broken agent fails at `hello`, which is where discovery learns it is
        // unusable — the v2 equivalent of a failing `--describe`.
        let hello = if matches!(self.behavior, Behavior::BrokenDescribe) {
            r#"
        sys.stderr.write("handshake is broken\n")
        sys.exit(1)
"#
        } else {
            r#"
        ok(mid, adapter="mock/1.0.0",
           agent={"name": NAME, "version": "0.0.0"},
           supports=["interrupt", "steer", "approval", "slash", "set_config"],
           flags={"model": {"choices": ["fast", "slow"], "description": "Mock model"},
                  "effort": {"choices": ["low", "high"], "description": "Mock effort"}})
"#
        };

        let on_send = self.on_send();

        format!(
            r#"#!/usr/bin/env python3
import json, sys, time

NAME = {name:?}


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def ok(mid, **fields):
    emit({{"t": "ok", "id": mid, **fields}})


def ev(kind, phase=None, **fields):
    body = {{"t": "event", "kind": kind}}
    if phase:
        body["phase"] = phase
    body.update({{k: v for k, v in fields.items() if v is not None}})
    emit(body)


for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    msg = json.loads(raw)
    t, mid = msg.get("t"), msg.get("id")

    if t == "hello":{hello}
    elif t == "open":
        # Echo the id we were asked to resume, exactly as a real adapter must: `open`
        # has to answer with one, and the server rejects a null as an unresumable
        # session. The claude adapter got this wrong on the resume branch, which made
        # every session unlinkable once its first link died.
        ok(mid, sessionId=msg.get("sessionId") or "mock-session-1",
           commands=[{{"name": "noop", "description": "Does nothing"}},
                     {{"name": "model", "description": "Pick a model",
                       "kind": "control", "configId": "model"}}],
           config=[{{"type": "select", "id": "model", "name": "Model",
                     "category": "model", "currentValue": "fast",
                     "options": [{{"value": "fast", "name": "Fast"}},
                                 {{"value": "slow", "name": "Slow"}}]}}],
           subagents=[{{"name": "Explore", "description": "Search"}}],
           permissionMode="default")
    elif t == "set_config":
        if msg.get("configId") != "model":
            emit({{"t": "err", "id": mid, "code": "UNSUPPORTED_FIELD",
                   "message": "no such option"}})
            continue
        ok(mid, facets={{"config": [
            {{"type": "select", "id": "model", "name": "Model", "category": "model",
              "currentValue": msg.get("value"),
              "options": [{{"value": "fast", "name": "Fast"}},
                          {{"value": "slow", "name": "Slow"}}]}}]}})
    elif t == "send":{on_send}
    elif t == "interrupt":
        ok(mid)
        ev("turn", "completed", text="interrupted",
           data={{"stopReason": "interrupted"}})
    elif t == "close":
        ok(mid)
        break
    else:
        ok(mid)
"#,
            name = self.name,
            hello = hello,
            on_send = on_send,
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
