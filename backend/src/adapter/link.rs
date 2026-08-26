//! A live adapter process, and the correlation that makes it a conversation.
//!
//! A link is deliberately **disposable**. It does not survive a server restart,
//! it idles out, and a turn on a detached session establishes a new one by
//! resuming the agent's native session id — which is exactly what v1 did every
//! turn. Treating the link as precious would mean a restart lost history;
//! treating it as cheap means the worst case is the old behaviour.
//!
//! Both sides originate messages concurrently, so every command carries an id
//! and every reply echoes it. The alternative — implicit request/response
//! ordering — deadlocks the first time an adapter raises an approval while the
//! server is awaiting an `ok`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, warn};

use super::wire::{
    parse_inbound, AdapterRequest, Capability, CapabilityFacets, FlagSpec, Inbound, Outbound,
    SessionCapabilities,
};
use crate::env::{Environment, ProcessSignal, ProcessSpec, Signal};

/// How long the handshake may take before the adapter is called broken.
///
/// Matches v1's `--describe` budget: an adapter that cannot answer who it is in
/// ten seconds is not going to run a turn.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long any other command may wait for its reply.
///
/// A turn is not a command, so this does not bound how long an agent may think:
/// `send` is acknowledged when accepted, and completion arrives as an event.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// What the adapter told us about itself during the handshake.
#[derive(Debug, Clone)]
pub struct AdapterIdentity {
    pub adapter: String,
    pub agent_name: String,
    pub agent_version: Option<String>,
    pub supports: Vec<Capability>,
    pub flags: HashMap<String, FlagSpec>,
}

impl AdapterIdentity {
    pub fn can(&self, capability: Capability) -> bool {
        self.supports.contains(&capability)
    }
}

/// An adapter refused a command, in its own words and with its own code.
///
/// A typed error rather than a formatted string because the two facts it carries have
/// different audiences: `message` is for a person, and `code` decides which HTTP status
/// and which UI affordance the refusal earns. `UNSUPPORTED_FIELD` means the control
/// should not be offered; `INVALID_VALUE` means the control is fine and this one value
/// is not — and answering the second with the first tells a user their agent cannot do
/// something it does perfectly well.
///
/// Reached through `anyhow`'s `downcast_ref`, so the many layers between an adapter and
/// an HTTP handler need no signature change to pass it along, and a caller that does
/// not care keeps treating it as an opaque error.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AdapterError {
    /// The protocol's error code, as the adapter declared it.
    pub code: String,
    pub message: String,
}

/// Something that happened on a link, for whoever is driving the session.
#[derive(Debug, Clone)]
pub enum LinkEvent {
    /// A normalized event to journal and publish.
    Event(serde_json::Value),
    /// The adapter is waiting on a human. Carries the id a resolve must echo.
    Request { id: String, request: AdapterRequest },
    /// The capability surface changed — a skill was edited, a plugin loaded.
    Capabilities(SessionCapabilities),
    /// The adapter exited. Terminal: no further events arrive on this link.
    Closed { exit_code: Option<i32>, stderr_tail: String },
}

/// How to start one adapter.
pub struct LinkConfig {
    pub adapter_path: PathBuf,
    pub workdir: String,
    /// Agent-native session id to resume, or `None` for a fresh session.
    pub session_id: Option<String>,
    /// A path, or `Some("")` to inject nothing, or `None` to let the adapter
    /// resolve its own default. Three distinct meanings, which is why this is
    /// not a `bool` plus a path.
    pub soul: Option<String>,
    pub flags: HashMap<String, String>,
    pub permission_mode: Option<String>,
    pub env: HashMap<String, String>,
}

/// Commands awaiting their reply, keyed by the id we sent.
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value>>>>>;

/// A live adapter process.
pub struct Link {
    /// Serialized writes. A channel rather than a shared `AsyncWrite` because
    /// two concurrent writers can interleave halves of two JSON lines, which
    /// corrupts the stream in a way that is nearly impossible to read back from
    /// a log.
    outbox: mpsc::Sender<String>,
    pending: Pending,
    next_id: AtomicU64,
    signaler: Arc<dyn ProcessSignal>,
    identity: AdapterIdentity,
    capabilities: Arc<Mutex<SessionCapabilities>>,
    /// The agent-native session id, learned at `open` and needed to resume.
    native_session_id: Arc<Mutex<Option<String>>>,
}

impl Link {
    /// Spawn an adapter, handshake, and open a session on it.
    ///
    /// Returns the link and the stream of things that happen on it. Failing
    /// anywhere here leaves no live process: the adapter is killed rather than
    /// left running with nobody reading its output.
    pub async fn establish(
        environment: &dyn Environment,
        config: LinkConfig,
    ) -> Result<(Self, mpsc::Receiver<LinkEvent>)> {
        let mut env = config.env.clone();
        // Adapters share the v1 convention for the identity path, so an adapter
        // author has one rule to learn rather than two.
        if let Some(soul) = config.soul.as_ref() {
            env.insert("SYLPH_SOUL".into(), soul.clone());
        }

        let mut process = environment
            .spawn(ProcessSpec {
                program: config.adapter_path.clone(),
                args: vec!["--serve".into()],
                cwd: config.workdir.clone(),
                env,
            })
            .await
            .with_context(|| format!("Failed to spawn adapter {:?}", config.adapter_path))?;

        let mut stdin = process
            .take_stdin()
            .ok_or_else(|| anyhow!("Adapter stdin unavailable"))?;
        let stdout = process
            .take_stdout()
            .ok_or_else(|| anyhow!("Adapter stdout unavailable"))?;
        let stderr = process.take_stderr();
        let signaler = process.signaler();

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let capabilities = Arc::new(Mutex::new(SessionCapabilities::default()));
        let native_session_id = Arc::new(Mutex::new(config.session_id.clone()));

        let (outbox_tx, mut outbox_rx) = mpsc::channel::<String>(64);
        let (events_tx, events_rx) = mpsc::channel::<LinkEvent>(256);

        // Writer: the only task holding stdin, so lines cannot interleave.
        tokio::spawn(async move {
            while let Some(line) = outbox_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
            // Dropping stdin here is what tells a well-behaved adapter to exit.
            drop(stdin);
        });

        // Stderr is captured for diagnostics and never parsed. Draining it
        // concurrently matters: a chatty adapter that fills the stderr pipe
        // while we wait on stdout deadlocks.
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = stderr {
            let sink = stderr_tail.clone();
            tokio::spawn(async move {
                const TAIL_LIMIT: usize = 4096;
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut tail = sink.lock().await;
                    tail.push_str(&line);
                    tail.push('\n');
                    if tail.len() > TAIL_LIMIT * 2 {
                        let cut = tail.len() - TAIL_LIMIT;
                        *tail = tail.split_off(cut);
                    }
                }
            });
        }

        // Reader: routes replies to waiters and everything else to the consumer.
        {
            let pending = pending.clone();
            let capabilities = capabilities.clone();
            let events_tx = events_tx.clone();
            let stderr_tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let Some(inbound) = parse_inbound(trimmed) else {
                        debug!("Adapter diagnostic: {}", trimmed);
                        continue;
                    };

                    match inbound {
                        Inbound::Ok { id, body } => {
                            if let Some(waiter) = pending.lock().await.remove(&id) {
                                let _ = waiter.send(Ok(body));
                            } else {
                                warn!("Adapter replied to unknown command id {}", id);
                            }
                        }
                        Inbound::Err { id, code, message } => {
                            if let Some(waiter) = pending.lock().await.remove(&id) {
                                // Typed, so a caller can branch on the adapter's own
                                // code. It used to be flattened into `"{code}: {msg}"`,
                                // which left every refusal indistinguishable to
                                // anything above — a *value* the agent rejected and a
                                // *field* it does not have both surfaced as
                                // `AGENT_UNSUPPORTED`, whose contract is "disable the
                                // control". Disabling a permission picker because one
                                // of its six values was refused is the wrong repair to
                                // the wrong layer. Recovering the code by parsing the
                                // message back out was the alternative and is worse:
                                // the text is the adapter's and may be reworded, while
                                // the code is a declared part of the protocol.
                                let _ = waiter.send(Err(anyhow::Error::new(
                                    AdapterError { code, message },
                                )));
                            } else {
                                warn!("Adapter error for unknown command id {}: {}", id, message);
                            }
                        }
                        Inbound::Request { id, request } => {
                            let _ = events_tx.send(LinkEvent::Request { id, request }).await;
                        }
                        Inbound::Event { body } => {
                            // A capability change is an event on the wire but a
                            // state change here, so it updates the cached surface
                            // before being forwarded.
                            //
                            // Applied facet by facet. Assigning the whole surface is
                            // what erased 46 slash commands the first time an
                            // adapter reported only a permission-mode change, and
                            // the symptom appeared two layers away as a menu that
                            // would not open.
                            //
                            // `facets` is the only key read. A v2 whole-surface
                            // `capabilities` member used to be accepted as a
                            // fallback, and that was the wrong shape to tolerate:
                            // its field names parse as facets, so a partial update
                            // arriving under the old key would have been applied as
                            // the whole surface — reintroducing the exact erasure
                            // above, silently, from the branch meant to soften it.
                            // No adapter has emitted it since v3.
                            let is_capabilities =
                                body.get("kind").and_then(|k| k.as_str()) == Some("capabilities");
                            if is_capabilities {
                                let facets = body
                                    .get("facets")
                                    .cloned()
                                    .and_then(|c| {
                                        serde_json::from_value::<CapabilityFacets>(c).ok()
                                    });
                                if let Some(facets) = facets {
                                    if facets.is_empty() {
                                        // Naming no facet is a no-op, never a clear.
                                        debug!("Ignoring a capability update naming no facet");
                                        continue;
                                    }
                                    let snapshot = {
                                        let mut surface = capabilities.lock().await;
                                        facets.apply_to(&mut surface);
                                        surface.clone()
                                    };
                                    let _ =
                                        events_tx.send(LinkEvent::Capabilities(snapshot)).await;
                                    continue;
                                }
                            }
                            let _ = events_tx.send(LinkEvent::Event(body)).await;
                        }
                        Inbound::Unknown { t } => {
                            // Additive by design: a type we do not know is the
                            // adapter being newer, not the adapter being wrong.
                            debug!("Ignoring unknown adapter message type '{}'", t);
                        }
                    }
                }

                // stdout closed: the adapter is done. Reap it so the exit code
                // is real rather than inferred, and fail every outstanding
                // command so no caller waits on a reply that cannot arrive.
                let exit_code = process.wait().await.ok().flatten();
                let tail = stderr_tail.lock().await.trim().to_string();
                for (_, waiter) in pending.lock().await.drain() {
                    let _ = waiter.send(Err(anyhow!("Adapter exited before replying")));
                }
                let _ = events_tx
                    .send(LinkEvent::Closed { exit_code, stderr_tail: tail })
                    .await;
            });
        }

        let link = Self {
            outbox: outbox_tx,
            pending,
            next_id: AtomicU64::new(1),
            signaler,
            // Replaced by the handshake below; a link is never handed out with
            // this placeholder.
            identity: AdapterIdentity {
                adapter: String::new(),
                agent_name: String::new(),
                agent_version: None,
                supports: Vec::new(),
                flags: HashMap::new(),
            },
            capabilities,
            native_session_id,
        };

        // Held separately because `handshake` consumes the link, and a failed
        // handshake still has to kill the process it started.
        let kill_on_failure = link.signaler.clone();
        let link = match link.handshake(&config).await {
            Ok(link) => link,
            Err(error) => {
                // Never leave a process running that nobody is reading: its
                // stdout has no consumer and it would sit there until the server
                // exits.
                let _ = kill_on_failure.signal(Signal::Kill).await;
                return Err(error);
            }
        };

        Ok((link, events_rx))
    }

    /// `hello` then `open`, both bounded by [`HANDSHAKE_TIMEOUT`].
    ///
    /// Two round trips rather than one because they answer different questions:
    /// `hello` is what the adapter and its CLI can do at all, `open` is what
    /// this session in this directory can do — a command list depends on the
    /// project, so it is not knowable until the CLI has loaded it.
    async fn handshake(mut self, config: &LinkConfig) -> Result<Self> {
        let hello = self
            .call_with_timeout(Outbound::Hello, HANDSHAKE_TIMEOUT)
            .await
            .context("Adapter handshake failed")?;

        let agent = hello.get("agent");
        let identity = AdapterIdentity {
            adapter: string_at(&hello, "adapter").unwrap_or_else(|| "unknown".into()),
            agent_name: agent
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            agent_version: agent
                .and_then(|a| a.get("version"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Filtered per entry rather than deserialized as a whole list: one
            // capability name we do not model must not discard the rest.
            supports: hello
                .get("supports")
                .and_then(|s| s.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_default(),
            flags: hello
                .get("flags")
                .cloned()
                .and_then(|f| serde_json::from_value(f).ok())
                .unwrap_or_default(),
        };

        if identity.agent_name.is_empty() {
            bail!("Adapter handshake omitted its agent name");
        }
        self.identity = identity;

        let opened = self
            .call_with_timeout(
                Outbound::Open {
                    workdir: config.workdir.clone(),
                    session_id: config.session_id.clone(),
                    soul: config.soul.clone(),
                    flags: config.flags.clone(),
                    permission_mode: config.permission_mode.clone(),
                    env: HashMap::new(),
                },
                HANDSHAKE_TIMEOUT,
            )
            .await
            .context("Adapter refused to open the session")?;

        // The id the agent actually used. Without it a later turn cannot resume,
        // so an adapter that omits it has produced an unresumable session.
        let session_id = string_at(&opened, "sessionId")
            .ok_or_else(|| anyhow!("Adapter opened a session without reporting its id"))?;
        *self.native_session_id.lock().await = Some(session_id);

        if let Ok(surface) = serde_json::from_value::<SessionCapabilities>(opened) {
            *self.capabilities.lock().await = surface;
        }

        Ok(self)
    }

    /// Send a command and wait for its reply.
    async fn call_with_timeout(
        &self,
        message: Outbound,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let id = format!("c{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();

        // Registered before the write, or a fast adapter can reply before there
        // is anywhere to route the reply to.
        self.pending.lock().await.insert(id.clone(), tx);

        if self.outbox.send(message.to_line(&id)).await.is_err() {
            self.pending.lock().await.remove(&id);
            bail!("Adapter is no longer accepting input");
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => bail!("Adapter closed before replying"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("Adapter did not reply within {:?}", timeout)
            }
        }
    }

    async fn call(&self, message: Outbound) -> Result<serde_json::Value> {
        self.call_with_timeout(message, COMMAND_TIMEOUT).await
    }

    /// Fire a message with no reply expected.
    async fn tell(&self, message: Outbound) -> Result<()> {
        let id = format!("c{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        self.outbox
            .send(message.to_line(&id))
            .await
            .map_err(|_| anyhow!("Adapter is no longer accepting input"))
    }

    pub fn identity(&self) -> &AdapterIdentity {
        &self.identity
    }

    pub async fn capabilities(&self) -> SessionCapabilities {
        self.capabilities.lock().await.clone()
    }

    pub async fn native_session_id(&self) -> Option<String> {
        self.native_session_id.lock().await.clone()
    }

    /// Start a turn. Returns when the adapter has *accepted* it, not when the
    /// agent has finished: a turn outlives the request that started it, which is
    /// the same reason `POST /turns` answers `202`.
    pub async fn send(
        &self,
        text: String,
        attachments: Vec<String>,
        command: Option<String>,
        args: HashMap<String, String>,
        config: HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        self.call(Outbound::Send { text, attachments, command, args, config })
            .await
            .map(|_| ())
    }

    /// End the current turn, keep the session.
    ///
    /// Distinct from a stop signal, which kills the process. Conflating them
    /// loses one of two real intentions: "that is not what I meant" versus
    /// "stop everything".
    pub async fn interrupt(&self) -> Result<()> {
        if !self.identity.can(Capability::Interrupt) {
            bail!("{} cannot interrupt a turn", self.identity.agent_name);
        }
        self.call(Outbound::Interrupt).await.map(|_| ())
    }

    /// Add text to the turn named by `expected_turn_id`.
    ///
    /// Awaited rather than fired and forgotten, because the adapter is the only
    /// thing that can tell us the turn moved on: a steer that missed its turn has
    /// to surface as an error, not as a message quietly delivered into the next
    /// conversation.
    pub async fn steer(&self, text: String, expected_turn_id: Option<String>) -> Result<()> {
        if !self.identity.can(Capability::Steer) {
            bail!("{} cannot be steered mid-turn", self.identity.agent_name);
        }
        self.call(Outbound::Steer { text, expected_turn_id })
            .await
            .map(|_| ())
    }

    /// Set one config option, and take the refreshed surface from the reply.
    ///
    /// The reply carries the whole `config` facet because setting one option can
    /// change another's choices — `pi`'s thinking levels depend on the selected
    /// model, and `claude` carries per-model effort levels. Reading it back here is
    /// what keeps the cached surface true without a second round trip.
    pub async fn set_config(
        &self,
        config_id: &str,
        value: serde_json::Value,
    ) -> Result<SessionCapabilities> {
        if !self.identity.can(Capability::SetConfig) {
            bail!("{} cannot change config on a live session", self.identity.agent_name);
        }
        let reply = self
            .call(Outbound::SetConfig { config_id: config_id.to_string(), value })
            .await?;

        let facets = serde_json::from_value::<CapabilityFacets>(
            reply.get("facets").cloned().unwrap_or(reply.clone()),
        )
        .unwrap_or_default();
        let mut surface = self.capabilities.lock().await;
        if !facets.is_empty() {
            facets.apply_to(&mut surface);
        }
        Ok(surface.clone())
    }

    /// Answer a request the adapter raised.
    ///
    /// Fire-and-forget: the adapter's acknowledgement is the turn continuing, and
    /// waiting for a reply here would block a resolve behind whatever the agent
    /// does next with the answer.
    pub async fn resolve(
        &self,
        request_id: &str,
        option: &str,
        input: Option<serde_json::Value>,
    ) -> Result<()> {
        self.tell(Outbound::Resolve {
            request_id: request_id.to_string(),
            option: option.to_string(),
            input,
        })
        .await
    }

    pub async fn query(&self, what: &str) -> Result<serde_json::Value> {
        self.call(Outbound::Query { what: what.to_string() }).await
    }

    /// Ask the adapter to exit, then make sure it did.
    ///
    /// A `close` an adapter ignores must not leak a process, so the grace period
    /// is followed by a signal. Escalation is unconditional rather than checked,
    /// because signalling an already-exited group is defined to be harmless.
    pub async fn close(&self) {
        let _ = self.tell(Outbound::Close).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = self.signaler.signal(Signal::Term).await;
    }

    /// A signal channel that outlives this borrow, for a hard stop.
    pub fn signaler(&self) -> Arc<dyn ProcessSignal> {
        self.signaler.clone()
    }
}

fn string_at(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}
