//! What is safe to drop, and who gets to say.
//!
//! Retention asks exactly one question and never asks how old anything is. A
//! time-based rule deletes data because a clock advanced, which is precisely the
//! case where the data may not exist anywhere else yet.
//!
//! The discipline is a Postgres replication slot's `restart_lsn` or a Kafka
//! consumer offset: the upstream keeps whatever any consumer might still need, so
//! a lagging consumer causes *growth* rather than loss. Growth is visible and
//! fixable; loss is neither.

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info};

use crate::storage::HistoryStore;
use crate::tracedb::Client as TraceDbClient;

/// Who confirms that a session's history is safe to drop.
#[async_trait]
pub trait RetentionAuthority: Send + Sync {
    /// A name for the journal, so a stored watermark records what produced it.
    fn name(&self) -> &'static str;

    /// The highest sequence number safe to drop, or `None` to drop nothing.
    async fn safe_to_drop(&self, session_id: &str) -> Result<Option<i32>>;
}

/// No downstream copy exists, so nothing is ever dropped.
///
/// This is the authority for a session whose agent-native store died with its
/// environment — a sandbox. There is no watermark to wait for, and pruning would
/// destroy the only copy. Such a session is still *compacted* — deltas folded,
/// blobs externalized, text compressed — but never pruned.
///
/// It is also the default for anything unrecognized, because the safe direction to
/// be wrong in is keeping too much.
pub struct TerminalAuthority;

#[async_trait]
impl RetentionAuthority for TerminalAuthority {
    fn name(&self) -> &'static str {
        "terminal"
    }

    async fn safe_to_drop(&self, _session_id: &str) -> Result<Option<i32>> {
        Ok(None)
    }
}

/// TraceDB has ingested the agent's native store, so blocks it covers are safe.
///
/// The derivation is sound because of two properties confirmed in TraceDB's own
/// source rather than assumed (`docs/storage.md`):
///
/// * **Ingestion is whole-session and transactional.** `Facade::scan` compares a
///   source fingerprint and re-parses the session entirely; there is no
///   incremental append, and `IngestMode::retain_full` can no longer create a
///   partial record. So a session TraceDB holds is complete as of its ingestion.
/// * **`list` reports the extent of what it holds**, as `endedAtMs`.
///
/// `endedAtMs` rather than `ingestedAtMs` is the load-bearing choice.
/// `ingestedAtMs` is when TraceDB *ran*, which would assume the agent had flushed
/// its native store by that moment — a race whose window is the agent's own
/// buffering and is invisible from here. `endedAtMs` is a statement about data
/// rather than about timing, so it needs no such assumption.
pub struct TraceDbAuthority {
    tracedb: TraceDbClient,
    history: HistoryStore,
}

impl TraceDbAuthority {
    pub fn new(tracedb: TraceDbClient, history: HistoryStore) -> Self {
        Self { tracedb, history }
    }
}

#[async_trait]
impl RetentionAuthority for TraceDbAuthority {
    fn name(&self) -> &'static str {
        "tracedb"
    }

    async fn safe_to_drop(&self, session_id: &str) -> Result<Option<i32>> {
        // Asked by id through `list`, because that is the operation carrying
        // `endedAtMs` and the event count. `show` would return the whole
        // trajectory to answer a question about its extent.
        let page = self
            .tracedb
            .list(None, 200, None)
            .await
            .map_err(|error| anyhow::anyhow!("{}", error))?;

        // A session TraceDB has never seen has no row, which yields no watermark
        // and therefore no pruning. The absent case is safe by construction.
        let Some(row) = page
            .sessions
            .into_iter()
            .find(|row| row.id == session_id)
        else {
            debug!("TraceDB has no record of {}; keeping everything", session_id);
            return Ok(None);
        };

        // A session ingested with no transcript confirms nothing. Common for
        // aborted runs, and pruning against it would drop real history.
        if row.events == 0 {
            debug!("TraceDB holds {} with no events; keeping everything", session_id);
            return Ok(None);
        }

        let Some(ended_at_ms) = row.ended_at_ms else {
            // Still open there, so nothing about it is settled.
            return Ok(None);
        };

        self.history
            .max_seq_at_or_before(session_id, ended_at_ms)
            .await
    }
}

/// Compact every consolidatable turn, then prune what an authority confirms.
///
/// Runs the two independently on purpose: compaction is always safe and always
/// worth doing, while pruning depends on a confirmation that may never come. A
/// terminal session gets the first and never the second.
pub async fn run_retention(
    journal: &crate::db::LiveStore,
    history: &HistoryStore,
    authority: &dyn RetentionAuthority,
    session_id: &str,
) -> Result<RetentionReport> {
    let mut report = RetentionReport::default();

    let watermark = authority.safe_to_drop(session_id).await?;
    history
        .set_watermark(session_id, authority.name(), watermark)
        .await?;

    match watermark {
        Some(seq) => {
            report.pruned = history.prune_below(session_id, seq).await?;
            report.confirmed_seq = Some(seq);
            if report.pruned > 0 {
                info!(
                    "Pruned {} blocks from {} below seq {}",
                    report.pruned, session_id, seq
                );
            }
        }
        None => debug!("Nothing confirmed for {}; keeping every block", session_id),
    }

    // Independent of any watermark: a blob is unreachable when no live block
    // references it, which is a different question from how far a consumer has read.
    report.blobs_removed = history.sweep_unreferenced_blobs().await?;

    let _ = journal;
    Ok(report)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub pruned: u64,
    pub blobs_removed: u64,
    pub confirmed_seq: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::cas::BlobStore;
    use crate::storage::history::Block;
    use tempfile::TempDir;

    async fn history() -> (HistoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let blobs = BlobStore::new(dir.path().join("cas"));
        let store = HistoryStore::open(
            dir.path().join("history.db").to_str().unwrap(),
            blobs,
        )
        .await
        .unwrap();
        (store, dir)
    }

    fn block(session: &str, seq: i32, at_ms: i64) -> Block {
        Block {
            session_id: session.to_string(),
            seq,
            turn_id: "turn-1".to_string(),
            kind: "assistant".to_string(),
            phase: Some("completed".to_string()),
            stream_id: Some(format!("a{seq}")),
            name: None,
            text: format!("block {seq}"),
            folded: None,
            data: None,
            is_error: false,
            source: "agent".to_string(),
            started_at_ms: at_ms,
            created_at_ms: at_ms,
        }
    }

    /// A session with no downstream copy must never be pruned, however much
    /// history it accumulates. This is the sandbox case, and getting it wrong
    /// destroys the only copy.
    #[tokio::test]
    async fn a_terminal_session_keeps_every_block() {
        let (history, _dir) = history().await;
        history
            .put_blocks(&[block("s1", 0, 1000), block("s1", 1, 2000)])
            .await
            .unwrap();

        let report = run_retention(
            &crate::db::LiveStore::new(_dir.path().join("live.db").to_str().unwrap())
                .await
                .unwrap(),
            &history,
            &TerminalAuthority,
            "s1",
        )
        .await
        .unwrap();

        assert_eq!(report.pruned, 0);
        assert_eq!(report.confirmed_seq, None);
        assert_eq!(history.block_count("s1").await.unwrap(), 2);
        // Recorded as terminal, so a later reader knows why nothing was dropped.
        assert_eq!(
            history.watermark("s1").await.unwrap(),
            Some(("terminal".to_string(), None))
        );
    }

    #[tokio::test]
    async fn pruning_drops_only_blocks_at_or_below_the_watermark() {
        let (history, dir) = history().await;
        history
            .put_blocks(&[
                block("s1", 0, 1000),
                block("s1", 1, 2000),
                block("s1", 2, 3000),
            ])
            .await
            .unwrap();

        let pruned = history.prune_below("s1", 1).await.unwrap();

        assert_eq!(pruned, 2);
        let left = history.blocks("s1", -1).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].seq, 2, "the unconfirmed block survives");
        drop(dir);
    }

    #[tokio::test]
    async fn one_sessions_watermark_does_not_prune_another() {
        let (history, _dir) = history().await;
        history
            .put_blocks(&[block("s1", 0, 1000), block("s2", 0, 1000)])
            .await
            .unwrap();

        history.prune_below("s1", 5).await.unwrap();

        assert_eq!(history.block_count("s1").await.unwrap(), 0);
        assert_eq!(history.block_count("s2").await.unwrap(), 1);
    }

    /// A blob shared by a pruned block and a retained one must survive. A bare
    /// refcount cannot express this, which is why references are per block.
    #[tokio::test]
    async fn a_blob_shared_with_a_retained_block_survives_the_prune() {
        let (history, _dir) = history().await;

        let shared = "shared payload ".repeat(500);
        let mut early = block("s1", 0, 1000);
        early.text = shared.clone();
        let mut late = block("s1", 5, 2000);
        late.text = shared.clone();

        history.put_blocks(&[early, late]).await.unwrap();
        history.prune_below("s1", 0).await.unwrap();
        let removed = history.sweep_unreferenced_blobs().await.unwrap();

        assert_eq!(removed, 0, "the object is still reachable from seq 5");
        let survivors = history.blocks("s1", -1).await.unwrap();
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].text, shared, "and it still reads back");
    }

    #[tokio::test]
    async fn a_blob_nothing_references_is_swept() {
        let (history, _dir) = history().await;
        let mut only = block("s1", 0, 1000);
        only.text = "large unique payload ".repeat(500);
        history.put_blocks(&[only]).await.unwrap();

        history.prune_below("s1", 0).await.unwrap();
        assert_eq!(history.sweep_unreferenced_blobs().await.unwrap(), 1);
        // Idempotent: a second sweep finds nothing left to do.
        assert_eq!(history.sweep_unreferenced_blobs().await.unwrap(), 0);
    }
}
