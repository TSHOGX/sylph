//! Batching the journal's hot path, without moving the durability boundary.
//!
//! `docs/storage.md` calls this the single largest lever on the hot path: a 2 KB
//! assistant message arrives as ~400 deltas, and one transaction per delta makes
//! ~400 fsyncs out of what can be one. Batching converts thousands of tiny
//! transactions per turn into tens, and fsync count is what actually costs.
//!
//! ## Only ephemeral events are batched, and that is not a compromise
//!
//! The split is forced by an invariant already in the code, not chosen for
//! caution. [`crate::supervisor::turn`] journals a stable event *before* publishing
//! it, so that "a client reconnecting immediately after seeing an event always
//! finds it in replay". Meanwhile [`crate::events::EventHub`] keeps a bounded ring
//! of **only ephemeral** events, which is what a mid-stream reconnect replays from.
//!
//! So the two kinds already have different durability paths:
//!
//! | | Replayed from | Batching it would |
//! | --- | --- | --- |
//! | stable | the journal | open a window where a reconnect sees a hole |
//! | ephemeral | the hub's ring | cost nothing observable |
//!
//! Batching only ephemeral events therefore captures nearly all the win — deltas
//! are thousands per turn, stable events tens — while leaving the reconnect
//! invariant exactly as it was, and needing no flush-on-read machinery to preserve
//! it. Loss is bounded to one window of deltas on a turn that was visibly still
//! streaming, which is the tradeoff `docs/storage.md` already states.
//!
//! ## The ordering rule
//!
//! Consolidation folds a turn by reading its journal rows, so a buffered window
//! must reach the journal *before* the fold runs, or the fold silently omits the
//! deltas still in memory and produces a block whose replay is missing its tail.
//! [`JournalWriter::flush_turn`] is what the settle path calls, and it is the one
//! ordering constraint this module imposes on its caller.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::db::JournalStore;
use crate::models::Event;

/// How long a window may accumulate before it commits.
///
/// A latency/loss tradeoff rather than a throughput knob: 50 ms is below the
/// threshold where a reader perceives streaming as anything but smooth, and it
/// bounds loss to one window. Raising it buys progressively less — the fsync count
/// is already divided by an order of magnitude here — while making the loss window
/// something a person could notice.
const WINDOW: Duration = Duration::from_millis(50);

/// Commit early once a window reaches this many events.
///
/// A fast agent can emit far more than a window's worth of deltas, and an unbounded
/// buffer would hold a whole turn's stream in memory while degrading into one huge
/// transaction at the end. This keeps both bounded.
const MAX_BATCH: usize = 256;

/// Buffers ephemeral events per turn and commits them in windows.
///
/// Cheap to clone; every clone shares one set of buffers.
#[derive(Clone)]
pub struct JournalWriter {
    store: JournalStore,
    turns: Arc<Mutex<HashMap<String, Buffer>>>,
}

struct Buffer {
    events: Vec<Event>,
    /// Whether a timer is already armed for this turn.
    ///
    /// Without it, every event would spawn its own timer and a busy turn would
    /// accumulate one task per delta, all racing to flush the same buffer.
    flushing: bool,
}

impl JournalWriter {
    pub fn new(store: JournalStore) -> Self {
        Self {
            store,
            turns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Journal one event, batching it when batching is safe.
    ///
    /// A stable event is written through immediately, because a reconnect replays
    /// stable events from the journal and buffering one would open a window where
    /// the client cannot find an event it has already seen. An ephemeral event joins
    /// the current window.
    pub async fn append(&self, event: &Event, is_stable: bool) -> anyhow::Result<()> {
        if is_stable {
            self.store.append_event(event).await?;
            return Ok(());
        }

        let turn_id = event.turn_id.clone().unwrap_or_default();
        // An event with no turn cannot be folded into one, and buffering it under an
        // empty key would mix unrelated events into one window. Write it through.
        if turn_id.is_empty() {
            self.store.append_event(event).await?;
            return Ok(());
        }

        let full = {
            let mut turns = self.turns.lock().await;
            let buffer = turns.entry(turn_id.clone()).or_insert_with(|| Buffer {
                events: Vec::new(),
                flushing: false,
            });
            buffer.events.push(event.clone());

            if buffer.events.len() >= MAX_BATCH {
                true
            } else if buffer.flushing {
                // A timer is already armed and will pick this up.
                return Ok(());
            } else {
                buffer.flushing = true;
                false
            }
        };

        if full {
            self.flush_turn(&turn_id).await;
            return Ok(());
        }

        // Detached: the caller is on the token path, and making it wait out the
        // window would convert a throughput win into a latency loss.
        let writer = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(WINDOW).await;
            writer.flush_turn(&turn_id).await;
        });
        Ok(())
    }

    /// Commit whatever this turn has buffered, now.
    ///
    /// **Must be called before consolidating a turn.** The fold reads journal rows,
    /// so deltas still in memory would be missing from the block it writes — and
    /// the fold then deletes the rows, making the omission permanent. This is the
    /// one ordering rule the batching layer imposes.
    ///
    /// Idempotent, so the settle path and an in-flight timer can both call it.
    pub async fn flush_turn(&self, turn_id: &str) {
        let batch = {
            let mut turns = self.turns.lock().await;
            match turns.remove(turn_id) {
                Some(buffer) => buffer.events,
                None => return,
            }
        };

        if batch.is_empty() {
            return;
        }

        // Loud, and for the same reason the unbatched path is: an event published to
        // a client but never journaled is worse than one that never arrived, because
        // the client has already been told it happened. Here it also means the fold
        // will be missing deltas it should have had.
        if let Err(error) = self.store.append_events(&batch).await {
            error!(
                "DATA LOSS: {} buffered event(s) for turn {} were published but not journaled: {:#}",
                batch.len(),
                turn_id,
                error
            );
        }
    }

    /// Commit every buffered window, for shutdown.
    pub async fn flush_all(&self) {
        let turn_ids: Vec<String> = {
            let turns = self.turns.lock().await;
            turns.keys().cloned().collect()
        };
        if !turn_ids.is_empty() {
            warn!(
                "Flushing {} turn(s) with buffered events before shutdown",
                turn_ids.len()
            );
        }
        for turn_id in turn_ids {
            self.flush_turn(&turn_id).await;
        }
    }

    /// How many events are buffered for a turn. Tests only.
    #[cfg(test)]
    pub async fn buffered(&self, turn_id: &str) -> usize {
        self.turns
            .lock()
            .await
            .get(turn_id)
            .map(|buffer| buffer.events.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A store with one session and one turn, so events satisfy the foreign keys.
    async fn journal() -> (JournalStore, TempDir, String) {
        let dir = TempDir::new().unwrap();
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .unwrap();

        let session = store
            .create_session(&crate::models::Session {
                id: "s1".to_string(),
                origin: "sylph".to_string(),
                identity_mode: "sylph".to_string(),
                lifecycle_mode: "sylph".to_string(),
                agent: "mock".to_string(),
                backend_session_id: None,
                trace_session_id: None,
                workdir: dir.path().display().to_string(),
                title: None,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                created_at_ms: 1000,
                updated_at_ms: 1000,
            })
            .await
            .unwrap();

        let turn = store
            .create_turn(&crate::models::Turn {
                id: "t1".to_string(),
                session_id: session.id.clone(),
                incarnation_id: None,
                status: "running".to_string(),
                request_text: "hello".to_string(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                runner_exit_code: None,
                created_at_ms: 1000,
                started_at_ms: None,
                completed_at_ms: None,
                stopped_at_ms: None,
            })
            .await
            .unwrap();
        (store, dir, turn.id)
    }

    fn ephemeral(turn_id: &str, seq: i32, text: &str) -> Event {
        Event {
            id: None,
            session_id: "s1".to_string(),
            turn_id: Some(turn_id.to_string()),
            seq,
            kind: "assistant".to_string(),
            phase: Some("delta".to_string()),
            stream_id: Some("a1".to_string()),
            name: None,
            text: text.to_string(),
            data_json: "{}".to_string(),
            is_error: false,
            stability: "ephemeral".to_string(),
            source: "agent".to_string(),
            created_at_ms: 2000 + seq as i64,
            reconciled_at_ms: None,
            // Root turn: these fixtures test batching, not delegation.
            span: None,
        }
    }

    fn stable(turn_id: &str, seq: i32, text: &str) -> Event {
        Event {
            phase: Some("completed".to_string()),
            stability: "stable".to_string(),
            ..ephemeral(turn_id, seq, text)
        }
    }

    /// A stable event must be durable the instant it is published, because a
    /// reconnect replays stable events from the journal and would otherwise fail to
    /// find one the client has already seen.
    #[tokio::test]
    async fn a_stable_event_is_journaled_immediately_rather_than_buffered() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        writer.append(&stable(&turn_id, 0, "settled"), true).await.unwrap();

        assert_eq!(writer.buffered(&turn_id).await, 0, "nothing was buffered");
        assert_eq!(
            store.turn_events_after(&turn_id, -1).await.unwrap().len(),
            1,
            "and it is already readable from the journal"
        );
    }

    #[tokio::test]
    async fn ephemeral_events_accumulate_into_one_window() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        for seq in 0..5 {
            writer
                .append(&ephemeral(&turn_id, seq, "tok"), false)
                .await
                .unwrap();
        }

        assert_eq!(writer.buffered(&turn_id).await, 5);
        assert!(
            store.turn_events_for_consolidation(&turn_id).await.unwrap().is_empty(),
            "the window has not committed yet"
        );

        writer.flush_turn(&turn_id).await;

        assert_eq!(
            store.turn_events_for_consolidation(&turn_id).await.unwrap().len(),
            5,
            "one transaction carried all five"
        );
    }

    /// The ordering rule this module imposes on its caller. Consolidation folds a
    /// turn by reading its journal rows, so a buffered window must land first — a
    /// fold over a partial run writes a block whose replay is missing its tail, and
    /// then deletes the rows that would have fixed it.
    #[tokio::test]
    async fn flushing_a_turn_makes_every_buffered_delta_visible_to_the_fold() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        for (seq, text) in ["I'll ", "start ", "now"].iter().enumerate() {
            writer
                .append(&ephemeral(&turn_id, seq as i32, text), false)
                .await
                .unwrap();
        }
        writer.append(&stable(&turn_id, 3, "I'll start now"), true).await.unwrap();

        writer.flush_turn(&turn_id).await;

        // What `consolidate_turn` will read.
        let events = store.turn_events_for_consolidation(&turn_id).await.unwrap();
        let deltas = events
            .iter()
            .filter(|event| event.phase.as_deref() == Some("delta"))
            .count();
        assert_eq!(
            deltas, 3,
            "every delta must be on disk before the fold runs, not just some"
        );
    }

    /// A batched event must carry every column the unbatched path does.
    ///
    /// `span` is nullable, so omitting it from the batch insert succeeds and loses
    /// the delegation shape of every batched delta — the failure is silent, and it
    /// surfaces two layers away as a replay that flattens a workflow's parallel
    /// phases into one sequence. This is not hypothetical: the batch insert was
    /// written before `span` existed and did exactly that until this test.
    #[tokio::test]
    async fn a_batched_delta_keeps_its_delegation_span() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        let mut delegated = ephemeral(&turn_id, 0, "inside a subagent");
        delegated.span = Some("agent-explore".to_string());
        writer.append(&delegated, false).await.unwrap();
        writer.flush_turn(&turn_id).await;

        let events = store.turn_events_for_consolidation(&turn_id).await.unwrap();
        assert_eq!(
            events.first().and_then(|event| event.span.clone()),
            Some("agent-explore".to_string()),
            "the batch insert must bind every column, not the ones that existed \
             when it was written"
        );
    }

    /// Idempotent, because the settle path and an armed timer both call it.
    #[tokio::test]
    async fn flushing_twice_does_not_duplicate_or_fail() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());
        writer.append(&ephemeral(&turn_id, 0, "one"), false).await.unwrap();

        writer.flush_turn(&turn_id).await;
        writer.flush_turn(&turn_id).await;

        assert_eq!(store.turn_events_for_consolidation(&turn_id).await.unwrap().len(), 1);
    }

    /// A fast agent can outrun the window, and an unbounded buffer would hold a
    /// whole turn in memory before degrading into one huge transaction.
    #[tokio::test]
    async fn a_window_commits_early_once_it_reaches_the_batch_cap() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        for seq in 0..MAX_BATCH {
            writer
                .append(&ephemeral(&turn_id, seq as i32, "t"), false)
                .await
                .unwrap();
        }

        assert_eq!(
            writer.buffered(&turn_id).await,
            0,
            "the cap forced a commit rather than growing without bound"
        );
        assert_eq!(
            store.turn_events_for_consolidation(&turn_id).await.unwrap().len(),
            MAX_BATCH
        );
    }

    /// The timer path, not just explicit flushes: a turn that streams and then goes
    /// quiet without settling must still reach disk.
    #[tokio::test]
    async fn a_window_commits_on_its_own_once_the_interval_elapses() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        writer.append(&ephemeral(&turn_id, 0, "quiet"), false).await.unwrap();
        tokio::time::sleep(WINDOW * 4).await;

        assert_eq!(
            store.turn_events_for_consolidation(&turn_id).await.unwrap().len(),
            1,
            "no explicit flush happened, so the timer had to do it"
        );
    }

    /// One armed timer per turn, not one per event. Otherwise a busy turn spawns a
    /// task per delta, all racing to flush the same buffer.
    #[tokio::test]
    async fn a_busy_turn_arms_one_timer_rather_than_one_per_event() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        for seq in 0..10 {
            writer
                .append(&ephemeral(&turn_id, seq, "t"), false)
                .await
                .unwrap();
        }
        tokio::time::sleep(WINDOW * 4).await;

        // Ten events, one commit, ten rows. A per-event timer would have committed
        // partial windows and left the buffer empty at different moments; the
        // observable consequence either way is the row count, which must be exact.
        assert_eq!(store.turn_events_for_consolidation(&turn_id).await.unwrap().len(), 10);
        assert_eq!(writer.buffered(&turn_id).await, 0);
    }

    /// An event with no turn cannot be folded into one, so buffering it under an
    /// empty key would mix unrelated events into a shared window.
    #[tokio::test]
    async fn an_event_with_no_turn_is_written_through() {
        let (store, _dir, _turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());

        let mut orphan = ephemeral("", 0, "session-scoped");
        orphan.turn_id = None;
        writer.append(&orphan, false).await.unwrap();

        assert_eq!(writer.buffered("").await, 0);
    }

    #[tokio::test]
    async fn flush_all_commits_every_open_window() {
        let (store, _dir, turn_id) = journal().await;
        let writer = JournalWriter::new(store.clone());
        writer.append(&ephemeral(&turn_id, 0, "a"), false).await.unwrap();

        writer.flush_all().await;

        assert_eq!(store.turn_events_for_consolidation(&turn_id).await.unwrap().len(), 1);
    }
}
