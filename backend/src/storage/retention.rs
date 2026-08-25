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

/// What an authority confirmed, and what it was looking at when it did.
///
/// The provenance travels with the sequence number rather than being fetched
/// separately, because the two are one observation: a `confirmed_seq` derived from
/// one version of a source, recorded beside a fingerprint read at another moment,
/// would be a watermark that cannot be checked against anything.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// Highest sequence number safe to drop. `None` means drop nothing.
    pub safe_to_drop: Option<i32>,
    /// The archived session consulted, in the archive's own id space.
    pub archive_id: Option<String>,
    /// Which version of the agent-native source the archive held.
    pub source_fingerprint: Option<String>,
}

impl Confirmation {
    /// Nothing is safe to drop, and there was no downstream copy to consult.
    pub fn nothing() -> Self {
        Self::default()
    }
}

/// Who confirms that a session's history is safe to drop.
#[async_trait]
pub trait RetentionAuthority: Send + Sync {
    /// A name for the journal, so a stored watermark records what produced it.
    fn name(&self) -> &'static str;

    /// How far a downstream copy is confirmed to extend.
    async fn confirm(&self, session_id: &str) -> Result<Confirmation>;
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

    async fn confirm(&self, _session_id: &str) -> Result<Confirmation> {
        Ok(Confirmation::nothing())
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
    /// The id the archive keys this session under, `<agent>:<native-id>`.
    ///
    /// Held explicitly rather than derived inside `safe_to_drop`, because the
    /// Sylph id and the archive id are disjoint spaces and passing the wrong one
    /// is silently always-false rather than an error. An authority that cannot be
    /// constructed without the archive id cannot be asked the unanswerable
    /// question — which is how the previous version confirmed nothing while
    /// looking correct.
    archive_id: String,
}

impl TraceDbAuthority {
    pub fn new(tracedb: TraceDbClient, history: HistoryStore, archive_id: String) -> Self {
        Self {
            tracedb,
            history,
            archive_id,
        }
    }
}

#[async_trait]
impl RetentionAuthority for TraceDbAuthority {
    fn name(&self) -> &'static str {
        "tracedb"
    }

    async fn confirm(&self, session_id: &str) -> Result<Confirmation> {
        // Targeted `show`, not a `list` scan. `list` cannot filter by id, so
        // answering from it means paging the archive until the row appears — and
        // an unanchored paged scan gives up early on a large archive, reporting
        // "absent" for a session that is present 1300 rows in.
        let Some(state) = self
            .tracedb
            .ingest_state(&self.archive_id)
            .await
            .map_err(|error| anyhow::anyhow!("{}", error))?
        else {
            // A session the archive has never seen has no row, which yields no
            // watermark and therefore no pruning. Safe by construction.
            debug!(
                "TraceDB has no record of {} ({}); keeping everything",
                session_id, self.archive_id
            );
            return Ok(Confirmation::nothing());
        };

        // Recorded even when nothing is confirmed. A watermark that names the
        // archived session it consulted is checkable later; one that records only
        // "nothing yet" cannot be told apart from one that never looked.
        let seen = Confirmation {
            safe_to_drop: None,
            archive_id: Some(self.archive_id.clone()),
            source_fingerprint: state.source_fingerprint.clone(),
        };

        // Only a `full` record claims to be lossless. `partial` survives as a
        // readable compatibility spelling upstream, and pruning against one would
        // trade a complete copy here for an incomplete copy there.
        if state.mode.as_deref() != Some("full") {
            debug!(
                "TraceDB holds {} as mode {:?}, not full; keeping everything",
                self.archive_id, state.mode
            );
            return Ok(seen);
        }

        let Some(ended_at_ms) = state.ended_at_ms else {
            // No last-event timestamp means nothing about its extent is settled,
            // so there is no bound to prune against.
            debug!(
                "TraceDB reports no end for {}; keeping everything",
                self.archive_id
            );
            return Ok(seen);
        };

        Ok(Confirmation {
            safe_to_drop: self
                .history
                .max_seq_at_or_before(session_id, ended_at_ms)
                .await?,
            ..seen
        })
    }
}

/// Which environments keep a store that TraceDB can ingest.
///
/// `local` is the host's own filesystem, which is exactly what TraceDB scans. A
/// Khora sandbox or an SSH host keeps its agent-native store somewhere TraceDB
/// never reads, and in the sandbox case that store dies with the environment — so
/// this record is the only copy that will ever exist.
///
/// This cannot be observed from the archive. An absent archive row means either
/// "not ingested yet" or "never will be", and the two demand the same *action*
/// (keep everything) but different *reasoning*: waiting versus permanent. Reading
/// the environment is what tells them apart, which is why the discriminator is the
/// [`crate::env::EnvironmentDescriptor`]'s `kind` and not a guess.
fn environment_has_ingestable_store(environment_kind: &str) -> bool {
    matches!(environment_kind, "local")
}

/// Who confirms a given session, and why.
///
/// Returned rather than a bare `Box<dyn RetentionAuthority>` so the caller can
/// report *which* class a session fell into. A retention pass that prunes nothing
/// is correct for a terminal session and a symptom for a delegated one, and
/// without the class those two are indistinguishable in a log.
pub enum Resolved {
    /// A downstream copy exists, or will once ingestion runs.
    Delegated(TraceDbAuthority),
    /// No ingestion path. Compact forever, prune never.
    Terminal(TerminalAuthority),
}

impl Resolved {
    pub fn authority(&self) -> &dyn RetentionAuthority {
        match self {
            Self::Delegated(authority) => authority,
            Self::Terminal(authority) => authority,
        }
    }
}

/// Decide which authority governs a session.
///
/// Two independent conditions, and failing either makes the session terminal:
///
/// 1. The environment keeps a store TraceDB can reach.
/// 2. An archive id is derivable — without a native id, nothing downstream can
///    even be asked about this session.
///
/// Anything unrecognized falls to terminal, because the safe direction to be wrong
/// in is keeping too much. Growth is visible and fixable; loss is neither.
pub fn resolve_authority(
    tracedb: &TraceDbClient,
    history: &HistoryStore,
    environment_kind: &str,
    agent: &str,
    backend_session_id: Option<&str>,
    trace_session_id: Option<&str>,
) -> Resolved {
    if !environment_has_ingestable_store(environment_kind) {
        return Resolved::Terminal(TerminalAuthority);
    }

    match crate::tracedb::archive_id(agent, backend_session_id, trace_session_id) {
        Some(archive_id) => Resolved::Delegated(TraceDbAuthority::new(
            tracedb.clone(),
            history.clone(),
            archive_id,
        )),
        None => Resolved::Terminal(TerminalAuthority),
    }
}

/// Prune one session's history down to what an authority confirms.
///
/// Records the watermark whether or not anything was confirmed, because "checked
/// and nothing is safe yet" and "never checked" call for different responses and
/// look identical without the row.
///
/// Blob sweeping is deliberately *not* here. Reachability is not a property of one
/// session — the same file read in two sessions is one blob — so the sweep is a
/// global scan over `cas_refs`, and calling it per session would run that scan once
/// per session for one pass's worth of work. [`sweep_blobs`] is the per-pass
/// counterpart, and the split is what keeps a pass O(sessions) rather than
/// O(sessions × blobs).
pub async fn run_retention(
    history: &HistoryStore,
    authority: &dyn RetentionAuthority,
    session_id: &str,
) -> Result<RetentionReport> {
    let confirmation = authority.confirm(session_id).await?;

    history
        .set_watermark(session_id, authority.name(), &confirmation)
        .await?;

    let mut report = RetentionReport {
        confirmed_seq: confirmation.safe_to_drop,
        ..RetentionReport::default()
    };

    match confirmation.safe_to_drop {
        Some(seq) => {
            report.pruned = history.prune_below(session_id, seq).await?;
            if report.pruned > 0 {
                info!(
                    "Pruned {} blocks from {} below seq {}",
                    report.pruned, session_id, seq
                );
            }
        }
        None => debug!("Nothing confirmed for {}; keeping every block", session_id),
    }

    Ok(report)
}

/// Remove blobs no live block references any more, for a whole pass.
///
/// Independent of every watermark: a blob is unreachable when nothing points at it,
/// which is a different question from how far a consumer has read. A blob shared
/// between a pruned block and a retained one survives.
pub async fn sweep_blobs(history: &HistoryStore) -> Result<u64> {
    let removed = history.sweep_unreferenced_blobs().await?;
    if removed > 0 {
        info!("Swept {} unreferenced blob(s)", removed);
    }
    Ok(removed)
}

/// What one session's retention pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    pub pruned: u64,
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
            span: None,
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

        let report = run_retention(&history, &TerminalAuthority, "s1")
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

    // ---- Archive identity -------------------------------------------------
    //
    // These pin the join between two disjoint id spaces. Getting it wrong is not
    // an error at any layer: the archive is asked about an id no row can carry,
    // answers "no such session", and retention correctly keeps everything. The
    // system stays safe and stops doing its job, which is why the original defect
    // survived a written and tested authority.

    #[test]
    fn a_linked_session_is_addressed_as_agent_and_native_id() {
        assert_eq!(
            crate::tracedb::archive_id("claude", Some("9bdacb6e-b413"), None),
            Some("claude:9bdacb6e-b413".to_string()),
            "the archive keys sessions as <agent>:<native-id>, never as the Sylph id"
        );
    }

    /// Without a native id there is nothing downstream to ask about, so the
    /// session is terminal rather than unconfirmed.
    #[test]
    fn a_session_that_never_linked_has_no_archive_identity() {
        assert_eq!(crate::tracedb::archive_id("claude", None, None), None);
        assert_eq!(crate::tracedb::archive_id("claude", Some("  "), None), None);
    }

    /// An imported session already carries the archive's own id, which may name a
    /// subagent and survives a later incarnation reassigning the native id.
    #[test]
    fn an_imported_session_keeps_the_archive_id_it_was_imported_from() {
        assert_eq!(
            crate::tracedb::archive_id(
                "claude",
                Some("native-2"),
                Some("claude:original/agent-explore")
            ),
            Some("claude:original/agent-explore".to_string())
        );
    }

    /// A composite naming delegated work is not this session's own record.
    /// Claiming it would prune against a confirmation about different work.
    #[test]
    fn a_native_id_that_would_form_a_subagent_identity_is_refused() {
        assert_eq!(
            crate::tracedb::archive_id("claude", Some("uuid/agent-x"), None),
            None
        );
        assert_eq!(
            crate::tracedb::archive_id("claude", Some("claude:uuid"), None),
            None
        );
    }

    // ---- Authority resolution ---------------------------------------------

    async fn resolved_name(environment: &str, native: Option<&str>) -> &'static str {
        let (history, _dir) = history().await;
        let tracedb = crate::tracedb::Client::new("/nonexistent/trace-db".into());
        resolve_authority(&tracedb, &history, environment, "claude", native, None)
            .authority()
            .name()
    }

    #[tokio::test]
    async fn a_local_session_with_a_native_id_is_delegated_to_tracedb() {
        assert_eq!(resolved_name("local", Some("native-1")).await, "tracedb");
    }

    /// A sandbox's agent-native store dies with the sandbox, so this record is the
    /// only copy that will ever exist. Pruning it would destroy exactly the
    /// isolated work the terminal class exists to protect.
    #[tokio::test]
    async fn a_session_in_an_environment_tracedb_cannot_read_is_terminal() {
        assert_eq!(resolved_name("khora", Some("native-1")).await, "terminal");
        assert_eq!(resolved_name("ssh", Some("native-1")).await, "terminal");
    }

    #[tokio::test]
    async fn an_unlinked_local_session_is_terminal() {
        assert_eq!(resolved_name("local", None).await, "terminal");
    }

    /// The safe direction to be wrong in is keeping too much, so an environment
    /// nobody has taught this code about must not be treated as ingestable.
    #[tokio::test]
    async fn an_unrecognized_environment_falls_back_to_terminal() {
        assert_eq!(resolved_name("something-new", Some("n1")).await, "terminal");
    }

    // ---- Watermark provenance ---------------------------------------------

    /// A pass that confirms nothing still has to leave a row, or "checked and
    /// nothing is safe yet" cannot be told from "never checked" — and those call
    /// for opposite responses.
    #[tokio::test]
    async fn a_pass_that_confirms_nothing_still_records_that_it_looked() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();

        run_retention(&history, &TerminalAuthority, "s1").await.unwrap();

        assert_eq!(
            history.watermark("s1").await.unwrap(),
            Some(("terminal".to_string(), None)),
            "the row exists and says why nothing was dropped"
        );
    }

    #[tokio::test]
    async fn a_confirmation_records_the_archived_session_and_source_it_consulted() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();

        history
            .set_watermark(
                "s1",
                "tracedb",
                &Confirmation {
                    safe_to_drop: Some(0),
                    archive_id: Some("claude:abc".to_string()),
                    source_fingerprint: Some("file-v2:deadbeef:42:1000".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            history.watermark_fingerprint("s1").await.unwrap(),
            Some("file-v2:deadbeef:42:1000".to_string()),
            "so a later pass can see whether the archive re-ingested the source"
        );
    }

    /// A terminal session has no archived counterpart, so it must not be left
    /// carrying provenance from some earlier state.
    #[tokio::test]
    async fn a_terminal_pass_records_no_fingerprint() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();

        run_retention(&history, &TerminalAuthority, "s1").await.unwrap();

        assert_eq!(history.watermark_fingerprint("s1").await.unwrap(), None);
    }

    // ---- Byte accounting --------------------------------------------------
    //
    // These pin the one question the counts cannot answer. A terminal session
    // keeping every block is correct behaviour and identical, in every count, to a
    // session growing without bound — which is the actual state a full disk is in.

    /// A block-count report cannot rank sessions: one large tool result and one
    /// short status are both one block. Bytes are what separate them.
    #[tokio::test]
    async fn bytes_held_distinguish_a_large_session_from_a_long_one() {
        let (history, _dir) = history().await;

        let mut heavy = block("heavy", 0, 1000);
        heavy.text = "x".repeat(40_000);
        let mut chatty = Vec::new();
        for seq in 0..20 {
            chatty.push(block("chatty", seq, 1000 + seq as i64));
        }

        history.put_blocks(&[heavy]).await.unwrap();
        history.put_blocks(&chatty).await.unwrap();

        let (heavy_bytes, heavy_blocks) = history.measure_session_bytes("heavy").await.unwrap();
        let (chatty_bytes, chatty_blocks) =
            history.measure_session_bytes("chatty").await.unwrap();

        assert_eq!(heavy_blocks, 1);
        assert_eq!(chatty_blocks, 20);
        assert!(
            heavy_bytes > chatty_bytes * 10,
            "one 40 KB block outweighs twenty short ones: {heavy_bytes} vs {chatty_bytes}"
        );
        // Measured as the *logical* record, so an externalized payload still
        // reports its uncompressed size rather than the ~40 bytes that highly
        // repetitive text compresses to. Ranking by stored bytes would rank
        // partly by compressibility.
        assert!(
            heavy_bytes >= 40_000,
            "the uncompressed length is what is reported: {heavy_bytes}"
        );
    }

    /// The largest sessions are named, because "which session" is the question a
    /// full disk actually poses.
    #[tokio::test]
    async fn the_report_names_which_sessions_hold_the_most() {
        let (history, _dir) = history().await;
        for (session, size) in [("small", 100), ("largest", 50_000), ("middle", 5_000)] {
            let mut only = block(session, 0, 1000);
            only.text = "y".repeat(size);
            history.put_blocks(&[only]).await.unwrap();
            history.record_session_bytes(session).await.unwrap();
        }

        let largest = history.largest_sessions(2).await.unwrap();

        assert_eq!(
            largest.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            vec!["largest", "middle"],
            "largest first, and bounded to what was asked for"
        );

        let (total, measured) = history.total_bytes().await.unwrap();
        assert_eq!(measured, 3);
        assert!(total >= 55_100, "every measured session is counted: {total}");
    }

    /// Growth is a difference between two observations, so a session measured once
    /// must report *unknown* rather than zero. Reporting zero would claim a
    /// session is stable on the strength of never having been looked at twice.
    #[tokio::test]
    async fn a_first_measurement_reports_no_growth_rather_than_zero_growth() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();

        let first = history.record_session_bytes("s1").await.unwrap();

        assert!(first.bytes > 0);
        assert_eq!(first.grew_by, None, "one observation is not a trend");
        assert_eq!(first.grew_over_ms, None);
    }

    #[tokio::test]
    async fn growth_is_reported_once_a_session_has_grown() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();
        let before = history.record_session_bytes("s1").await.unwrap();

        let mut added = block("s1", 1, 2000);
        added.text = "z".repeat(10_000);
        history.put_blocks(&[added]).await.unwrap();
        let after = history.record_session_bytes("s1").await.unwrap();

        assert_eq!(after.grew_by, Some(after.bytes - before.bytes));
        assert!(after.grew_by.unwrap() >= 10_000, "{:?}", after.grew_by);
    }

    /// A pass that changes nothing must not flatten the baseline, or a genuinely
    /// growing session reads as stable the moment two passes happen to agree.
    #[tokio::test]
    async fn an_unchanged_measurement_keeps_the_earlier_baseline() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();
        history.record_session_bytes("s1").await.unwrap();

        let mut added = block("s1", 1, 2000);
        added.text = "z".repeat(10_000);
        history.put_blocks(&[added]).await.unwrap();
        let grown = history.record_session_bytes("s1").await.unwrap();

        // Nothing changed between these two.
        let again = history.record_session_bytes("s1").await.unwrap();

        assert_eq!(
            again.grew_by, grown.grew_by,
            "re-measuring an unchanged session must not report it as stable"
        );
    }

    /// Pruning has to be visible as a *negative* delta. A field that only ever
    /// reported increases would make a working archive indistinguishable from a
    /// stalled one.
    #[tokio::test]
    async fn pruning_shows_up_as_a_negative_delta() {
        let (history, _dir) = history().await;
        let mut early = block("s1", 0, 1000);
        early.text = "a".repeat(20_000);
        history.put_blocks(&[early, block("s1", 1, 2000)]).await.unwrap();
        history.record_session_bytes("s1").await.unwrap();

        history.prune_below("s1", 0).await.unwrap();
        let after = history.record_session_bytes("s1").await.unwrap();

        assert!(
            after.grew_by.unwrap() < -19_000,
            "pruning reads as shrinkage: {:?}",
            after.grew_by
        );
    }

    // ---- Batch selection --------------------------------------------------

    /// Ordered by last check so a bounded batch rotates through every session.
    /// Ordering by id would re-examine one alphabetical prefix forever, and the
    /// sessions that sort late would never be pruned at all.
    #[tokio::test]
    async fn the_batch_offers_never_checked_sessions_before_recently_checked_ones() {
        let (history, _dir) = history().await;
        history
            .put_blocks(&[
                block("aaa-checked", 0, 1000),
                block("zzz-fresh", 0, 1000),
            ])
            .await
            .unwrap();

        // The alphabetically-first session is the one already checked.
        history
            .set_watermark("aaa-checked", "terminal", &Confirmation::nothing())
            .await
            .unwrap();

        let batch = history.sessions_awaiting_retention(10).await.unwrap();

        assert_eq!(
            batch.first().map(String::as_str),
            Some("zzz-fresh"),
            "never-checked wins over alphabetical order: {batch:?}"
        );
    }

    /// Only sessions holding blocks are examined. One whose history is already
    /// empty has nothing to prune, so a round trip about it is pure cost.
    #[tokio::test]
    async fn a_session_with_no_blocks_left_is_not_offered_for_retention() {
        let (history, _dir) = history().await;
        history.put_blocks(&[block("s1", 0, 1000)]).await.unwrap();
        history.prune_below("s1", 0).await.unwrap();

        assert!(history
            .sessions_awaiting_retention(10)
            .await
            .unwrap()
            .is_empty());
    }
}
