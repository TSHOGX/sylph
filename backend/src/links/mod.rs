//! The pool of live agent sessions, and what happens to what they say.
//!
//! A [`Link`] is one adapter process. This module owns the set of them, keyed by
//! Sylph session id, and is the only thing that establishes or tears one down.
//!
//! Its real job is being the single place where a link's output becomes durable
//! state. An event is journaled and published; a request is journaled *and*
//! published, because a request blocks the agent and therefore has to outlive
//! every client. Splitting that across the API handlers would mean each one
//! having to remember which of the two a given message was.
//!
//! Links are disposable on purpose (`docs/session-protocol.md`): they idle out,
//! they die with the server, and a turn on a detached session establishes a fresh
//! one by resuming the agent's native session id.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::adapter::link::{Link, LinkConfig, LinkEvent};
use crate::adapter::wire::SessionCapabilities;
use crate::db::LiveStore;
use crate::env::Environment;
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::models::PendingRequest;

/// How long a link may sit unused before it is torn down.
///
/// A link costs a process and an agent's loaded context, and nothing is lost by
/// dropping it: history is journaled and the next turn resumes. Thirty minutes is
/// long enough that a user stepping away and returning finds the session still
/// warm, and short enough that a day of browsing does not accumulate processes.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How often the reaper looks for idle links.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// A live link plus the bookkeeping the pool needs about it.
struct Entry {
    link: Arc<Link>,
    /// Last time a turn or command touched this link, for idle reaping.
    last_used_ms: i64,
    /// The session's agent, so a caller need not re-read it to know what is
    /// attached.
    agent: String,
}

/// What a client can be told about a session's link without holding one.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    pub attached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What this agent can actually do, so a UI never offers a control that
    /// would fail. Empty when detached: capability is a property of a live link,
    /// and reporting a remembered list would promise interrupt on a session that
    /// has no process to interrupt.
    pub supports: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<SessionCapabilities>,
}

impl LinkState {
    fn detached() -> Self {
        Self { attached: false, agent: None, supports: Vec::new(), capabilities: None }
    }
}

#[derive(Clone)]
pub struct LinkManager {
    store: LiveStore,
    hub: EventHub,
    environment: Arc<dyn Environment>,
    agents_dir: PathBuf,
    soul_path: Option<PathBuf>,
    seq: SeqAllocator,
    links: Arc<RwLock<HashMap<String, Entry>>>,
    /// Serializes establishment per session.
    ///
    /// Two turns arriving together on a detached session would otherwise both
    /// see "no link", both spawn an adapter, and both resume the same agent
    /// session — two processes writing one native store, which corrupts it.
    establishing: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl LinkManager {
    pub fn new(
        store: LiveStore,
        hub: EventHub,
        environment: Arc<dyn Environment>,
        agents_dir: PathBuf,
        soul_path: Option<PathBuf>,
        seq: SeqAllocator,
    ) -> Self {
        let manager = Self {
            store,
            hub,
            environment,
            agents_dir,
            soul_path,
            seq,
            links: Arc::new(RwLock::new(HashMap::new())),
            establishing: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.spawn_reaper();
        manager
    }

    /// Tear down links nobody has used for [`IDLE_TIMEOUT`].
    fn spawn_reaper(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(REAP_INTERVAL);
            loop {
                ticker.tick().await;
                let cutoff = now_ms() - IDLE_TIMEOUT.as_millis() as i64;

                // Collected under a read lock and closed outside it: `close`
                // awaits a grace period, and holding the map through that would
                // stall every other session's turn.
                let idle: Vec<String> = {
                    let links = manager.links.read().await;
                    links
                        .iter()
                        .filter(|(_, entry)| entry.last_used_ms < cutoff)
                        .map(|(id, _)| id.clone())
                        .collect()
                };

                for session_id in idle {
                    // A turn may have started between the scan and here, so the
                    // busy check is repeated rather than trusted from the scan.
                    if manager.store.session_status(&session_id).await.as_deref() == Some("running")
                    {
                        continue;
                    }
                    info!("Reaping idle link for session {}", session_id);
                    manager.detach(&session_id, "idle").await;
                }
            }
        });
    }

    /// The link for a session, or `None` if it is detached.
    pub async fn get(&self, session_id: &str) -> Option<Arc<Link>> {
        let mut links = self.links.write().await;
        let entry = links.get_mut(session_id)?;
        entry.last_used_ms = now_ms();
        Some(entry.link.clone())
    }

    pub async fn state(&self, session_id: &str) -> LinkState {
        let links = self.links.read().await;
        let Some(entry) = links.get(session_id) else {
            return LinkState::detached();
        };
        let identity = entry.link.identity();
        LinkState {
            attached: true,
            agent: Some(entry.agent.clone()),
            supports: identity
                .supports
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            capabilities: Some(entry.link.capabilities().await),
        }
    }

    pub async fn attached_count(&self) -> usize {
        self.links.read().await.len()
    }

    /// The link for a session, establishing one if needed.
    ///
    /// This is the path every turn takes. Establishment is serialized per session,
    /// and the check is repeated inside the lock: the first caller through creates
    /// the link, and the second finds it rather than making a second one.
    pub async fn ensure(&self, session_id: &str) -> Result<Arc<Link>> {
        if let Some(link) = self.get(session_id).await {
            return Ok(link);
        }

        let gate = {
            let mut establishing = self.establishing.lock().await;
            establishing
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = gate.lock().await;

        // Whoever held the gate before us may have established it.
        if let Some(link) = self.get(session_id).await {
            return Ok(link);
        }

        self.establish(session_id).await
    }

    async fn establish(&self, session_id: &str) -> Result<Arc<Link>> {
        let session = self
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow!("Unknown session {}", session_id))?;

        let adapter_path = self.agents_dir.join(&session.agent).join("adapter.sh");
        if !adapter_path.exists() {
            return Err(anyhow!(
                "No adapter for agent '{}' at {:?}",
                session.agent,
                adapter_path
            ));
        }

        // Identity is the server's decision, and the empty string is how the
        // protocol spells "inject nothing". A preserved session must send it
        // explicitly: omitting the field means "resolve your default", which
        // would inject into a transcript whose identity is already fixed.
        let soul = match session.identity_mode.as_str() {
            "preserve" => Some(String::new()),
            _ => self
                .soul_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        };

        let flags: HashMap<String, String> = serde_json::from_str::<serde_json::Value>(
            &session.flags_json,
        )
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .or_else(|| Some((k, v.to_string())))
                })
                .collect()
        })
        .unwrap_or_default();

        info!(
            "Establishing link for session {} ({})",
            session_id, session.agent
        );

        let (link, events) = Link::establish(
            self.environment.as_ref(),
            LinkConfig {
                adapter_path,
                workdir: session.workdir.clone(),
                session_id: session.backend_session_id.clone(),
                soul,
                flags,
                permission_mode: None,
                env: HashMap::new(),
            },
        )
        .await
        .with_context(|| format!("Failed to establish a link for session {}", session_id))?;

        let link = Arc::new(link);

        // The agent may have assigned an id, or resumed the one we gave it.
        // Persisting it is what makes the *next* link a continuation rather than
        // a fresh conversation.
        if let Some(native_id) = link.native_session_id().await {
            if session.backend_session_id.as_deref() != Some(native_id.as_str()) {
                let incarnation = self.store.active_incarnation(session_id).await.ok().flatten();
                if let Err(error) = self
                    .store
                    .set_backend_session_id(
                        session_id,
                        incarnation.as_ref().map(|i| i.id.as_str()),
                        &native_id,
                    )
                    .await
                {
                    error!("Failed to persist native session id: {}", error);
                }
            }
        }

        self.links.write().await.insert(
            session_id.to_string(),
            Entry {
                link: link.clone(),
                last_used_ms: now_ms(),
                agent: session.agent.clone(),
            },
        );

        self.spawn_drain(session_id.to_string(), events);

        Ok(link)
    }

    /// Consume a link's output for as long as it lives.
    ///
    /// One task per link, and the only consumer: the channel is not a broadcast,
    /// so a second reader would steal messages rather than duplicate them.
    fn spawn_drain(&self, session_id: String, mut events: tokio::sync::mpsc::Receiver<LinkEvent>) {
        let manager = self.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    LinkEvent::Event(body) => {
                        manager.absorb_event(&session_id, body).await;
                    }
                    LinkEvent::Request { id, request } => {
                        manager.absorb_request(&session_id, &id, request).await;
                    }
                    LinkEvent::Capabilities(surface) => {
                        // Published rather than journaled: it is current state,
                        // not something that happened, and the link already
                        // caches it for whoever asks later.
                        manager
                            .publish_session_event(
                                &session_id,
                                "capabilities",
                                serde_json::to_value(&surface).unwrap_or_default(),
                            )
                            .await;
                    }
                    LinkEvent::Closed { exit_code, stderr_tail } => {
                        manager
                            .handle_close(&session_id, exit_code, stderr_tail)
                            .await;
                        break;
                    }
                }
            }
            debug!("Drain finished for session {}", session_id);
        });
    }

    /// Journal a normalized event and publish it to attached clients.
    async fn absorb_event(&self, session_id: &str, body: serde_json::Value) {
        let turn_id = match self.store.running_turn_id(session_id).await {
            Ok(Some(id)) => id,
            // An event outside a turn has nowhere to belong. It happens: an
            // adapter may report a status as it settles. Logged rather than
            // journaled under a guessed turn, since a wrong turn id puts the
            // event in the wrong place in the transcript forever.
            _ => {
                debug!("Dropping out-of-turn event for session {}", session_id);
                return;
            }
        };

        let Some(mut event) = StreamEvent::from_adapter(&body) else {
            warn!("Unparseable adapter event on session {}", session_id);
            return;
        };

        match self.seq.next(session_id).await {
            Ok(seq) => event.seq = seq,
            Err(error) => {
                error!("Failed to allocate a sequence number: {}", error);
                return;
            }
        }

        if event.is_stable() {
            // Journaled before publishing, so a client that reconnects the
            // instant after seeing an event always finds it in replay.
            if let Err(error) = self
                .store
                .append_event(&event.to_row(session_id, &turn_id))
                .await
            {
                error!("Failed to journal event: {}", error);
            }
        }

        self.hub.publish(&turn_id, event).await;
    }

    /// Persist a request and announce it.
    ///
    /// Journaled first. An approval blocks the agent, so if the process dies
    /// between here and the publish, the record of what was being asked is what
    /// explains the stall — and it is answerable later from any frontend.
    async fn absorb_request(&self, session_id: &str, adapter_id: &str, request: crate::adapter::AdapterRequest) {
        let turn_id = self
            .store
            .running_turn_id(session_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

        let record = PendingRequest {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_id,
            adapter_request_id: adapter_id.to_string(),
            kind: request.kind.clone(),
            tool: request.tool.clone(),
            tool_use_id: request.tool_use_id.clone(),
            title: request.title.clone(),
            detail_json: request
                .detail
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "{}".to_string()),
            options_json: serde_json::to_string(&request.options).unwrap_or_else(|_| "[]".into()),
            default_option: request.default.clone(),
            risk: request.risk.clone(),
            status: "pending".to_string(),
            chosen_option: None,
            resolved_input_json: None,
            resolved_by: None,
            created_at_ms: now_ms(),
            resolved_at_ms: None,
        };

        let stored = match self.store.create_pending_request(&record).await {
            Ok(stored) => stored,
            Err(error) => {
                error!("Failed to persist a pending request: {}", error);
                return;
            }
        };

        info!(
            "Session {} is waiting on a {} request ({})",
            session_id, stored.kind, stored.id
        );

        self.publish_session_event(
            session_id,
            "request",
            serde_json::to_value(&stored).unwrap_or_default(),
        )
        .await;
    }

    /// A link died. Record why, and free anything waiting on it.
    async fn handle_close(&self, session_id: &str, exit_code: Option<i32>, stderr_tail: String) {
        self.links.write().await.remove(session_id);

        match self.store.abandon_open_requests(session_id).await {
            Ok(0) => {}
            Ok(count) => warn!(
                "Abandoned {} unanswered request(s) on session {}: its agent exited",
                count, session_id
            ),
            Err(error) => error!("Failed to abandon open requests: {}", error),
        }

        let mut data = serde_json::json!({"reason": "closed", "exitCode": exit_code});
        if !stderr_tail.is_empty() {
            data["stderr"] = stderr_tail.into();
        }
        self.publish_session_event(session_id, "link", data).await;
    }

    /// Close a link deliberately.
    pub async fn detach(&self, session_id: &str, reason: &str) {
        let entry = self.links.write().await.remove(session_id);
        let Some(entry) = entry else { return };

        entry.link.close().await;
        let _ = self.store.abandon_open_requests(session_id).await;
        self.publish_session_event(
            session_id,
            "link",
            serde_json::json!({"reason": reason, "attached": false}),
        )
        .await;
    }

    /// Publish a session-scoped fact.
    ///
    /// Links, capability changes, and requests are properties of a session rather
    /// than of a turn, so a client watching only turns would miss an approval
    /// raised while it was idle.
    async fn publish_session_event(&self, session_id: &str, kind: &str, data: serde_json::Value) {
        let event = StreamEvent {
            seq: self.seq.next(session_id).await.unwrap_or(0),
            kind: kind.to_string(),
            phase: None,
            stream_id: None,
            name: None,
            text: String::new(),
            data: Some(data),
            is_error: false,
            stability: "stable".to_string(),
            source: "server".to_string(),
            created_at_ms: now_ms(),
        };
        self.hub.publish_session(session_id, event).await;
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
