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
    /// Returns whether this call is the one that settled it. `begin_settling` guards on
    /// the status, so a second attempt is a no-op and reports `false` — which is what
    /// makes this safe to reach from both a waiter's outcome and an agent's own
    /// `turn`/`completed` without either having to know about the other.
    ///
    /// # The turn goes terminal last, and that is the whole shape of this function
    ///
    /// Settling is two writes with the sweep between them: the outcome is claimed under a
    /// non-terminal `settling` status, the record is finished, and only then does the status
    /// become terminal. So **a client that sees a terminal turn is looking at a complete
    /// record**, and needs no timer, no grace period, and no second signal to know it.
    ///
    /// It used to be one write, first — and `status` was therefore answering two questions
    /// at once, *what did this turn produce* and *is its record finished*, with no field
    /// that could separate them. Every reader in the window between that write and the
    /// sweep saw a terminal turn over a record still being written, and had no way to tell
    /// that was what it was looking at. Measured on the wire: the frontend ended its claim
    /// on the turn as soon as the row read terminal, so it unmounted the stream before the
    /// sweep's own `subagent`/`completed` events arrived, and its history refetch fired on
    /// the same early signal and read a journal the sweep had not written yet. A delegation
    /// the server had closed rendered as permanently running. At natural speed the sweep
    /// usually won, so the suite passed with the bug present and failed roughly one run in
    /// three — a *different* spec each time, which reads as flakiness rather than as one
    /// shared cause.
    ///
    /// The fix is here rather than in the client because the client was not the problem.
    /// Keying it on the stream's announcement with a bounded grace period was the
    /// alternative, and it leaves the server publishing a state no client can name, so
    /// every future reader has to re-derive the same timer.
    pub async fn close(&self, session_id: &sylph_protocol::SessionId, turn_id: &str, outcome: Outcome) -> bool {
        let (status, error_code, message) = outcome.as_status();

        let claimed = match self
            .store
            .begin_settling(turn_id, error_code, Some(&message))
            .await
        {
            Ok(claimed) => claimed,
            Err(error) => {
                error!("Failed to record turn {} as {}: {}", turn_id, status, error);
                return false;
            }
        };

        if !claimed {
            debug!(
                "Turn {} was already settled; not recording it as {}",
                turn_id, status
            );
            return false;
        }

        // The session holds no running turn from this moment, so it is idle from this
        // moment — written inside the settle, before the turn's status goes terminal.
        //
        // The two are separate rows and cannot be one transaction, so *something* is
        // briefly observable between them; the question is only which inconsistency a
        // client can see. Writing the session first means a client that reads a terminal
        // turn and then the session always sees `idle`, which is the pair the composer
        // needs to accept the next message — a finished turn on a "running" session is
        // what makes it refuse.
        //
        // Two-phase settling makes that ordering free rather than a trade. The window
        // between these two writes used to be the whole sweep wide, so an assertion that
        // polled the turn and then read the session could catch it; now the turn does not
        // read terminal until the sweep has finished, and the interleaving cannot be
        // observed at all. `a_settled_turn_is_never_visible_on_a_running_session` in
        // `tests/integration.rs` still drives many turns to pin it.
        //
        // What a client *can* see here is a `settling` turn on an idle session. That is
        // accurate — the agent is done and the record is closing — and no client branches
        // on the pair, because `settling` is not terminal.
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
        //
        // Before the promotion, not after, and that is the second half of what makes a
        // terminal row trustworthy. A history read takes the journal tier whenever it has
        // rows, so a client refetching on the terminal status would find the sweep's closes
        // — appended directly — and *not* the deltas still sitting in the writer. Flushing
        // first means the tier it reads is complete, not merely non-empty.
        self.writer.flush_turn(turn_id).await;

        // The record is finished, so the turn may now say so. Everything a client can reach
        // by reading this turn is already written: the session is idle, its open
        // trajectories are closed and journaled, its error frame is out, and its buffered
        // deltas are down.
        //
        // A failure here is loud. The turn is stuck `settling` until the next restart sweeps
        // it, which is a card that reads as closing forever — worse than a wrong outcome,
        // because nothing else in the system will mention it.
        match self.store.promote_settled_turn(turn_id, status).await {
            Ok(true) => {}
            Ok(false) => error!(
                "Turn {} left settling: it was no longer claimed at promotion, so {} was \
                 never recorded",
                turn_id, status
            ),
            Err(error) => error!(
                "Turn {} left settling: promoting it to {} failed: {}",
                turn_id, status, error
            ),
        }

        // Last, so the frame announcing the outcome cannot arrive before the row carrying
        // it. A client that refetches on this frame — which is what it is for — then reads
        // a turn that is already terminal, rather than one still settling that it would
        // re-adopt as live.
        //
        // Its `text` is deliberately empty: the state travels in `data.state`, and the row
        // is the authority. That is also why its absence from a history read costs nothing,
        // which is what lets the promotion go first.
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
    ///
    /// That ordering covers a client reading the *stream*. It is not by itself enough for
    /// one reading the *record*, which is why [`Self::close`] runs this between the claim
    /// and the promotion: a client that learns the turn ended by polling the row never saw
    /// these frames at all, and its only repair is a history refetch. Emitting them early
    /// in stream order while the row already read terminal is exactly the state that let a
    /// refetch miss them.
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
