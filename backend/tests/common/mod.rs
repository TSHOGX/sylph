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

pub mod mock;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use mock::{MockAgents, MockRunner};

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
    quick_start_dir: PathBuf,
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
    /// The agents this backend can run.
    ///
    /// Exposed so a test can make one **absent** ([`MockAgents::uninstall`]), which is what
    /// `remove_dir_all(agents_dir().join(name))` used to express. There is no adapter
    /// directory any more — an adapter is linked in — so availability is a fact the source
    /// holds rather than one the filesystem does.
    pub agents: MockAgents,
}

impl TestBackend {
    /// Start a backend with the default mock runner (`mock`), which streams a
    /// short thinking/assistant sequence and exits successfully.
    pub async fn start() -> Self {
        Self::start_with_runners(&[MockRunner::success("mock")]).await
    }

    /// Start a backend whose runners directory contains exactly `runners`.
    pub async fn start_with_runners(runners: &[MockRunner]) -> Self {
        Self::start_with(runners, None, &[]).await
    }

    /// Start a backend whose resolved `SOUL.md` holds `text`.
    ///
    /// The file must exist before [`Config`] is built, because resolution
    /// requires a readable file — the same order production runs in, where
    /// `SOUL.md` predates the process.
    pub async fn start_with_soul(text: &str) -> Self {
        Self::start_with(&[MockRunner::success("mock")], Some(text), &[]).await
    }

    async fn start_with(
        runners: &[MockRunner],
        soul: Option<&str>,
        extra_roots: &[&Path],
    ) -> Self {
        let temp = TempDir::new().expect("failed to create temp dir");
        let root = temp.path();

        // The harness root *is* the Sylph home, so every path resolves the way
        // production resolves it — from one root — rather than being assembled
        // field by field here. A hand-built `Config` was how the harness came to
        // describe a layout the resolver no longer produced.
        let workdir = root.join("workspace");
        std::fs::create_dir_all(&workdir).unwrap();

        if let Some(text) = soul {
            std::fs::write(root.join("SOUL.md"), text).unwrap();
        }

        let agents = MockAgents::new(runners);

        // Resolved from an explicit environment rather than the process's own, so
        // tests run concurrently: `std::env::set_var` is process-global and unsafe
        // in a threaded test binary.
        let env: sylph_web_backend::config::resolve::Env = [
            ("HOME", root.display().to_string()),
            ("SYLPH_HOME", root.display().to_string()),
            // The harness root, not `$HOME`: browsing tests must resolve inside the
            // throwaway tree so they assert the real containment rule without
            // depending on the developer's home directory. Colon-separated, as in
            // production, when a test permits a directory outside its own tree.
            (
                "SYLPH_BROWSE_ROOTS",
                std::iter::once(root.display().to_string())
                    .chain(extra_roots.iter().map(|path| path.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(":"),
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
        config.cookie_secret = "test-secret-token".to_string();

        // Injected rather than discovered, which is what lets tests run concurrently while
        // several of them offer an agent of the same name with different behaviour.
        let state = AppState::build_with_adapters(config.clone(), Arc::new(agents.clone()))
            .await
            .expect("failed to build state");
        let cookie = format!("sylph_session={}", state.config.cookie_secret);

        // Resolved once, in the spelling the server records and reports. Every
        // assertion comparing against these is then comparing against the
        // configured value as the server resolved it, rather than against a
        // spelling only the harness ever uses.
        let quick_start_dir = std::fs::canonicalize(&config.quick_start_dir)
            .unwrap_or_else(|_| config.quick_start_dir.clone());
        let workdir = std::fs::canonicalize(&workdir).unwrap_or(workdir);

        // Every stored session the mock enumerates reports this directory. Inside the
        // harness tree deliberately: adopting runs the *recorded* path through the same
        // containment bound `POST /sessions` uses, so a fixture reporting somewhere else
        // would exercise the refusal rather than the adopt. Canonical, because that is the
        // spelling the server journals and compares against.
        agents.set_store_workdir(Some(workdir.display().to_string()));

        Self {
            router: router(state.clone()),
            temp,
            workdir,
            quick_start_dir,
            cookie,
            config,
            state,
            agents,
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
        // The same source, so a restart keeps whatever a test uninstalled — which is what
        // a restart does to a machine whose CLI is gone.
        let state = AppState::build_with_adapters(
            self.config.clone(),
            Arc::new(self.agents.clone()),
        )
        .await
        .expect("failed to rebuild state");
        self.router = router(state.clone());
        // Replaced too, or a test would keep querying the pre-restart store and
        // miss exactly the recovery it is asserting on.
        self.state = state;
    }

    /// Restart with the Quick Start directory *outside* every browse root.
    ///
    /// The configuration this expresses is a real one — `SYLPH_HOME` and
    /// `paths.quick_start_dir` both move it, and `docs/configuration.md` says so — and it
    /// is the only one in which the Quick Start half of `session_roots()` is observable.
    /// With the default layout it sits under the harness root, so a bound built from
    /// `browse_roots` alone coincides with `session_roots()` and a test cannot tell them
    /// apart. Returns the new directory, canonicalized as the server resolves it.
    pub async fn restart_with_external_quick_start(&mut self) -> PathBuf {
        let external = self.temp.path().join("outside-the-roots").join("scratch");
        std::fs::create_dir_all(&external).unwrap();
        // `browse_roots` deliberately keeps only the workspace subtree, so the new Quick
        // Start directory is reachable *only* through the unconditional permission.
        self.config.browse_roots = vec![self.workdir.clone()];
        self.config.quick_start_dir = external.clone();
        self.restart().await;
        let canonical = std::fs::canonicalize(&external).unwrap_or(external);
        self.quick_start_dir = canonical.clone();
        canonical
    }

    /// Restart against the same journal with the session roots narrowed to
    /// `roots`.
    ///
    /// Exists for one property that cannot be observed any other way: a session
    /// recorded while its directory was permitted must stay readable after
    /// configuration stops permitting it. Reading the journal with a *different*
    /// bound in force is the whole test, and a harness that could only restart
    /// with identical configuration could not express it.
    pub async fn restart_with_session_roots(&mut self, roots: &[&Path]) {
        self.config.browse_roots = roots.iter().map(|path| path.to_path_buf()).collect();
        // Moved out of the way too. It is permitted unconditionally, so leaving it
        // pointing inside the tree would keep a root the test meant to remove.
        self.config.quick_start_dir = self.temp.path().join("no-quick-start");
        self.restart().await;
    }

    /// The session workdir, in the spelling the server records.
    ///
    /// Canonical, because session creation resolves a client-named workdir before
    /// journalling it — so this is what a created session's `workdir` reads back
    /// as, and what `/workspaces` groups by. `TempDir` hands out a path under
    /// `/var/folders` on macOS, which resolves to `/private/var/folders`; a test
    /// comparing against the unresolved spelling would be asserting against a
    /// value the server never produces.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Where Quick Start sessions run, as configuration resolved it.
    ///
    /// Exposed so a test asserts against the *configured* directory rather than a
    /// literal path. A test that hardcoded one could only ever confirm that the
    /// endpoint agreed with the test, which is how the endpoint kept reporting a
    /// directory belonging to another project. Canonical for the same reason
    /// [`Self::workdir`] is.
    pub fn quick_start_dir(&self) -> &Path {
        &self.quick_start_dir
    }

    /// A directory outside every configured session root.
    ///
    /// Exists because "outside the roots" cannot be asserted with a literal: the
    /// harness root moves per test, so a hardcoded path would either be
    /// accidentally inside it or prove nothing about the configured bound. Created
    /// on demand and cleaned up with the returned guard.
    pub fn outside_the_roots(&self) -> TempDir {
        TempDir::new().expect("failed to create a directory outside the roots")
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
        let workdir = self.workdir.display().to_string();
        self.create_session_in(agent, &workdir).await
    }

    /// [`Self::create_session`] in a named directory.
    ///
    /// For the filter tests, which need two sessions in *different* directories — and
    /// specifically in sibling directories whose paths are prefixes of one another, since
    /// that is the case a substring filter gets wrong.
    pub async fn create_session_in(&self, agent: &str, workdir: &str) -> String {
        // Read back from the server rather than hardcoded — see `environment_id`.
        let environment = self.environment_id().await;
        let response = self
            .post_authed(
                "/api/v1/sessions",
                json!({
                    // Required now: an agent's availability is decided per machine, so
                    // there is no global default that would be true anywhere.
                    "environment": environment,
                    "agent": agent,
                    "workdir": workdir,
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

    /// The environment this harness's server reports.
    ///
    /// Read back from `GET /environments` rather than hardcoded: a test that hardcodes
    /// what the code hardcodes proves only that the two agree, which `AGENTS.md` records
    /// as having twice let a test and the endpoint be wrong about the product together.
    pub async fn environment_id(&self) -> String {
        let body = json_body(self.get_authed("/api/v1/environments").await).await;
        body["environments"][0]["id"]
            .as_str()
            .expect("the server reports at least its own environment")
            .to_string()
    }

    /// The typed form of a session id, for the store-level APIs.
    pub fn session_id(encoded: &str) -> sylph_protocol::SessionId {
        sylph_protocol::SessionId::decode(encoded).expect("the server issued this id")
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
