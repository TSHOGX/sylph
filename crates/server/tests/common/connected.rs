//! Actual loopback sockets with a controllable receipt-loss window.
use super::common::{self, FixtureAgents};
use anyhow::Result;
use async_trait::async_trait;
use axum::{Router, body::Body, http::Request};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use sylph_envagent::{adapters::AdapterSource, transport::FramedConnection};
use sylph_protocol::{
    trace::{ObservationRef, TransportKind},
    *,
};
use sylph_server::{
    AppState,
    config::{Config, Resolved},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Notify, watch},
};
use tower::ServiceExt;

#[derive(Clone, Default)]
pub struct CaptureAgents {
    pub fixture: FixtureAgents,
    captures: Arc<Mutex<HashMap<String, Arc<dyn RawCaptureSink>>>>,
    pub journal: PathBuf,
    pub native_files: Arc<Mutex<HashMap<String, PathBuf>>>,
}
impl AdapterSource for CaptureAgents {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        (name == "codex").then(|| Box::new(self.clone()) as Box<dyn Adapter>)
    }
    fn known_agents(&self) -> Vec<String> {
        vec!["codex".into()]
    }
}
#[async_trait]
impl Adapter for CaptureAgents {
    async fn probe(&self) -> Result<AgentInfo> {
        self.fixture.probe().await
    }
    async fn run(
        &self,
        spec: RunSpec,
        capture: Arc<dyn RawCaptureSink>,
    ) -> Result<Box<dyn Session>> {
        let session = self.fixture.run(spec, capture.clone()).await?;
        let native = session.native_id().unwrap();
        self.captures
            .lock()
            .unwrap()
            .insert(native.to_string(), capture);
        Ok(Box::new(JournaledSession {
            session,
            journal: self.journal.clone(),
        }))
    }
    async fn native_sources(&self, id: &NativeId) -> Result<Vec<NativeSource>> {
        Ok(self.native_files.lock().unwrap().get(id.as_str()).map(|path| NativeSource {
            locator: path.to_string_lossy().into_owned(), namespace: "codex.native".into(),
            format: NativeSourceFormat::Jsonl, companion_locator: None,
            owner: Some(NativeOwner::Session { native_id: id.clone() }),
            identity_record_ordinal: Some(0),
        }).into_iter().collect())
    }
    async fn enumerate(&self, _: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(vec![])
    }
}
impl CaptureAgents {
    pub fn new(journal: PathBuf) -> Self {
        Self {
            journal,
            ..Default::default()
        }
    }
    pub async fn emit(&self, session: &SessionId, bytes: Vec<u8>) -> ObservationRef {
        let sink = self.captures.lock().unwrap()[session.native_id.as_str()].clone();
        sink.capture(RawCapture {
            stream: format!("connected-history-{}", session.native_id),
            transport: TransportKind::NativeJsonl,
            bytes,
            captured_at_ms: 5,
        })
        .await
        .unwrap()
    }
}
struct JournaledSession {
    session: Box<dyn Session>,
    journal: PathBuf,
}
impl JournaledSession {
    fn record(&self, input: &TurnInput) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.journal)
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({"native":self.native_id(), "text":input.text})
        )
        .unwrap();
        file.sync_all().unwrap();
    }
}
#[async_trait]
impl Session for JournaledSession {
    fn native_id(&self) -> Option<NativeId> {
        self.session.native_id()
    }
    async fn identity_evidence(&self) -> Vec<ObservationRef> {
        self.session.identity_evidence().await
    }
    async fn capabilities(&self) -> Result<SessionCapabilities> {
        self.session.capabilities().await
    }
    async fn send(&self, input: TurnInput) -> Result<()> {
        self.record(&input);
        self.session.send(input).await
    }
    async fn send_initial(&self, input: TurnInput) -> Result<NativeId> {
        self.record(&input);
        self.session.send_initial(input).await
    }
    async fn last_sent_turn_id(&self) -> Option<String> {
        self.session.last_sent_turn_id().await
    }
    async fn interrupt(&self) -> Result<()> {
        self.session.interrupt().await
    }
    async fn steer(&self, text: &str, expected: Option<&str>) -> Result<()> {
        self.session.steer(text, expected).await
    }
    async fn queue(&self, text: &str) -> Result<()> {
        self.session.queue(text).await
    }
    async fn set_config(&self, id: &str, value: Value) -> Result<Vec<ConfigOption>> {
        self.session.set_config(id, value).await
    }
    async fn resolve(&self, resolution: Resolution) -> Result<()> {
        self.session.resolve(resolution).await
    }
    fn messages(&mut self) -> Option<ControlStream> {
        self.session.messages()
    }
    async fn close(&self) -> Result<()> {
        self.session.close().await
    }
}

pub struct WireControl {
    hold_next: AtomicBool,
    held: Mutex<Option<ObservationRef>>,
    held_ready: Notify,
    cut: watch::Sender<u64>,
    pub connections: AtomicU64,
    pub upload_frames: AtomicU64,
}
impl WireControl {
    fn new() -> Self {
        Self {
            hold_next: AtomicBool::new(false),
            held: Mutex::new(None),
            held_ready: Notify::new(),
            cut: watch::channel(0).0,
            connections: AtomicU64::new(0),
            upload_frames: AtomicU64::new(0),
        }
    }
    pub fn hold_next_receipt(&self) {
        *self.held.lock().unwrap() = None;
        self.hold_next.store(true, Ordering::SeqCst);
    }
    pub async fn held_receipt(&self) -> ObservationRef {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let ready = self.held_ready.notified();
                if let Some(held) = self.held.lock().unwrap().clone() {
                    return held;
                }
                ready.await;
            }
        })
        .await
        .expect("the server must commit and emit the held receipt")
    }
    pub fn disconnect(&self) {
        self.cut.send_modify(|epoch| *epoch += 1);
    }
}

pub struct ConnectedBackend {
    pub state: AppState,
    pub source: CaptureAgents,
    pub wire: Arc<WireControl>,
    pub environment: EnvironmentId,
    router: Router,
    address: String,
    hello: Hello,
    credential: String,
    config: sylph_envagent::EnvAgentConfig,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    child: Option<std::process::Child>,
    // Database owners and sockets must drop before their temporary files.
    pub root: tempfile::TempDir,
}
impl Drop for ConnectedBackend {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl ConnectedBackend {
    pub async fn start(child_process: bool) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("error")
            .try_init();
        let root = tempfile::tempdir().unwrap();
        let env = [
            ("HOME".into(), root.path().display().to_string()),
            ("SYLPH_HOME".into(), root.path().display().to_string()),
            (
                "SYLPH_BROWSE_ROOTS".into(),
                root.path().display().to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let resolved = Resolved::from_parts(&env, &Default::default(), false);
        sylph_server::config::seed(&resolved).unwrap();
        let mut config = Config::from_resolved(&resolved);
        config.cookie_secret = "connected-test-cookie".into();
        let state = AppState::build(config).await.unwrap();
        let offer = state.env_bridge.create_pairing().await.unwrap();
        let grant = state
            .env_bridge
            .claim_pairing(
                &offer.code,
                "Connected acceptance",
                "development",
                vec![common::mock::info()],
            )
            .await
            .unwrap();
        let environment = grant.environment_id.clone();
        let mut hello = Hello::current(
            grant.installation_id,
            ProducerId::parse("connected-acceptance").unwrap(),
            Some(environment.clone()),
            0,
        );
        hello.agents = vec![AgentInventory::available(common::mock::info())];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_address = listener.local_addr().unwrap();
        let bridge = state.env_bridge.clone();
        let server = tokio::spawn(async move {
            bridge.serve(listener).await.unwrap();
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let wire = Arc::new(WireControl::new());
        let control = wire.clone();
        let proxy = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (downstream, _) = connection.unwrap();
                        downstream.set_nodelay(true).unwrap();
                        let upstream = TcpStream::connect(server_address).await.unwrap();
                        upstream.set_nodelay(true).unwrap();
                        let control = control.clone();
                        connections.spawn(async move { relay(downstream, upstream, control).await; });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        let source = CaptureAgents::new(root.path().join("vendor-sends.jsonl"));
        let config = sylph_envagent::EnvAgentConfig {
            environment_id: environment.to_string(),
            producer_id: "connected-acceptance".into(),
            spool: sylph_envagent::SpoolConfig::new(
                root.path().join("spool.db"),
                128 * 1024 * 1024,
            ),
            path_policy: sylph_envagent::PathPolicy::from_roots(vec![root.path().to_path_buf()])
                .unwrap(),
        };
        let mut backend = Self {
            router: sylph_server::router(state.clone()),
            state,
            source,
            wire,
            environment,
            root,
            address,
            hello,
            credential: grant.credential,
            config,
            tasks: vec![server, proxy],
            child: None,
        };
        if child_process {
            backend.start_child();
        } else {
            let address = backend.address.clone();
            let hello = backend.hello.clone();
            let credential = backend.credential.clone();
            let config = backend.config.clone();
            let source = backend.source.clone();
            backend.tasks.push(tokio::spawn(async move {
                sylph_envagent::client::run_runtime_reconnecting_with_adapters(
                    address,
                    hello,
                    credential,
                    config,
                    Arc::new(source),
                )
                .await
                .unwrap();
            }));
        }
        backend.wait_connected_after(0).await;
        backend
    }
    pub fn start_child(&mut self) {
        assert!(self.child.is_none());
        let settings = serde_json::json!({"address":self.address,"hello":self.hello,"credential":self.credential,"root":self.root.path()});
        self.child = Some(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "connected_runtime_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("SYLPH_CONNECTED_RUNTIME", settings.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }
    pub fn kill_child(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    pub async fn epoch(&self) -> u64 {
        self.state
            .store
            .environment_row(&self.environment)
            .await
            .unwrap()
            .unwrap()
            .connection_epoch
    }
    pub async fn wait_connected_after(&self, previous: u64) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self
                    .state
                    .env_bridge
                    .environment_connected(&self.environment)
                    .await
                    && self.epoch().await > previous
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("envagent reconnect must finish");
    }
    pub async fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost")
            .header("cookie", "sylph_session=connected-test-cookie");
        let body = body.map_or(Body::empty(), |body| {
            request = std::mem::take(&mut request).header("content-type", "application/json");
            Body::from(body.to_string())
        });
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    pub async fn create_session(&self, text: &str) -> String {
        let (status, draft) = self.request("POST", "/api/v1/sessions", Some(serde_json::json!({"environment":self.environment,"agent":"codex","workdir":self.root.path(),"injectIdentity":false}))).await;
        assert_eq!(status, 201, "{draft}");
        let (status, created) = self
            .request(
                "POST",
                &format!(
                    "/api/v1/sessions/draft/{}/commands",
                    draft["draftId"].as_str().unwrap()
                ),
                Some(serde_json::json!({"text":text,"idempotencyKey":text})),
            )
            .await;
        assert_eq!(status, 202, "{created}");
        created["session"]["id"]
            .as_str()
            .expect("fixture native identity")
            .into()
    }
    pub async fn send(&self, session: &str, text: &str) -> Value {
        let (status, result) = self.request("POST", "/api/v1/trace/commands", Some(serde_json::json!({"target":{"kind":"session","agent":sylph_protocol::trace::AgentRef::Session(SessionId::decode(session).unwrap()).encode(),"sessionId":session},"action":"send","idempotencyKey":text,"text":text,"attachments":[]}))).await;
        assert_eq!(status, 202, "{result}");
        result
    }
    /// Active and pending view leases across every subject.
    ///
    /// Read from the durable evidence store rather than a service accessor, so a
    /// lease the server believes it released but never deleted still counts.
    pub async fn active_leases(&self) -> i64 {
        use sqlx::Connection;
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.state.config.data_dir.join("evidence.db"))
            .read_only(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query_scalar("SELECT COUNT(*) FROM trace_view_leases WHERE state IN ('pending','active') AND expires_at_ms>?")
            .bind(sylph_server::models::now_ms())
            .fetch_one(&mut connection)
            .await
            .unwrap()
    }
    pub async fn spool_count(&self) -> i64 {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&self.config.spool.path)
            .read_only(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        use sqlx::Connection;
        sqlx::query_scalar("SELECT count(*) FROM capture_queue")
            .fetch_one(&mut connection)
            .await
            .unwrap()
    }
    pub async fn pending_observations(&self) -> Vec<ObservationRef> {
        use sqlx::Connection;
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&self.config.spool.path)
            .read_only(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        let captures: Vec<Vec<u8>> = sqlx::query_scalar("SELECT capture_json FROM capture_queue ORDER BY producer_id,producer_epoch,producer_seq")
            .fetch_all(&mut connection).await.unwrap();
        captures
            .into_iter()
            .map(|bytes| {
                serde_json::from_slice::<trace::Capture>(&bytes)
                    .unwrap()
                    .observation_ref
            })
            .collect()
    }
    pub async fn drained(&self) {
        tokio::time::timeout(Duration::from_secs(45), async {
            while self.spool_count().await != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("every queued capture must receive a terminal receipt");
    }
    pub fn sends(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.source.journal)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

async fn relay(downstream: TcpStream, upstream: TcpStream, control: Arc<WireControl>) {
    control.connections.fetch_add(1, Ordering::SeqCst);
    let mut cut = control.cut.subscribe();
    let (agent_read, agent_write) = downstream.into_split();
    let (server_read, server_write) = upstream.into_split();
    let upload = async {
        let mut reader = FramedConnection::new(agent_read);
        let mut writer = FramedConnection::new(server_write);
        loop {
            let bytes = reader.read_bytes().await?;
            if matches!(
                sylph_protocol::decode_message(&bytes),
                Ok(DecodedMessage::Known(WireMessage::CaptureUpload { .. }))
            ) {
                control.upload_frames.fetch_add(1, Ordering::SeqCst);
            }
            writer.write_bytes(&bytes).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), sylph_envagent::FrameError>(())
    };
    let download = async {
        let mut reader = FramedConnection::new(server_read);
        let mut writer = FramedConnection::new(agent_write);
        loop {
            let bytes = reader.read_bytes().await?;
            if let Ok(DecodedMessage::Known(WireMessage::CaptureReply {
                outcome: CaptureReply::Admitted { receipts, .. },
                ..
            })) = sylph_protocol::decode_message(&bytes)
            {
                if control.hold_next.swap(false, Ordering::SeqCst) {
                    *control.held.lock().unwrap() = Some(receipts[0].observation.clone());
                    control.held_ready.notify_waiters();
                    continue;
                }
            }
            writer.write_bytes(&bytes).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), sylph_envagent::FrameError>(())
    };
    tokio::select! { _ = upload => {}, _ = download => {}, _ = cut.changed() => {} }
}
