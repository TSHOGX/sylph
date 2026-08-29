//! Recording a turn's terminal state, for turns nobody is waiting on.
//!
//! A user's turn settles through [`crate::supervisor::LinkedSupervisor`]: a POST parks on a waiter, the
//! agent's `turn`/`completed` arrives, and the waiter is woken with the outcome. An
//! *agent-initiated* turn has no waiter, because nobody asked for it — so the same
//! work has to be reachable from the link manager, which is where its events land.
//!
//! Extracted rather than duplicated. This is the code that closes open trajectories,
//! marks the session idle, and hands the turn to consolidation; a second copy would
//! be a second place for those to diverge, and the ordering constraints between them
//! are subtle enough that the divergence would not be obvious. The supervisor keeps
//! only what is genuinely its own — claiming the waiter before settling, so a stop
//! and a completion cannot both be reported.
//!
//! Holds no reference to either manager, which is what lets both hold one of these:
//! `LinkedSupervisor` owns a `LinkManager`, so a direct call back the other way would
//! be a cycle.

use tracing::{debug, error};

use crate::db::JournalStore;
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::storage::Consolidator;
use crate::supervisor::turn::{server_event, Outcome};

#[derive(Clone)]
pub struct TurnCloser {
    store: JournalStore,
    hub: EventHub,
    seq: SeqAllocator,
    consolidator: Consolidator,
    /// Held only to flush a turn's buffered deltas before the fold reads them.
    writer: crate::db::JournalWriter,
}

impl TurnCloser {
    pub fn new(
        store: JournalStore,
        hub: EventHub,
        seq: SeqAllocator,
        consolidator: Consolidator,
        writer: crate::db::JournalWriter,
    ) -> Self {
        Self { store, hub, seq, consolidator, writer }
    }

    /// Record a turn as terminal, resolve what it left open, and hand it to history.
    ///
    /// Returns whether this call is the one that settled it. `finish_turn` guards on
    /// the status, so a second attempt is a no-op and reports `false` — which is what
    /// makes this safe to reach from both a waiter's outcome and an agent's own
    /// `turn`/`completed` without either having to know about the other.
    pub async fn close(&self, session_id: &sylph_protocol::SessionId, turn_id: &str, outcome: Outcome) -> bool {
        let (status, error_code, message) = outcome.as_status();

        let settled = match self
            .store
            .finish_turn(turn_id, status, error_code, Some(&message))
            .await
        {
            Ok(settled) => settled,
            Err(error) => {
                error!("Failed to record turn {} as {}: {}", turn_id, status, error);
                return false;
            }
        };

        if !settled {
            debug!(
                "Turn {} was already settled; not recording it as {}",
                turn_id, status
            );
            return false;
        }

        // The session holds no running turn from this moment, so it is idle from this
        // moment — written before the terminal events below, not after.
        //
        // The two writes are separate rows and cannot be one transaction, so
        // *something* is briefly observable between them; the question is only which
        // inconsistency a client can see. Settling the turn first and the session
        // last means a client that reads a terminal turn and then the session sees a
        // finished turn on a "running" session, which is the state that makes the
        // composer refuse the next message. The reverse costs nothing, because idle
        // is also what the session is between turns.
        //
        // The window used to be two statements wide and was merely improbable.
        // Closing open trajectories put a query and a per-trajectory emit inside it,
        // which is enough to fail an assertion that polls the turn and then reads the
        // session.
        let _ = self.store.update_session_status(session_id, "idle").await;

        self.close_open_trajectories(session_id, turn_id, &outcome).await;

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

        // Awaited, and it must stay awaited. Consolidation folds this turn by
        // reading its journal rows, so any window still buffered in the writer has to
        // land first — the fold would otherwise write a block missing its final
        // deltas and then delete the rows, making the omission permanent. This is the
        // one ordering rule `crate::db::writer` imposes, and the only place the
        // batching layer is visible outside itself.
        //
        // Guarded by `replay_reproduces_the_deltas_and_not_only_the_final_text` in
        // `tests/storage.rs`, which fails when this line is removed because the
        // replay then carries only the settled text.
        self.writer.flush_turn(turn_id).await;

        // The turn's rows have stopped changing, so this is the moment to move them.
        // Detached: the client already has its events, and making anything wait on
        // disk work here would be pure latency.
        self.consolidator.schedule(turn_id.to_string());
        true
    }

    /// Close delegated trajectories the adapter left open.
    ///
    /// A subtree with no closing event is one the UI must keep reporting as running,
    /// because nothing ever said otherwise — so an interrupted turn would leave a
    /// permanently spinning subagent card.
    ///
    /// Emitted before the turn's own terminal events, so a client applying the stream
    /// in order sees every trajectory resolved by the time the turn's status arrives,
    /// rather than briefly holding a finished turn that still contains running work.
    async fn close_open_trajectories(
        &self,
        session_id: &sylph_protocol::SessionId,
        turn_id: &str,
        outcome: &Outcome,
    ) {
        match self.store.open_trajectories(turn_id).await {
            Ok(open) => {
                for (stream_id, span, name) in open {
                    let reason = match outcome {
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
                // Not fatal: the turn still settles. The cost is a card that reads as
                // running until the page is reloaded, so it is worth logging.
                error!(
                    "Could not close open trajectories for turn {}: {}",
                    turn_id, error
                );
            }
        }
    }

    async fn emit(
        &self,
        session_id: &sylph_protocol::SessionId,
        turn_id: &str,
        mut event: StreamEvent,
    ) -> crate::error::Result<()> {
        event.seq = self.seq.next(session_id).await?;

        if event.is_stable() {
            // Journaled before publishing, so a client reconnecting immediately after
            // seeing an event always finds it in replay.
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
