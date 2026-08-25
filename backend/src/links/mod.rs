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
use crate::db::JournalStore;
use crate::env::Environment;
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::models::PendingRequest;
use crate::runners::Registry;

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
    /// Why there is no live link, when establishing one was tried and failed.
    ///
    /// An empty capability surface is ambiguous in a way that matters: an agent
    /// that declares no commands, an adapter that does not exist, and a handshake
    /// still in flight are three different situations needing three different
    /// responses from the user, and they were indistinguishable to the client. The
    /// server logged the reason and returned a bare `attached: false`, so the UI
    /// could only show nothing — which is how a stale install presented as a
    /// composer that ignored the `/` key.
    ///
    /// `None` means no attempt failed: either a link is attached, or nobody has
    /// asked for one yet. A failure is not cached — the next read tries again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<LinkFailure>,
}

/// Why a link could not be established, in a shape a client can act on.
///
/// `code` is a closed vocabulary so the UI can choose its own words and offer the
/// right recovery, and `message` carries the server's detail for the case where a
/// human has to read it. A code the client does not recognize still renders,
/// because the message is always populated.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkFailure {
    pub code: LinkFailureCode,
    pub message: String,
}

/// The session names an agent discovery did not find.
///
/// A distinct error type rather than a message to match on, so classification
/// survives the wording being edited. It is the one link failure a retry cannot
/// fix, which is exactly why the client has to be able to tell it apart.
#[derive(Debug, thiserror::Error)]
#[error("No adapter registered for agent '{0}'; it was not discovered")]
pub struct NoAdapter(pub String);

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkFailureCode {
    /// The session names an agent discovery did not find. Fixing this is an
    /// install or configuration change, not a retry.
    NoAdapter,
    /// The adapter exists but would not come up: a crash, a timeout, a refused
    /// handshake. Retrying can work.
    HandshakeFailed,
}

impl LinkState {
    fn detached() -> Self {
        Self {
            attached: false,
            agent: None,
            supports: Vec::new(),
            capabilities: None,
            failure: None,
        }
    }

    fn failed(failure: LinkFailure) -> Self {
        Self { failure: Some(failure), ..Self::detached() }
    }
}

#[derive(Clone)]
pub struct LinkManager {
    store: JournalStore,
    /// The batching front for the journal, used by the drain path.
    ///
    /// Deltas arrive here thousands per turn, so this is the one write path where
    /// batching matters. Held beside `store` rather than replacing it: the manager
    /// also does ordinary single-row work (persisting a native id, a pending
    /// request) that has nothing to batch.
    writer: crate::db::JournalWriter,
    hub: EventHub,
    environment: Arc<dyn Environment>,
    /// Where an adapter actually is, according to the thing that found it.
    ///
    /// Deliberately the registry rather than a directory to join a name onto.
    /// Discovery searches several roots, so re-deriving the path here meant an
    /// agent found in one root was listed as usable while every link to it failed
    /// with "No adapter for agent 'claude' at <the other root>". One component
    /// knows where adapters are, and this is not it.
    registry: Arc<Mutex<Registry>>,
    soul_path: Option<PathBuf>,
    seq: SeqAllocator,
    /// Turns waiting to be told how they ended.
    ///
    /// A turn's completion arrives here as an event, and the supervisor is what
    /// records terminal state. Rather than call back into it — which would be a
    /// cycle, since it calls `ensure` — the drain task resolves the channel the
    /// supervisor is parked on. Set once at startup.
    waiters: Arc<RwLock<Option<crate::supervisor::turn::TurnWaiters>>>,
    /// Records terminal state for a turn no waiter exists for.
    ///
    /// An agent-initiated turn has no waiter by definition — nobody asked for it, so
    /// nothing is parked on its outcome — and resolving a channel is therefore not
    /// enough to settle one. This is the supervisor's own settling path, factored out
    /// so both can reach it without the cycle a direct call back would create
    /// (`supervisor::closer`). Set once at startup, like `waiters`.
    closer: Arc<RwLock<Option<crate::supervisor::closer::TurnCloser>>>,
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
        store: JournalStore,
        writer: crate::db::JournalWriter,
        hub: EventHub,
        environment: Arc<dyn Environment>,
        registry: Arc<Mutex<Registry>>,
        soul_path: Option<PathBuf>,
        seq: SeqAllocator,
    ) -> Self {
        let manager = Self {
            store,
            writer,
            hub,
            environment,
            registry,
            soul_path,
            seq,
            waiters: Arc::new(RwLock::new(None)),
            closer: Arc::new(RwLock::new(None)),
            links: Arc::new(RwLock::new(HashMap::new())),
            establishing: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.spawn_reaper();
        manager
    }

    /// Hand the manager the supervisor's waiter registry.
    ///
    /// Wired after construction because the two reference each other: the
    /// supervisor asks the manager for a link, and the manager settles the
    /// supervisor's turns. One of the two edges has to be late, and this is the
    /// cheaper one — a manager with no waiters yet simply cannot settle a turn,
    /// and no turn exists before the supervisor does.
    pub async fn set_waiters(&self, waiters: crate::supervisor::turn::TurnWaiters) {
        *self.waiters.write().await = Some(waiters);
    }

    /// Hand the manager the settling path for turns with no waiter.
    ///
    /// Late for the same reason as `set_waiters`, and safe for the same reason: an
    /// agent cannot have started a turn before the thing that settles turns exists.
    pub async fn set_closer(&self, closer: crate::supervisor::closer::TurnCloser) {
        *self.closer.write().await = Some(closer);
    }

    /// Tell a waiting turn how it ended.
    async fn settle_turn(&self, session_id: &str, turn_id: &str, outcome: crate::supervisor::turn::Outcome) {
        // Cloned out from under the read guard, so the guard is not held across
        // the `lock().await` below — holding both would order two locks and
        // invite a deadlock the moment anything takes them the other way.
        let waiters = self.waiters.read().await.clone();
        let waiting = match waiters {
            // Taken out of the map in its own statement so the guard drops before the
            // send. An `if let` here would hold the lock across it, and the receiving
            // supervisor may re-enter this manager while draining.
            Some(waiters) => waiters.lock().await.remove(turn_id),
            None => None,
        };

        if let Some(tx) = waiting {
            // A send error means the supervisor stopped waiting, which happens
            // when a stop already settled the turn. Benign.
            let _ = tx.send(outcome);
            return;
        }

        // No waiter. Two very different reasons for that, and settling both here is a
        // bug: an agent-initiated turn never had one, while a *user's* turn whose
        // waiter is already gone is one the supervisor has claimed and is about to
        // settle itself.
        //
        // Claiming the waiter first is how a stop wins over the link teardown it
        // causes (see `TurnSupervisor::settle`). Settling every waiter-less turn here
        // broke that: a stop tore down the link, `handle_close` raised `LinkLost`, and
        // this path recorded it before the stop's own `cancelled` arrived — so a
        // deliberate stop was reported as the agent having exited. The old early
        // return was accidentally right about user turns; `origin` is what makes it
        // deliberate.
        match self.store.get_turn(turn_id).await {
            Ok(Some(turn)) if turn.is_agent_initiated() => {}
            Ok(_) => return,
            Err(error) => {
                error!("Could not read turn {} to settle it: {}", turn_id, error);
                return;
            }
        }

        // An agent-initiated turn is settled through the same closer the supervisor
        // uses, so it consolidates, closes what it left open, and returns the session
        // to idle by exactly the path a user's turn does. Left unsettled it would sit
        // `running` forever, and the session with it: the composer refuses a message
        // while a turn is live, so one unprompted turn would wedge the session.
        if let Some(closer) = self.closer.read().await.clone() {
            closer.close(session_id, turn_id, outcome).await;
        } else {
            // Only reachable before wiring completes, which is startup-order rather
            // than a runtime condition — but a silent skip here is a wedged session,
            // so it is worth a line.
            error!(
                "Turn {} completed with no waiter and no closer; it stays running",
                turn_id
            );
        }
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
            failure: None,
        }
    }

    /// The link state for a session whose establishment just failed.
    ///
    /// Classification happens here rather than in the handler because this module
    /// is what knows the difference: a name the registry does not hold can only be
    /// fixed by installing an adapter, while a handshake that timed out may work on
    /// the next try. Collapsing both into one error would make the UI offer a retry
    /// button for a condition no retry can change.
    pub async fn state_after_failure(&self, session_id: &str, error: &anyhow::Error) -> LinkState {
        // A link may have been established concurrently — by a turn, or by another
        // client — between the failure and this call. The live answer wins.
        let live = self.state(session_id).await;
        if live.attached {
            return live;
        }

        // Downcast rather than matching the message text: the reason a client sees
        // must not depend on the wording of a log line, which is edited freely.
        let code = if error.downcast_ref::<NoAdapter>().is_some() {
            LinkFailureCode::NoAdapter
        } else {
            LinkFailureCode::HandshakeFailed
        };
        LinkState::failed(LinkFailure { code, message: format!("{:#}", error) })
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

        // Asked of the registry, which is what discovered it. An agent that is not
        // there is genuinely unavailable, rather than merely absent from one of the
        // roots discovery happens to search.
        let adapter_path = {
            let registry = self.registry.lock().await;
            registry
                .adapter_path(&session.agent)
                .map(|path| path.to_path_buf())
                .ok_or_else(|| anyhow::Error::new(NoAdapter(session.agent.clone())))?
        };

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
    ///
    /// A `turn` event with `phase: completed` is also the signal that settles the
    /// waiting turn, because on a link a turn's outcome is an event rather than a
    /// process exit.
    async fn absorb_event(&self, session_id: &str, body: serde_json::Value) {
        let turn_id = match self.store.running_turn_id(session_id).await {
            Ok(Some(id)) => id,
            // No turn is running, and the agent is speaking anyway. That used to be
            // treated as impossible and the event was dropped — the reasoning being
            // that a guessed turn id puts an event in the wrong place in the
            // transcript forever, which is true, and which is why the answer is to
            // open the right turn rather than to guess one.
            //
            // Measured against claude 2.x: after a turn's `result`, a session with
            // backgrounded work emits `background_tasks_changed`, `task_updated`, and
            // `task_notification` when that work finishes, and then an entire exchange
            // of its own — `init`, an assistant message, another `result`. Its stdout
            // stays open and the process does not exit. So the drop discarded exactly
            // the completion a user had been promised, along with the agent's own
            // report of it, and no monitor, schedule, or background job could ever
            // reach a client however the rest of the stack was built.
            Ok(None) => match self.open_agent_turn(session_id).await {
                Some(id) => id,
                None => return,
            },
            Err(error) => {
                error!(
                    "Could not read the running turn for session {}: {}",
                    session_id, error
                );
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

        // Everything is journaled, including deltas.
        //
        // This reverses v1, where a delta was ephemeral and dropped. Lossless
        // replay needs them: consolidation folds a block's deltas into ~8 bytes
        // each, and it can only fold what the journal actually kept. Dropping them
        // here would make `docs/storage.md`'s whole guarantee unreachable — the
        // fold would have nothing to fold.
        //
        // The cost is bounded and short-lived. Deltas sit in the hot journal only
        // until the turn settles, which is when consolidation replaces them with
        // the folded form and deletes the rows.
        //
        // A failure here is loud on purpose. This was a real bug: a CHECK
        // constraint rejected every adapter event, and because the error was only
        // logged at debug level the turn streamed perfectly and came back empty on
        // reload. An event published but not journaled is worse than one that never
        // arrived, because the client has already been told it happened.
        // Stable events commit immediately; ephemeral ones join a ~50 ms window.
        // The writer holds that distinction rather than this call site, because
        // which events may be batched is a property of where a reconnect replays
        // them from — see [`crate::db::writer`].
        if let Err(error) = self
            .writer
            .append(&event.to_row(session_id, &turn_id), event.is_stable())
            .await
        {
            error!(
                "DATA LOSS: event seq {} kind '{}' on session {} was published but not journaled: {}",
                event.seq, event.kind, session_id, error
            );
        }

        let settles = event.kind == "turn" && event.phase.as_deref() == Some("completed");
        let is_error = event.is_error;
        let stop_reason = event
            .data
            .as_ref()
            .and_then(|d| d.get("stopReason"))
            .and_then(|r| r.as_str())
            .map(|s| s.to_string());
        let text = event.text.clone();

        self.hub.publish(&turn_id, event).await;

        if settles {
            use crate::supervisor::turn::Outcome;
            // The agent's own error flag decides this, not the transport: a turn
            // that ran and reported failure is a completed exchange the user
            // needs to see, not a broken link.
            let outcome = if is_error {
                Outcome::Failed {
                    message: if text.is_empty() {
                        "The agent reported a failure".to_string()
                    } else {
                        text
                    },
                }
            } else if stop_reason.as_deref() == Some("interrupted") {
                Outcome::Interrupted
            } else {
                Outcome::Completed { stop_reason }
            };
            self.settle_turn(session_id, &turn_id, outcome).await;
        }
    }

    /// Open a turn for an agent that spoke without being asked.
    ///
    /// A real turn, not a container: it consolidates, its events order by sequence,
    /// and it appears in the transcript where it happened. `origin` records that
    /// nobody prompted it, which a reader needs and an empty `request_text` cannot
    /// say on its own (`migrations/006_turn_origin.sql`).
    ///
    /// Opened on the *first* out-of-turn event of any kind rather than on a
    /// `turn`/`started`. The vendor's own turn boundary arrives partway through:
    /// `task_notification` — the completion this exists to deliver — precedes it. So
    /// waiting for the bracket would drop the payload and keep only the announcement.
    /// The adapter's later `turn`/`started` then lands inside this turn instead of
    /// opening a second one, because a turn is already running by then.
    ///
    /// `None` means the turn could not be recorded, and the caller drops the event.
    /// That is the same outcome as before for a session whose row is gone — a link
    /// outliving its session — and it stays a drop rather than an unattributed write.
    async fn open_agent_turn(&self, session_id: &str) -> Option<String> {
        let turn = crate::models::Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            incarnation_id: None,
            status: "running".to_string(),
            origin: "agent".to_string(),
            // Empty by construction: there was no request. `origin` is what
            // distinguishes this from a user who sent nothing.
            request_text: String::new(),
            command_invocation_id: None,
            idempotency_key: None,
            error_code: None,
            error_message: None,
            runner_exit_code: None,
            created_at_ms: now_ms(),
            started_at_ms: Some(now_ms()),
            completed_at_ms: None,
            stopped_at_ms: None,
        };

        match self.store.create_turn(&turn).await {
            Ok(created) => {
                info!(
                    "Session {} started a turn of its own ({})",
                    session_id, created.id
                );
                // The session is running again, so the composer reflects that rather
                // than inviting a message the agent is busy to receive.
                let _ = self.store.update_session_status(session_id, "running").await;
                // Announced on the session stream, which is the one a client holds
                // between turns. Without this a client learns about the turn only by
                // polling, so the notification it exists to carry arrives late or not
                // at all.
                self.publish_session_event(
                    session_id,
                    "turn-opened",
                    serde_json::json!({"turnId": created.id, "origin": "agent"}),
                )
                .await;
                Some(created.id)
            }
            Err(error) => {
                error!(
                    "Could not open an agent-initiated turn on session {}: {}",
                    session_id, error
                );
                None
            }
        }
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
            // Serialized rather than stored as sent, so what a client renders and what
            // the server validates an answer against are the same bytes. A form that
            // fails to serialize is dropped to `None`, which degrades to answering by
            // option rather than leaving the request unrenderable.
            form_json: request
                .form
                .as_ref()
                .and_then(|form| serde_json::to_string(form).ok()),
            default_option: request.default.clone(),
            risk: request.risk.clone(),
            allow_persistent: request.allow_persistent,
            expires_at_ms: request.expires_at_ms,
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

        // A turn still in flight will never hear from this agent again, so it is
        // settled here rather than left waiting on a channel whose sender is
        // about to drop. Doing it explicitly carries the stderr tail, which is the
        // only diagnostic there is — without it the timeline cannot distinguish
        // "the agent stopped" from "the agent finished".
        if let Ok(Some(turn_id)) = self.store.running_turn_id(session_id).await {
            self.settle_turn(
                session_id,
                &turn_id,
                crate::supervisor::turn::Outcome::LinkLost {
                    stderr_tail: stderr_tail.clone(),
                },
            )
            .await;
        }

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

    /// Re-publish the current capability surface to attached clients.
    ///
    /// Used after a config change made through the API: the client that made it
    /// learns the outcome from its own response, but every other attached client
    /// would otherwise keep showing the previous value.
    pub async fn announce_capabilities(&self, session_id: &str) {
        let Some(link) = self.get(session_id).await else {
            return;
        };
        let surface = link.capabilities().await;
        self.publish_session_event(
            session_id,
            "capabilities",
            serde_json::to_value(&surface).unwrap_or_default(),
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
            // Session-scoped, so never inside a delegation: this fires between
            // turns, when no trajectory is open.
            span: None,
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
