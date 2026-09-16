//! Isolated HTTP server and real envagent runtime communicating over framed duplex I/O.
#![allow(dead_code)]
pub mod mock;
use axum::{
    body::Body,
    http::{header, Request, Response},
    Router,
};
pub use mock::FixtureAgents;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use sylph_server::{
    config::{Config, Resolved},
    router, AppState,
};
use tempfile::TempDir;
use tower::ServiceExt;

pub struct TestBackend {
    pub state: AppState,
    pub source: FixtureAgents,
    router: Router,
    temp: TempDir,
    workdir: PathBuf,
    cookie: String,
    environment: sylph_protocol::EnvironmentId,
    installation_id: String,
    token: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for TestBackend {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl TestBackend {
    pub async fn start() -> Self {
        Self::with_source(FixtureAgents::default()).await
    }
    pub async fn with_source(source: FixtureAgents) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("error")
            .try_init();
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("workspace");
        std::fs::create_dir_all(&workdir).unwrap();
        let env = [
            ("HOME".into(), temp.path().display().to_string()),
            ("SYLPH_HOME".into(), temp.path().display().to_string()),
            (
                "SYLPH_BROWSE_ROOTS".into(),
                temp.path().display().to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let resolved = Resolved::from_parts(&env, &Default::default(), false);
        sylph_server::config::seed(&resolved).unwrap();
        let mut config = Config::from_resolved(&resolved);
        config.cookie_secret = "test-cookie".into();
        let state = AppState::build(config).await.unwrap();
        let offer = state.env_bridge.create_pairing().await.unwrap();
        let grant = state
            .env_bridge
            .claim_pairing(&offer.code, "Fixture", "development", vec![mock::info()])
            .await
            .unwrap();
        let environment = grant.environment_id.clone();
        let producer = sylph_protocol::ProducerId::parse("fixture-producer").unwrap();
        let mut hello = sylph_protocol::Hello::current(
            grant.installation_id.clone(),
            producer.clone(),
            Some(environment.clone()),
            0,
        );
        hello.agents = vec![sylph_protocol::AgentInventory::available(mock::info())];
        let (server, agent) = tokio::io::duplex(8 * 1024 * 1024);
        let bridge = state.env_bridge.clone();
        let server_task = tokio::spawn(async move {
            let (r, w) = tokio::io::split(server);
            if let Err(e) = bridge.serve_io(r, w).await {
                eprintln!("fixture bridge: {e:#}");
            }
        });
        let config = sylph_envagent::EnvAgentConfig {
            environment_id: environment.to_string(),
            producer_id: producer.to_string(),
            spool: sylph_envagent::SpoolConfig::new(temp.path().join("spool.db"), 64 * 1024 * 1024),
            path_policy: sylph_envagent::PathPolicy::from_roots(vec![temp.path().to_path_buf()])
                .unwrap(),
        };
        let agents = source.clone();
        let token = grant.credential.clone();
        let agent_task = tokio::spawn(async move {
            if let Err(e) = sylph_envagent::run_runtime_stream_with_adapters(
                agent,
                hello,
                token,
                config,
                Arc::new(agents),
            )
            .await
            {
                eprintln!("fixture envagent: {e:#}");
            }
        });
        for _ in 0..200 {
            if state.env_bridge.environment_connected(&environment).await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(state.env_bridge.environment_connected(&environment).await);
        Self {
            router: router(state.clone()),
            state,
            source,
            temp,
            workdir,
            cookie: "sylph_session=test-cookie".into(),
            environment,
            installation_id: grant.installation_id,
            token: grant.credential,
            tasks: vec![server_task, agent_task],
        }
    }
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }
    pub fn browse_root(&self) -> &Path {
        self.temp.path()
    }
    pub fn quick_start_dir(&self) -> &Path {
        &self.state.config.quick_start_dir
    }
    pub fn outside_the_roots(&self) -> TempDir {
        tempfile::tempdir().unwrap()
    }
    pub fn cookie(&self) -> &str {
        &self.cookie
    }
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }
    pub fn environment_token(&self) -> &str {
        &self.token
    }
    pub async fn environment_id(&self) -> String {
        self.environment.to_string()
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> Response<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let body = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        self.router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }
    pub async fn request_authed(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Response<Body> {
        self.request(method, path, body, Some(&self.cookie)).await
    }
    pub async fn get(&self, path: &str) -> Response<Body> {
        self.request("GET", path, None, None).await
    }
    pub async fn get_authed(&self, path: &str) -> Response<Body> {
        self.request_authed("GET", path, None).await
    }
    pub async fn get_with_cookie(&self, path: &str, cookie: &str) -> Response<Body> {
        self.request("GET", path, None, Some(cookie)).await
    }
    pub async fn post(&self, path: &str, body: Value) -> Response<Body> {
        self.request("POST", path, Some(body), None).await
    }
    pub async fn post_authed(&self, path: &str, body: Value) -> Response<Body> {
        self.request_authed("POST", path, Some(body)).await
    }
    pub async fn patch_authed(&self, path: &str, body: Value) -> Response<Body> {
        self.request_authed("PATCH", path, Some(body)).await
    }
    pub async fn delete_authed(&self, path: &str) -> Response<Body> {
        self.request_authed("DELETE", path, None).await
    }
    pub async fn prepare(&self, workdir: &str) -> Value {
        let response=self.post_authed("/api/v1/sessions",json!({"environment":self.environment,"agent":"codex","workdir":workdir,"injectIdentity":false})).await;
        let status = response.status();
        let value = json_body(response).await;
        assert_eq!(status, 201, "{value}");
        value
    }
    pub async fn create_session(&self, _: &str) -> String {
        self.create_session_in("codex", self.workdir.to_str().unwrap())
            .await
    }
    pub async fn create_session_in(&self, _: &str, workdir: &str) -> String {
        let draft = self.prepare(workdir).await;
        let response = self
            .post_authed(
                &format!(
                    "/api/v1/sessions/draft/{}/commands",
                    draft["draftId"].as_str().unwrap()
                ),
                json!({"text":"Fixture input","idempotencyKey":"initial"}),
            )
            .await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, 202, "{body}");
        body["session"]["id"].as_str().unwrap().into()
    }
    pub fn session_id(value: &str) -> sylph_protocol::SessionId {
        sylph_protocol::SessionId::decode(value).unwrap()
    }
}
pub async fn json_body(response: Response<Body>) -> Value {
    serde_json::from_str(&body_string(response).await).unwrap()
}
pub async fn body_string(response: Response<Body>) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
