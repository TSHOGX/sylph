//! When retention runs, and why it is safe to run it often.
//!
//! [`retention`](crate::storage::retention) answers *what* is safe to drop; this
//! module decides *when* to ask. Without it the whole watermark design is
//! unreachable — `history.db` grows without bound and the authorities that were
//! written and tested confirm nothing, because nothing ever calls them. That was
//! the state this module was added to fix, and the symptom was invisible: an empty
//! `watermarks` table looks exactly like a correctly conservative one.
//!
//! Shaped like [`Consolidator`](crate::storage::Consolidator) on purpose — startup
//! pass, then an interval, one permit at a time — because the two do the same kind
//! of work for the same reason and a second scheduling idiom would be a second
//! thing to reason about.
//!
//! **A pass is cheap by construction, not by skipping.** Each session costs one
//! targeted `show` (~0.5 ms) against a bounded batch, so a pass is tens of
//! milliseconds every five minutes. Nothing here tries to avoid that round trip:
//! the fingerprint that would tell us a session is unchanged *arrives in the
//! response*, so it cannot gate the call that fetches it, and the writes it could
//! skip are one upsert and a DELETE matching no rows. It is recorded as provenance
//! and counted as a staleness signal — a pass where every session is unchanged
//! means the archive is not advancing, which is the shape of a broken ingestion
//! daemon.
//!
//! A global epoch to poll instead was considered and rejected: the only surface
//! carrying TraceDB's archive-wide ack is `doctor --json`, measured at 2m34s
//! because it walks every agent's native store — far more expensive than the
//! per-session reads it would be gating.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, error, info, warn};

use crate::db::JournalStore;
use crate::storage::history::HistoryStore;
use crate::storage::retention::{self, Resolved};
use crate::tracedb::Client as TraceDbClient;

/// How many sessions one pass examines.
///
/// Bounded so a large archive cannot make a pass unbounded work. The batch is
/// drawn least-recently-checked first, so successive passes rotate through every
/// session rather than re-examining one prefix.
const PASS_BATCH: i64 = 64;

/// How long between passes.
///
/// Retention is not latency-sensitive: nothing a user does waits on it, and the
/// cost of running late is disk rather than correctness. Matched to the
/// consolidator's interval because both are background disk work and staggering
/// them buys nothing while making two numbers to keep in mind.
const PASS_INTERVAL: Duration = Duration::from_secs(300);

/// How many sessions the byte report names.
///
/// The report exists to answer "which session do I go look at", which a handful
/// answers and a full listing does not — it would put every session in a
/// diagnostics response, and growth is a heavily skewed distribution: one
/// long-lived sandbox session dwarfs forty ordinary ones.
const LARGEST_REPORTED: i64 = 10;

/// What a completed pass did, for `/diagnostics`.
///
/// Reported because a subsystem whose correct behaviour is "nothing happened" is
/// otherwise indistinguishable from one that is broken. `withheld` beside
/// `confirmed` is the load-bearing pair: withholding is right for a terminal
/// session and a symptom for a delegated one, and a count of pruned blocks alone
/// cannot tell those apart.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassReport {
    pub ran_at_ms: i64,
    /// Sessions examined this pass.
    pub examined: usize,
    /// Sessions whose archived source was byte-identical to the last pass.
    ///
    /// A diagnostic rather than a skip. All-unchanged across passes means the
    /// archive is not advancing — a stopped ingestion daemon looks exactly like a
    /// healthy steady state otherwise.
    pub unchanged: usize,
    /// Sessions where an authority confirmed a sequence number.
    pub confirmed: usize,
    /// Sessions where nothing was safe to drop.
    pub withheld: usize,
    /// Sessions governed by `terminal` — no ingestion path, so never pruned.
    pub terminal: usize,
    pub blocks_pruned: u64,
    pub blobs_swept: u64,
    /// Bytes of record held across every session ever measured, and how many.
    ///
    /// Present because the counts above answer "was anything dropped" and cannot
    /// answer "how much is being kept" — and for a terminal session, which is
    /// *supposed* to keep everything, the second is the only question with an
    /// actionable answer. See
    /// [`HistoryStore::measure_session_bytes`](crate::storage::HistoryStore::measure_session_bytes)
    /// for what a byte is here: the logical record, not the footprint on disk.
    pub bytes_held: i64,
    pub sessions_measured: i64,
    /// Net change across the sessions this pass measured.
    ///
    /// Signed: negative means pruning outpaced arrival, which is a working
    /// archive rather than an anomaly.
    pub bytes_grew_by: i64,
    /// The largest sessions, largest first, each with its own growth.
    ///
    /// Bounded to a handful. The question this answers is "which session do I go
    /// look at", and a full listing would put every session in a diagnostics
    /// response to answer a question about its head.
    pub largest_sessions: Vec<crate::storage::history::SessionBytes>,
    /// Sessions whose pass failed, so their history is simply kept.
    pub failed: usize,
    /// Whether the archive could be reached at all this pass.
    ///
    /// Probed once per pass rather than discovered per session. An install without
    /// TraceDB is a supported degraded state, and treating it as N session failures
    /// would report a configuration fact as a fault N times per pass forever —
    /// while `false` here says the actionable thing once.
    ///
    /// A pass with no archive prunes nothing and records nothing, because "cannot
    /// ask" is not "asked and nothing is safe". Writing a watermark would claim a
    /// check that never happened.
    pub archive_available: bool,
}

/// Bytes for a log line, in the unit a reader can act on.
///
/// A raw count is the wrong shape here: `1183741952` and `118374195` differ by an
/// order of magnitude and read as the same length, which is exactly the
/// distinction the line exists to convey. The wire format keeps raw integers —
/// a client should not have to parse a rendered string back.
fn human_bytes(bytes: i64) -> String {
    const KIB: f64 = 1024.0;
    let magnitude = bytes.unsigned_abs() as f64;
    let sign = if bytes < 0 { "-" } else { "" };

    for (limit, unit) in [
        (KIB, "B"),
        (KIB * KIB, "KiB"),
        (KIB * KIB * KIB, "MiB"),
    ] {
        if magnitude < limit {
            let scaled = magnitude / (limit / KIB);
            return if unit == "B" {
                format!("{sign}{magnitude:.0} B")
            } else {
                format!("{sign}{scaled:.1} {unit}")
            };
        }
    }
    format!("{sign}{:.1} GiB", magnitude / (KIB * KIB * KIB))
}

/// Runs retention passes over consolidated history.
#[derive(Clone)]
pub struct RetentionScheduler {
    journal: JournalStore,
    history: HistoryStore,
    tracedb: TraceDbClient,
    /// The environment sessions run in.
    ///
    /// One value today because `local` is the only environment. It is a field
    /// rather than a literal at the call site so a per-session environment later
    /// replaces one lookup instead of introducing the concept.
    environment_kind: String,
    /// One pass at a time, for the same reason the consolidator serializes: this is
    /// I/O-heavy background work and competing with the token path for the same
    /// disk is what the tiering exists to avoid.
    gate: Arc<Semaphore>,
    last: Arc<Mutex<Option<PassReport>>>,
}

impl RetentionScheduler {
    pub fn new(
        journal: JournalStore,
        history: HistoryStore,
        tracedb: TraceDbClient,
        environment_kind: impl Into<String>,
    ) -> Self {
        Self {
            journal,
            history,
            tracedb,
            environment_kind: environment_kind.into(),
            gate: Arc::new(Semaphore::new(1)),
            last: Arc::new(Mutex::new(None)),
        }
    }

    /// The most recent pass, or `None` before the first one finishes.
    pub async fn last_pass(&self) -> Option<PassReport> {
        self.last.lock().await.clone()
    }

    /// Run one pass now, returning what it did.
    pub async fn pass(&self) -> PassReport {
        let _permit = match self.gate.acquire().await {
            Ok(permit) => permit,
            // Never closed; the periodic pass is the safety net if it somehow were.
            Err(_) => return PassReport::default(),
        };

        let mut report = PassReport {
            ran_at_ms: chrono::Utc::now().timestamp_millis(),
            ..PassReport::default()
        };

        // Once, before any session. A delegated session cannot be confirmed without
        // the archive, and asking each one separately turns one configuration fact
        // into a per-session failure.
        report.archive_available = self.tracedb.probe().await;

        let sessions = match self.history.sessions_awaiting_retention(PASS_BATCH).await {
            Ok(sessions) => sessions,
            Err(error) => {
                error!("Could not list sessions awaiting retention: {:#}", error);
                return report;
            }
        };

        for session_id in sessions {
            report.examined += 1;
            match self.run_one(&session_id, &mut report).await {
                Ok(()) => {}
                // One session's failure must not stop the pass, or a single
                // unreachable archive row would block every session behind it.
                // Keeping its history is always the safe outcome.
                Err(error) => {
                    report.failed += 1;
                    warn!("Retention pass failed for {}: {:#}", session_id, error);
                }
            }

            // Measured on every path, including the two `run_one` returns early
            // on and the one that failed. Bytes held is a local fact that needs no
            // authority, and the install where nothing can be confirmed — no
            // TraceDB, so nothing is ever pruned — is precisely the one whose
            // growth is unbounded. Measuring only where a watermark was written
            // would leave that install's report empty.
            //
            // After the prune rather than before, so the number is what is held
            // now rather than what was held a moment ago.
            match self.history.record_session_bytes(&session_id).await {
                Ok(measured) => report.bytes_grew_by += measured.grew_by.unwrap_or(0),
                Err(error) => debug!("Could not measure {}: {:#}", session_id, error),
            }
        }

        // Once per pass, not once per session: reachability spans sessions, so this
        // is a global scan over `cas_refs`.
        match retention::sweep_blobs(&self.history).await {
            Ok(removed) => report.blobs_swept = removed,
            Err(error) => warn!("Could not sweep unreferenced blobs: {:#}", error),
        }

        // Read after the loop so the total and the rows beside it are one
        // observation. Both are whole-store facts rather than per-pass ones: a
        // bounded batch means this pass measured 64 sessions, and an operator
        // asking what is being kept means all of them.
        match self.history.total_bytes().await {
            Ok((bytes, sessions)) => {
                report.bytes_held = bytes;
                report.sessions_measured = sessions;
            }
            Err(error) => warn!("Could not total held bytes: {:#}", error),
        }
        match self.history.largest_sessions(LARGEST_REPORTED).await {
            Ok(largest) => report.largest_sessions = largest,
            Err(error) => warn!("Could not list the largest sessions: {:#}", error),
        }

        if report.blocks_pruned > 0 || report.blobs_swept > 0 {
            info!(
                "Retention pass: examined {}, pruned {} block(s), swept {} blob(s), \
                 holding {} across {} session(s)",
                report.examined,
                report.blocks_pruned,
                report.blobs_swept,
                human_bytes(report.bytes_held),
                report.sessions_measured
            );
        } else if report.bytes_grew_by > 0 {
            // One line naming the single largest holder, not one per growing
            // session: growth every pass is the *expected* state of a terminal
            // session, so a line per session would be an unbounded stream
            // reporting correct behaviour. `/diagnostics` carries the whole list
            // for anyone who wants it, and this says only enough to know whether
            // to go look.
            //
            // At `info` rather than `warn`: growth is not a fault. There is no
            // threshold above which this subsystem would be entitled to delete a
            // terminal session's record, so a warning would name a problem the
            // reader cannot act on through any setting — see `docs/storage.md` for
            // why no such bound is offered.
            info!(
                "Retention pass: holding {} across {} session(s), {} added this pass{}",
                human_bytes(report.bytes_held),
                report.sessions_measured,
                human_bytes(report.bytes_grew_by),
                match report.largest_sessions.first() {
                    Some(largest) => format!(
                        "; largest is {} at {}",
                        largest.session_id,
                        human_bytes(largest.bytes)
                    ),
                    None => String::new(),
                }
            );
        } else if !report.archive_available && report.examined > 0 {
            // Once per pass, not once per session, and at debug because an install
            // without TraceDB is a supported configuration rather than a fault.
            // `/diagnostics` carries the same fact for anyone looking.
            debug!(
                "Retention pass: archive unavailable, so {} session(s) keep everything",
                report.examined
            );
        } else {
            debug!(
                "Retention pass: examined {} session(s), {} unchanged, nothing to drop",
                report.examined, report.unchanged
            );
        }

        *self.last.lock().await = Some(report.clone());
        report
    }

    /// One session: resolve its authority, skip it if unchanged, otherwise prune.
    async fn run_one(&self, session_id: &str, report: &mut PassReport) -> anyhow::Result<()> {
        // A session whose journal row is gone cannot have its authority resolved —
        // there is no agent or native id to derive an archive identity from — so it
        // is treated as terminal and its history is kept.
        let session = self.journal.get_session(session_id).await?;
        let Some(session) = session else {
            report.terminal += 1;
            report.withheld += 1;
            debug!(
                "No journal row for {}; keeping its history rather than guessing",
                session_id
            );
            return Ok(());
        };

        let resolved = retention::resolve_authority(
            &self.tracedb,
            &self.history,
            &self.environment_kind,
            &session.agent,
            session.backend_session_id.as_deref(),
            session.trace_session_id.as_deref(),
        );

        if matches!(resolved, Resolved::Terminal(_)) {
            report.terminal += 1;
        }

        // A delegated session with no reachable archive is left entirely alone: not
        // pruned, and not recorded as checked. Recording it would write a watermark
        // asserting a confirmation that was never obtained, and "cannot ask" must
        // not be stored as "asked, and nothing is safe yet".
        //
        // A terminal session still proceeds. It never consults the archive, so its
        // answer does not depend on one being there.
        if matches!(resolved, Resolved::Delegated(_)) && !report.archive_available {
            report.withheld += 1;
            debug!(
                "Archive unavailable; leaving {}'s history and watermark untouched",
                session_id
            );
            return Ok(());
        }

        // Read before the pass overwrites it, so the two fingerprints being
        // compared are the previous observation and the current one.
        let previous_fingerprint = self.history.watermark_fingerprint(session_id).await?;

        let outcome = retention::run_retention(&self.history, resolved.authority(), session_id)
            .await?;

        // Only meaningful when both exist: a first pass has no previous fingerprint
        // and a terminal session never has one, and neither is "unchanged".
        if let (Some(before), Some(after)) = (
            previous_fingerprint,
            self.history.watermark_fingerprint(session_id).await?,
        ) {
            if before == after {
                report.unchanged += 1;
            }
        }

        match outcome.confirmed_seq {
            Some(_) => report.confirmed += 1,
            None => report.withheld += 1,
        }
        report.blocks_pruned += outcome.pruned;
        Ok(())
    }

    /// Pass now, then keep passing.
    ///
    /// Immediately at startup because a restart is when history is most likely to
    /// hold blocks nobody has ever checked — including everything accumulated
    /// before this scheduler existed.
    pub fn spawn(&self) {
        let scheduler = self.clone();
        tokio::spawn(async move {
            scheduler.pass().await;

            let mut ticker = tokio::time::interval(PASS_INTERVAL);
            // The first tick is immediate and the pass above already covered it.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                scheduler.pass().await;
            }
        });
    }
}
