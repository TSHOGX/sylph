//! Moving settled turns out of the journal, and the two moments that happen at.
//!
//! Consolidation is what makes the tiered design real: without it `journal.db`
//! grows unbounded and the WAL-checkpoint hazard `docs/storage.md` describes stays
//! live. The work itself is [`history::consolidate_turn`]; this module decides
//! *when*.
//!
//! Two triggers, because there are two ways a turn ends up settled:
//!
//! * **After a turn finishes**, since that is when its rows stop changing and the
//!   data is freshest in the page cache.
//! * **At startup**, for anything a crash or a restart left behind. A turn that
//!   settled while the server was down has no live task to notice it.
//!
//! Both run detached. A consolidation failure must never fail a turn: the journal
//! still holds the events, so the cost of deferring is disk rather than data.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use crate::db::LiveStore;
use crate::storage::history::{self, HistoryStore};

/// How many turns the startup sweep consolidates per pass.
///
/// Bounded because a journal holding thousands of unconsolidated turns — a server
/// that ran for months without this wired up — should not spend minutes on I/O
/// before it starts serving. It catches up over subsequent passes.
const SWEEP_BATCH: i64 = 64;

/// How long to wait between sweep passes.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Consolidates settled turns into history.
#[derive(Clone)]
pub struct Consolidator {
    journal: LiveStore,
    history: HistoryStore,
    /// One consolidation at a time.
    ///
    /// Not for correctness — `put_blocks` is idempotent and each turn is
    /// independent — but because consolidation is I/O-heavy and competing with the
    /// token path for the same disk is exactly what the tiering exists to avoid.
    /// Serializing it keeps the background work off the latency-sensitive path.
    gate: Arc<Semaphore>,
}

impl Consolidator {
    pub fn new(journal: LiveStore, history: HistoryStore) -> Self {
        Self {
            journal,
            history,
            gate: Arc::new(Semaphore::new(1)),
        }
    }

    /// Consolidate one turn, in the background.
    ///
    /// Called when a turn settles. Detached and never awaited by the caller: the
    /// turn is already complete and the client already has its events, so making
    /// the response wait on disk work would be pure latency.
    pub fn schedule(&self, turn_id: String) {
        let consolidator = self.clone();
        tokio::spawn(async move {
            let _permit = match consolidator.gate.acquire().await {
                Ok(permit) => permit,
                // The semaphore is never closed, so this cannot happen; if it
                // somehow did, the startup sweep is the safety net.
                Err(_) => return,
            };

            match history::consolidate_turn(&consolidator.journal, &consolidator.history, &turn_id)
                .await
            {
                Ok(0) => debug!("Turn {} had nothing to consolidate", turn_id),
                Ok(blocks) => debug!("Turn {} consolidated into {} blocks", turn_id, blocks),
                // Deliberately not fatal and not retried here. The rows are still
                // in the journal, so the next sweep picks it up — deferring costs
                // disk, while failing the turn would cost the user their answer.
                Err(error) => warn!(
                    "Deferring consolidation of turn {} to the next sweep: {:#}",
                    turn_id, error
                ),
            }
        });
    }

    /// Consolidate everything settled that is still in the journal.
    ///
    /// Returns how many turns moved. Run at startup and periodically after.
    pub async fn sweep(&self) -> usize {
        let _permit = match self.gate.acquire().await {
            Ok(permit) => permit,
            Err(_) => return 0,
        };

        let turns = match self.journal.consolidatable_turns(SWEEP_BATCH).await {
            Ok(turns) => turns,
            Err(error) => {
                error!("Could not list turns awaiting consolidation: {}", error);
                return 0;
            }
        };

        let mut moved = 0;
        for turn_id in turns {
            match history::consolidate_turn(&self.journal, &self.history, &turn_id).await {
                Ok(0) => {}
                Ok(_) => moved += 1,
                // One bad turn must not stop the sweep, or a single unparseable
                // row would block every turn behind it forever.
                Err(error) => warn!("Could not consolidate turn {}: {:#}", turn_id, error),
            }
        }

        if moved > 0 {
            info!("Consolidated {} settled turn(s) into history", moved);
        }
        moved
    }

    /// Sweep now, then keep sweeping.
    ///
    /// The periodic pass is a safety net rather than the main path: a turn that
    /// settles normally is consolidated by [`Self::schedule`] within moments. This
    /// catches what a crash, a failed attempt, or a restart left behind.
    pub fn spawn_sweeper(&self) {
        let consolidator = self.clone();
        tokio::spawn(async move {
            // Immediately, because a restart is exactly when the journal is most
            // likely to hold settled turns nobody is watching.
            consolidator.sweep().await;

            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            // The first tick fires instantly and the sweep above already covered
            // it, so it is consumed rather than acted on.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                consolidator.sweep().await;
            }
        });
    }

    pub fn history(&self) -> &HistoryStore {
        &self.history
    }
}
