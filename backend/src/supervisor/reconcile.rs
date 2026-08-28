//! Recording a delegation's outcome on the turn that owns it.
//!
//! A trajectory id is **session-scoped, not turn-scoped**. Backgrounded work is the
//! case that forces the distinction: a delegation opened on turn N reports its
//! outcome on turn N+k, because an agent that dispatched work into the background
//! finishes its turn and then opens one of its own to say what happened
//! (`docs/session-protocol.md` § An agent may start a turn).
//!
//! Nothing addressed that. The late close was journaled against whichever turn was
//! running when it arrived, so it could not reach the node it closed — measured, it
//! rendered as a second, parentless card while turn N's own record still said the
//! work was unfinished. Two records about one delegation, disagreeing, and the wrong
//! one first.
//!
//! **Why amend in place rather than move the event.** Three shapes were considered:
//!
//! - *Journal the late close under turn N.* Rejected: it breaks the two-tier read.
//!   `api::sessions::history` returns a turn's journal rows when there are any and
//!   its history blocks otherwise, so adding one row to an already-consolidated turn
//!   replaces its entire transcript with that single row.
//! - *Suppress the late close and re-emit it under turn N.* Rejected for the same
//!   reason, and it also discards the arrival-order fact — the outcome genuinely
//!   arrived during the later turn, and a client watching that turn should see it.
//! - *Amend turn N's own record, and let the late close stand where it landed.*
//!   Chosen. Turn N's row is a claim about state, not an event that happened, and
//!   `abandoned` is documented as non-final precisely so it can be corrected. The two
//!   records then agree instead of contradicting, which is the actual defect.
//!
//! The amendment is an `UPDATE` of one row in whichever tier holds it, so it adds no
//! rows and the two-tier read is untouched. Both tiers are attempted unconditionally:
//! consolidation can move the row between them at any moment, and checking which tier
//! holds it first is a race, while writing to both is not.

use tracing::{debug, error};

use crate::db::JournalStore;
use crate::storage::HistoryStore;

/// A trajectory's real outcome, as some later turn reported it.
///
/// One type for both tiers so the rule about *what may be overwritten* has a single
/// derivation. Two copies of that predicate would be two places to grow a case, and
/// the tiers would then disagree about the same delegation depending only on whether
/// consolidation had run — the least debuggable possible split.
pub struct TrajectoryOutcome<'a> {
    /// What the delegate said about itself.
    pub text: &'a str,
    /// Whether it reported a failure. A real outcome may be a failure, and recording
    /// it as one is the point: `abandoned` means nobody reported, not that nobody
    /// could have.
    pub is_error: bool,
    /// The turn the outcome arrived on, kept as provenance so the corrected row still
    /// says where the report came from.
    pub reported_on_turn_id: &'a str,
}

impl TrajectoryOutcome<'_> {
    /// Whether this outcome may overwrite the record described by `phase` and `data`.
    ///
    /// Exactly two records are provisional, and nothing else may be touched:
    ///
    /// - **`started`** — declared and never closed. The turn-settle sweep left it
    ///   alone because the adapter said it was backgrounded, so consolidation kept it
    ///   open and a client reads it as running. It is running, and this is the report
    ///   it was waiting for.
    /// - **`completed` closed by the server** — the sweep's own close, `abandoned`.
    ///   Provisional by construction: it records that nobody had reported an outcome,
    ///   which a later report falsifies.
    ///
    /// An outcome the *agent* already stated is never overwritten. A delegate that
    /// closed cleanly on its own turn and is mentioned again later is being described,
    /// not re-decided, and letting a mention rewrite a settled outcome would make the
    /// last word win — which for a summary that quotes an earlier failure inverts it.
    pub fn supersedes(&self, phase: Option<&str>, data: Option<&serde_json::Value>) -> bool {
        match phase {
            Some("started") | Some("delta") => true,
            Some("completed") => data
                .and_then(|d| d.get("closedBy"))
                .and_then(|v| v.as_str())
                == Some("server"),
            _ => false,
        }
    }

    /// Rewrite a provisional record into this outcome.
    ///
    /// `source` becomes `agent`, because the statement is now the agent's rather than
    /// the server's — and because the frontend reads `closedBy: "server"` as the sole
    /// signal for `abandoned`, so leaving it in place would keep rendering "never
    /// finished" over a delegation that demonstrably finished. The provenance is kept
    /// under different keys instead: the correction stays auditable without claiming
    /// the server is the one making the claim.
    pub fn apply(
        &self,
        phase: &mut Option<String>,
        text: &mut String,
        is_error: &mut bool,
        data: &mut Option<serde_json::Value>,
        source: &mut String,
    ) {
        *phase = Some("completed".to_string());
        *text = self.text.to_string();
        *is_error = self.is_error;
        *source = "agent".to_string();

        // Merged into whatever was there rather than replacing it, so a progress
        // snapshot and `is_backgrounded` survive — the latter is the only durable
        // record that this delegation was *expected* to outlive its turn.
        let mut merged = match data.take() {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        merged.remove("closedBy");
        merged.remove("reason");
        merged.insert("reconciled".to_string(), serde_json::Value::Bool(true));
        merged.insert(
            "reportedOnTurnId".to_string(),
            serde_json::Value::String(self.reported_on_turn_id.to_string()),
        );
        *data = Some(serde_json::Value::Object(merged));
    }
}

/// Applies a late outcome to whichever tier still holds the trajectory.
///
/// Holds both stores because a trajectory's record crosses them on consolidation's
/// schedule, which no caller can observe. Handed to the link manager at startup the
/// same way [`crate::supervisor::TurnCloser`] is, and for the same reason: the drain
/// task is where late events land, and it cannot reach the storage tier otherwise.
#[derive(Clone)]
pub struct TrajectoryReconciler {
    journal: JournalStore,
    history: HistoryStore,
}

impl TrajectoryReconciler {
    pub fn new(journal: JournalStore, history: HistoryStore) -> Self {
        Self { journal, history }
    }

    /// Record `outcome` for `stream_id` on whatever earlier turn owns it.
    ///
    /// Returns whether anything was amended. `false` is the ordinary case and not a
    /// failure: almost every trajectory closes on the turn that opened it, and there
    /// is then no earlier record to correct.
    pub async fn record(
        &self,
        session_id: &sylph_protocol::SessionId,
        stream_id: &str,
        arriving_turn_id: &str,
        outcome: &TrajectoryOutcome<'_>,
    ) -> bool {
        let mut amended = false;

        match self
            .journal
            .reconcile_trajectory(session_id, stream_id, arriving_turn_id, outcome)
            .await
        {
            Ok(true) => amended = true,
            Ok(false) => {}
            Err(error) => error!(
                "Could not reconcile trajectory {} in the journal: {}",
                stream_id, error
            ),
        }

        match self
            .history
            // `history.db` keys on the encoded triple, so the encoding happens here at
            // the boundary between the two stores.
            .reconcile_trajectory(&session_id.encode(), stream_id, arriving_turn_id, outcome)
            .await
        {
            Ok(true) => amended = true,
            Ok(false) => {}
            Err(error) => error!(
                "Could not reconcile trajectory {} in history: {}",
                stream_id, error
            ),
        }

        if amended {
            // Logged because the whole point is that a record changed after the turn
            // holding it had settled. Without a line here, the only evidence that
            // reconciliation ran at all is the absence of a contradiction — and a
            // subsystem whose success looks like nothing having happened cannot be
            // told from one that never ran.
            debug!(
                "Trajectory {} on session {} was reported finished by turn {}",
                stream_id, session_id, arriving_turn_id
            );
        }

        amended
    }
}
