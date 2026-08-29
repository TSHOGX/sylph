//! When retention runs, and why it is safe to run it often.
//!
//! [`crate::storage::retention`] answers *what* is safe to drop; this
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
//! **A pass is cheap by construction, not by skipping.** It examines a bounded batch,
//! drawn least-recently-checked first so successive passes rotate through every session
//! rather than re-examining one prefix.
//!
//! **An unreachable environment is one fact about the pass, not N session failures.**
//! Each environment is probed once, before any session. This repo has paid for the other
//! shape twice, one layer apart: per-session probing turned one missing dependency into
//! a warning per session per pass, and a timer-driven caller then reproduced the same
//! unbounded stream a layer down — 2339 identical lines in one measured E2E run. An
//! install with no environment paired is a supported configuration, not a fault.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, error, info, warn};

use crate::db::JournalStore;
use crate::env::Environment;
use crate::storage::history::HistoryStore;
use crate::storage::retention::{self, Resolved};
use sylph_protocol::SessionId;
use crate::models::now_ms;

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
    /// Sessions where an authority confirmed a sequence number.
    pub confirmed: usize,
    /// Sessions where nothing was safe to drop.
    ///
    /// **Counted from the authority's own reported reason, never inferred from an
    /// absent bound.** `confirmed_seq.is_none()` cannot separate "the environment
    /// answered and nothing is safe yet" from "no envagent was connected, so nobody was
    /// asked", and those call for opposite responses — the second means the check never
    /// happened. `withheld_reasons` below is what makes the difference legible.
    pub withheld: usize,
    /// How many sessions withheld for each reason, so a report says *why* rather than
    /// only *how many*.
    ///
    /// A `BTreeMap` for a stable key order: an unordered map churns the diagnostics
    /// response between calls for no reason a reader can act on.
    pub withheld_reasons: std::collections::BTreeMap<String, usize>,
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
    /// Which environments answered this pass, and which did not.
    ///
    /// Probed once per pass rather than per session, and reported per environment rather
    /// than as one boolean — which is the better question the rewrite bought. A single
    /// `archive_available` could only say "the archive is unreachable"; this
    /// distinguishes a missing CLI from a sleeping laptop from an exhausted sandbox node
    /// (`docs/environments.md` § Health is per environment).
    ///
    /// A session whose environment is absent from this map, or present and false, is
    /// **left entirely alone**: not pruned, and *not* recorded as checked.
    pub environments_reachable: std::collections::BTreeMap<String, bool>,
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
    /// The environment sessions run in.
    ///
    /// One today. Held as the trait object rather than a kind string because the
    /// authority now asks it a *question* — is this session's native source still
    /// there — rather than branching on what kind of environment it is. That branch
    /// (`matches!(kind, "local")`) was the "who started it" reading `docs/storage.md`
    /// forbids.
    environment: Arc<dyn Environment>,
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
        environment: Arc<dyn Environment>,
    ) -> Self {
        Self {
            journal,
            history,
            environment,
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
            ran_at_ms: now_ms(),
            ..PassReport::default()
        };

        // Once, before any session. A backed session cannot be confirmed without its
        // environment, and asking per session turns one configuration fact into a
        // per-session failure.
        let descriptor = self.environment.descriptor();
        let reachable = self.environment.probe().await.available;
        report
            .environments_reachable
            .insert(descriptor.id.clone(), reachable);

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
        } else if report
            .environments_reachable
            .values()
            .any(|reachable| !reachable)
            && report.examined > 0
        {
            // Once per pass, not once per session, and at `debug` because an install
            // with an unpaired environment is a supported configuration rather than a
            // fault. `/diagnostics` carries the same fact per environment for anyone
            // looking.
            debug!(
                "Retention pass: {} environment(s) unreachable, so some of {} session(s) \
                 keep everything",
                report
                    .environments_reachable
                    .values()
                    .filter(|reachable| !**reachable)
                    .count(),
                report.examined
            );
        } else {
            debug!(
                "Retention pass: examined {} session(s), {} withheld, nothing to drop",
                report.examined, report.withheld
            );
        }

        *self.last.lock().await = Some(report.clone());
        report
    }

    /// One session: resolve its authority, skip it if unchanged, otherwise prune.
    async fn run_one(&self, encoded: &str, report: &mut PassReport) -> anyhow::Result<()> {
        // `history.db` keys on the encoded triple, so a batch arrives as strings and
        // this is where the decode happens. A key that will not decode cannot name a
        // session, so nothing can be asked about it and its history is kept — the safe
        // direction, and counted so a corrupt row is visible rather than silently
        // skipped.
        let Ok(session_id) = SessionId::decode(encoded) else {
            report.terminal += 1;
            self.record_withheld(report, retention::WithheldReason::NoNativeSource);
            warn!("History holds blocks under an unparseable session id: {encoded}");
            return Ok(());
        };

        // A session whose journal row is gone cannot be asked about: nothing says which
        // environment held its native source. Terminal, and its history is kept.
        if self.journal.get_session(&session_id).await?.is_none() {
            report.terminal += 1;
            self.record_withheld(report, retention::WithheldReason::NoNativeSource);
            debug!(
                "No journal row for {}; keeping its history rather than guessing",
                encoded
            );
            return Ok(());
        }

        let resolved = retention::resolve_authority(&self.environment, &session_id);

        if matches!(resolved, Resolved::Terminal(_)) {
            report.terminal += 1;
        }

        // A backed session whose environment did not answer is left **entirely alone**:
        // not pruned, and *not* recorded as checked. Writing a watermark would assert a
        // confirmation that was never obtained, and "cannot ask" must not be stored as
        // "asked, and nothing is safe yet" — a later pass would believe a bound had been
        // established.
        //
        // A terminal session still proceeds. It never consults an environment, so its
        // answer does not depend on one being there.
        //
        // Read from the per-environment map rather than one global boolean, and absent
        // counts as unreachable: an environment nobody probed is one nobody asked.
        let reachable = report
            .environments_reachable
            .get(session_id.environment.as_str())
            .copied()
            .unwrap_or(false);
        if matches!(resolved, Resolved::Backed(_)) && !reachable {
            self.record_withheld(report, retention::WithheldReason::EnvironmentUnreachable);
            debug!(
                "Environment {} is unreachable; leaving {}'s history and watermark untouched",
                session_id.environment, encoded
            );
            return Ok(());
        }

        let outcome =
            retention::run_retention(&self.history, resolved.authority(), &session_id).await?;

        // **Read from the authority's own answer, not inferred from an absent bound.**
        // `confirmed_seq.is_none()` was what this used to branch on, and it cannot
        // separate a conservative answer from a question that was never put.
        match outcome.confirmed_seq {
            Some(_) => report.confirmed += 1,
            None => {
                let reason = outcome
                    .withheld
                    // An authority that confirmed nothing and gave no reason is a bug in
                    // that authority rather than a state to render, so it is recorded
                    // under the most conservative reason rather than dropped from the
                    // count — a withheld session missing from `withheld` would make the
                    // totals disagree with each other.
                    .unwrap_or(retention::WithheldReason::Ambiguous);
                self.record_withheld(report, reason);
            }
        }
        report.blocks_pruned += outcome.pruned;
        Ok(())
    }

    /// Count one withheld session, and why.
    ///
    /// One helper rather than two increments at each of the five sites, because the
    /// total and the per-reason breakdown must agree: a site that bumped `withheld` and
    /// forgot the reason would report a session as withheld for no reason at all, which
    /// is exactly the ambiguity the breakdown exists to remove.
    fn record_withheld(&self, report: &mut PassReport, reason: retention::WithheldReason) {
        report.withheld += 1;
        *report
            .withheld_reasons
            .entry(reason.as_str().to_string())
            .or_insert(0) += 1;
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
