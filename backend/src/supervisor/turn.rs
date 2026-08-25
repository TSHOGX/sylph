//! Running a turn on a live link.
//!
//! The v1 supervisor spawned a process per turn and read its exit code to learn
//! the outcome. A turn on a link has no process of its own: it is a message into
//! a session, and its completion arrives as an event on the link's stream.
//!
//! That inverts where the turn's outcome comes from. The supervisor no longer
//! *drives* a turn to completion; it starts one and waits to be told. The link's
//! drain task in [`crate::links`] is what journals the events, so this module's
//! job is narrower than the old one's: mark running, send, and settle when the
//! `turn` event lands or the link dies.

use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use crate::db::LiveStore;
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::links::LinkManager;

/// Everything needed to run one turn.
pub struct TurnRequest {
    pub session_id: String,
    pub turn_id: String,
    pub incarnation_id: String,
    pub prompt: String,
    pub flags: HashMap<String, String>,
    pub attachments: Vec<String>,
    pub command: Option<String>,
    pub command_args: HashMap<String, String>,
}

/// How a turn ended, as reported by the link.
#[derive(Debug, Clone)]
pub enum Outcome {
    Completed {
        /// The agent's own stop reason, when it gave one.
        stop_reason: Option<String>,
    },
    Failed {
        message: String,
    },
    /// The user interrupted it. Distinct from failure: the turn did what was
    /// asked of it by stopping.
    Interrupted,
    /// The link died mid-turn. The turn's fate is unknown rather than known-bad,
    /// and saying so is more honest than calling it a failure the agent caused.
    LinkLost {
        stderr_tail: String,
    },
}

impl Outcome {
    /// The journal status and error code this outcome records.
    pub fn as_status(&self) -> (&'static str, Option<&'static str>, String) {
        match self {
            Self::Completed { .. } => ("completed", None, String::new()),
            Self::Failed { message } => ("failed", Some("TURN_FAILED"), message.clone()),
            Self::Interrupted => (
                "cancelled",
                Some("TURN_INTERRUPTED"),
                "Interrupted by user".to_string(),
            ),
            Self::LinkLost { stderr_tail } => (
                "failed",
                Some("LINK_LOST"),
                if stderr_tail.is_empty() {
                    "The agent exited during this turn".to_string()
                } else {
                    format!("The agent exited during this turn: {}", stderr_tail)
                },
            ),
        }
    }
}

/// Waiters for turns in flight, so the link's drain task can settle them.
///
/// A turn's completion arrives as an event, which the drain task sees. It cannot
/// call back into the supervisor without a cycle, so it resolves a channel the
/// supervisor is holding instead.
pub type TurnWaiters = std::sync::Arc<tokio::sync::Mutex<HashMap<String, oneshot::Sender<Outcome>>>>;

/// Runs turns against links.
#[derive(Clone)]
pub struct LinkedSupervisor {
    store: LiveStore,
    hub: EventHub,
    links: LinkManager,
    seq: SeqAllocator,
    waiters: TurnWaiters,
    /// Moves a settled turn's events into history.
    consolidator: crate::storage::Consolidator,
}

impl LinkedSupervisor {
    pub fn new(
        store: LiveStore,
        hub: EventHub,
        links: LinkManager,
        seq: SeqAllocator,
        consolidator: crate::storage::Consolidator,
    ) -> Self {
        Self {
            store,
            hub,
            links,
            seq,
            waiters: Default::default(),
            consolidator,
        }
    }

    /// The waiter registry, handed to the link manager so its drain task can
    /// settle turns it sees complete.
    pub fn waiters(&self) -> TurnWaiters {
        self.waiters.clone()
    }

    /// Start a turn and return immediately.
    ///
    /// The turn owns its outcome from here: a detached task records a terminal
    /// state whatever happens, including when establishing the link fails, so a
    /// turn can never be left permanently `running`.
    pub async fn spawn(&self, request: TurnRequest) {
        let supervisor = self.clone();
        tokio::spawn(async move {
            let turn_id = request.turn_id.clone();
            let session_id = request.session_id.clone();
            if let Err(error) = supervisor.run(request).await {
                error!("Turn {} failed to start: {:#}", turn_id, error);
                supervisor
                    .settle(
                        &session_id,
                        &turn_id,
                        Outcome::Failed {
                            message: format!("{:#}", error),
                        },
                    )
                    .await;
            }
        });
    }

    async fn run(&self, request: TurnRequest) -> Result<()> {
        let TurnRequest {
            session_id,
            turn_id,
            incarnation_id,
            prompt,
            flags,
            attachments,
            command,
            command_args,
        } = request;

        self.hub.open(&turn_id).await;

        // Registered before the turn is marked running, so there is no window in
        // which the journal says `running` but no waiter exists. A `stop` landing
        // in that window would find a running turn it could not settle and report
        // STOP_ALREADY_COMPLETED — which showed up as a flaky test under parallel
        // load rather than as an obvious bug.
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().await.insert(turn_id.clone(), tx);

        if !self.store.mark_turn_running(&turn_id, &incarnation_id).await? {
            // Already claimed, stopped, or interrupted before it began.
            debug!("Turn {} was no longer queued; skipping", turn_id);
            self.waiters.lock().await.remove(&turn_id);
            self.hub.close(&turn_id).await;
            return Ok(());
        }
        self.store.update_session_status(&session_id, "running").await?;

        // The user's own message is durable transcript, journaled as an event
        // rather than living only in `turns.request_text`.
        if !prompt.is_empty() {
            self.emit(&session_id, &turn_id, server_event("user", prompt.clone(), None))
                .await?;
        }

        // Establishing may take seconds — an agent CLI loads a project — so the
        // status event goes out first and the UI can say what it is waiting for.
        self.emit(
            &session_id,
            &turn_id,
            server_event(
                "status",
                String::new(),
                Some(serde_json::json!({"state": "starting"})),
            ),
        )
        .await?;

        let link = match self.links.ensure(&session_id).await {
            Ok(link) => link,
            Err(error) => {
                self.waiters.lock().await.remove(&turn_id);
                return Err(error);
            }
        };

        if let Some(name) = command.as_deref() {
            info!("Turn {} invokes /{}", turn_id, name);
        }

        // Config travels with the turn rather than as calls before it.
        //
        // The previous shape was one `set` per flag and then `send` — extra round
        // trips with a window in between where a turn could begin under a mix of
        // old and new settings, and a failure partway left no single answer to
        // "what did this turn run as". Sending them together makes the turn and its
        // settings one atom, which is also how codex models a turn-scoped override.
        let config: HashMap<String, serde_json::Value> = flags
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect();

        link.send(prompt, attachments, command, command_args, config)
            .await
            .map_err(|error| {
                anyhow::anyhow!("The agent would not accept this turn: {:#}", error)
            })?;

        // No timeout: an agent thinking for an hour is normal, and the escape
        // hatches are interrupt and stop. A deadline here would kill exactly the
        // long task a user most wants to leave running.
        let outcome = match rx.await {
            Ok(outcome) => outcome,
            // The sender was dropped without settling, which means the link died
            // and its drain task went with it.
            Err(_) => Outcome::LinkLost {
                stderr_tail: String::new(),
            },
        };

        self.settle(&session_id, &turn_id, outcome).await;
        Ok(())
    }

    /// Whether a session has a turn in flight.
    ///
    /// Read from the journal rather than from an in-memory set, which is what
    /// makes it correct across a restart: a turn left `running` by a dead process
    /// is reconciled at startup, so the journal is the only place that knows the
    /// truth after one.
    pub async fn session_busy(&self, session_id: &str) -> bool {
        self.store
            .running_turn_id(session_id)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    pub async fn active_turn_count(&self) -> usize {
        self.waiters.lock().await.len()
    }

    /// Stop a turn: ask the agent to end it, and settle it either way.
    ///
    /// Prefers `interrupt` over killing the process, because a session is worth
    /// more than a turn — an agent that has loaded a project and built context
    /// should survive the user changing their mind. Killing the adapter is the
    /// fallback for an agent that cannot interrupt, and it costs the session.
    ///
    /// Returns false when the turn is not in flight, which the caller reports as
    /// `STOP_ALREADY_COMPLETED`.
    pub async fn stop(&self, turn_id: &str) -> crate::error::Result<bool> {
        let session_id = match self.store.get_turn(turn_id).await? {
            Some(turn) if turn.status == "running" || turn.status == "queued" => turn.session_id,
            _ => return Ok(false),
        };

        if !self.waiters.lock().await.contains_key(turn_id) {
            return Ok(false);
        }

        match self.links.get(&session_id).await {
            Some(link) => {
                if link.interrupt().await.is_err() {
                    // Either the agent cannot interrupt or it refused. Falling
                    // back to a signal is what makes stop unconditional: a user
                    // asking to stop must not be told the agent declined.
                    warn!(
                        "Turn {} could not be interrupted; tearing the link down",
                        turn_id
                    );
                    // Settled *before* detaching, and this ordering is the whole
                    // point. Tearing the link down makes its drain task observe a
                    // close and settle the turn as LINK_LOST — which is true of
                    // the transport and false about what happened: the user asked
                    // for this. Claiming the outcome first means the drain task's
                    // later attempt finds the turn already settled and does
                    // nothing, so the record says cancelled.
                    self.settle(&session_id, turn_id, Outcome::Interrupted).await;
                    self.links.detach(&session_id, "stopped").await;
                    return Ok(true);
                }
            }
            None => {
                // No link, but a turn still marked running: settle it rather than
                // leaving the row inconsistent with reality.
                debug!("Stop for turn {} found no link", turn_id);
            }
        }

        // Settled here rather than waiting for a `turn` event, because an
        // interrupted agent may not emit one. If it does, `finish_turn`'s guard
        // makes this the outcome that stands.
        self.settle(&session_id, turn_id, Outcome::Interrupted).await;
        Ok(true)
    }

    /// Record a turn's terminal state and close its stream.
    async fn settle(&self, session_id: &str, turn_id: &str, outcome: Outcome) {
        let (status, error_code, message) = outcome.as_status();

        // Claimed before the waiter is dropped, and that order is load-bearing.
        //
        // Removing the waiter drops the oneshot sender, which wakes whoever is
        // parked in `run()` with a `RecvError` — and that path reads as "the link
        // died", so it settles the turn as LINK_LOST. Claiming first means that
        // second settlement finds the turn already terminal and does nothing.
        // Reversed, a user's stop is reported as the agent having exited: true of
        // the transport, false about what happened.
        let settled = match self
            .store
            .finish_turn(turn_id, status, None, error_code, Some(&message))
            .await
        {
            Ok(settled) => settled,
            Err(error) => {
                error!("Failed to record turn {} as {}: {}", turn_id, status, error);
                self.waiters.lock().await.remove(turn_id);
                return;
            }
        };

        self.waiters.lock().await.remove(turn_id);

        if !settled {
            debug!(
                "Turn {} was already settled; not recording it as {}",
                turn_id, status
            );
            return;
        }

        // The session holds no running turn from this moment, so it is idle from
        // this moment — written here rather than after the terminal events below.
        //
        // The two writes are separate rows and cannot be one transaction, so
        // *something* is briefly observable between them; the question is only
        // which inconsistency a client can see. Settling the turn first and the
        // session last means a client that reads a terminal turn and then the
        // session sees a finished turn on a "running" session, which is the state
        // that makes the composer refuse the next message. The reverse — a session
        // already idle while its turn is a moment from terminal — costs nothing,
        // because idle is also what the session is between turns.
        //
        // The window used to be two statements wide and was merely improbable.
        // Closing open trajectories put a query and a per-trajectory emit inside
        // it, which is enough to fail an assertion that polls the turn and then
        // reads the session.
        let _ = self.store.update_session_status(session_id, "idle").await;

        // Close delegated trajectories the adapter left open, before the turn's
        // own terminal events. A subtree with no closing event is one the UI must
        // keep reporting as running, because nothing ever said otherwise — so an
        // interrupted turn would leave a permanently spinning subagent card.
        //
        // Ordering matters: these are emitted first so a client applying the
        // stream in order sees every trajectory resolved by the time the turn's
        // status arrives, rather than briefly holding a finished turn that still
        // contains running work.
        match self.store.open_trajectories(turn_id).await {
            Ok(open) => {
                for (stream_id, span, name) in open {
                    let reason = match &outcome {
                        Outcome::Interrupted => "interrupted",
                        Outcome::Completed { .. } => "abandoned",
                        _ => "failed",
                    };
                    let _ = self
                        .emit(
                            session_id,
                            turn_id,
                            StreamEvent {
                                phase: Some("completed".to_string()),
                                stream_id: Some(stream_id),
                                span,
                                name,
                                is_error: true,
                                source: "server".to_string(),
                                ..server_event(
                                    "subagent",
                                    String::new(),
                                    Some(serde_json::json!({"closedBy": "server",
                                                            "reason": reason})),
                                )
                            },
                        )
                        .await;
                }
            }
            Err(error) => {
                // Not fatal: the turn still settles. The cost is a card that reads
                // as running until the page is reloaded, so it is worth logging.
                error!(
                    "Could not close open trajectories for turn {}: {}",
                    turn_id, error
                );
            }
        }

        if !message.is_empty() {
            let _ = self
                .emit(
                    session_id,
                    turn_id,
                    StreamEvent {
                        is_error: true,
                        ..server_event(
                            "error",
                            message.clone(),
                            Some(serde_json::json!({"code": error_code})),
                        )
                    },
                )
                .await;
        }

        let _ = self
            .emit(
                session_id,
                turn_id,
                server_event(
                    "status",
                    String::new(),
                    Some(serde_json::json!({"state": status})),
                ),
            )
            .await;

        self.hub.close(turn_id).await;

        // The turn's rows have stopped changing, so this is the moment to move
        // them. Detached: the client already has its events, and making anything
        // wait on disk work here would be pure latency.
        self.consolidator.schedule(turn_id.to_string());
    }

    async fn emit(
        &self,
        session_id: &str,
        turn_id: &str,
        mut event: StreamEvent,
    ) -> crate::error::Result<()> {
        event.seq = self.seq.next(session_id).await?;

        if event.is_stable() {
            // Journaled before publishing, so a client reconnecting immediately
            // after seeing an event always finds it in replay.
            if let Err(error) = self
                .store
                .append_event(&event.to_row(session_id, turn_id))
                .await
            {
                error!("Failed to journal event seq {}: {}", event.seq, error);
            }
        }

        self.hub.publish(turn_id, event).await;
        Ok(())
    }
}

/// A server-authored event. These have no phase and are always stable.
fn server_event(kind: &str, text: String, data: Option<serde_json::Value>) -> StreamEvent {
    StreamEvent {
        seq: 0,
        kind: kind.to_string(),
        phase: None,
        stream_id: None,
        // The server speaks about the turn, not from inside a delegation.
        span: None,
        name: None,
        text,
        data,
        is_error: false,
        stability: "stable".to_string(),
        source: "server".to_string(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}
