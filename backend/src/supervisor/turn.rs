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
}

impl LinkedSupervisor {
    pub fn new(store: LiveStore, hub: EventHub, links: LinkManager, seq: SeqAllocator) -> Self {
        Self {
            store,
            hub,
            links,
            seq,
            waiters: Default::default(),
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

        // Flags are applied to the live session rather than passed with the turn:
        // a link outlives one turn, so a per-turn flag would silently persist
        // into the next. Applying it explicitly makes the change visible in the
        // capability stream.
        for (key, value) in &flags {
            if let Err(error) = link.set(key, serde_json::Value::String(value.clone())).await {
                // Not fatal: an agent may refuse a flag it no longer supports,
                // and losing the turn over that is worse than running it with
                // the previous value.
                warn!("Agent refused to set {}={}: {:#}", key, value, error);
            }
        }

        link.send(prompt, attachments, command, command_args)
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
                    self.links.detach(&session_id, "stopped").await;
                }
            }
            None => {
                // No link, but a turn still marked running: settle it rather than
                // leaving the row inconsistent with reality.
                debug!("Stop for turn {} found no link", turn_id);
            }
        }

        // Settled here rather than waiting for a `turn` event, because an
        // interrupted agent may not emit one, and a detached link certainly will
        // not. The waiter is removed by `settle`, so a later event is a no-op.
        self.settle(&session_id, turn_id, Outcome::Interrupted).await;
        Ok(true)
    }

    /// Record a turn's terminal state and close its stream.
    async fn settle(&self, session_id: &str, turn_id: &str, outcome: Outcome) {
        self.waiters.lock().await.remove(turn_id);

        let (status, error_code, message) = outcome.as_status();

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

        if let Err(error) = self
            .store
            .finish_turn(turn_id, status, None, error_code, Some(&message))
            .await
        {
            error!("Failed to record turn {} as {}: {}", turn_id, status, error);
        }
        let _ = self.store.update_session_status(session_id, "idle").await;

        self.hub.close(turn_id).await;
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
        name: None,
        text,
        data,
        is_error: false,
        stability: "stable".to_string(),
        source: "server".to_string(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}
