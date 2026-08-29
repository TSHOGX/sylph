//! What is safe to drop, and who gets to say.
//!
//! Retention asks exactly one question and never asks how old anything is. A
//! time-based rule deletes data because a clock advanced, which is precisely the case
//! where the data may not exist anywhere else yet.
//!
//! The discipline is a Postgres replication slot's `restart_lsn` or a Kafka consumer
//! offset: the upstream keeps whatever any consumer might still need, so a lagging
//! consumer causes *growth* rather than loss. Growth is visible and fixable; loss is
//! neither.
//!
//! # The authority changed with the rewrite, and it changed in Sylph's favour
//!
//! Retention used to ask a *separate* archive whether it had ingested a session,
//! because that archive was a second copy fed by parsing native stores. With one store
//! and one vocabulary (`docs/architecture.md` § One authority) there is no second
//! Sylph-side copy to wait for — so the only remaining downstream copy is the
//! **vendor's own native store**, and only the environment holding it can say what it
//! still has.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info};

use crate::env::Environment;
use crate::storage::HistoryStore;
use sylph_protocol::SessionId;

/// What an environment reported about a session's native source.
///
/// Stored on the watermark row so a later reader can tell *why* a bound was or was not
/// established, which a bare `Option<i32>` cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSourceState {
    /// The environment answered and still holds the session.
    Present,
    /// The environment answered and does **not** hold it — the vendor deleted it, or
    /// never wrote it. The record then exists only here.
    Absent,
    /// Nobody answered. Distinct from [`Self::Absent`], and that distinction is the
    /// point: "the store does not have it" is a fact, while "no envagent is connected"
    /// is the absence of one.
    Unreachable,
}

impl NativeSourceState {
    /// The stored spelling, pinned here rather than derived from the variant names, so
    /// a rename cannot silently change what an existing journal row means.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::Unreachable => "unreachable",
        }
    }
}

/// Why nothing was confirmed.
///
/// **"Cannot ask" must never be recorded as "asked, and nothing is safe".** A watermark
/// row is a claim that a check happened, so an unreachable environment has to be
/// distinguishable from one that answered conservatively — writing a zero bound for the
/// first would let a later pass believe a bound was established.
///
/// This is why [`Confirmation`] carries a reason rather than only an `Option`. The
/// scheduler used to *infer* withholding from `confirmed_seq.is_none()`, which is
/// exactly the conflation `docs/storage.md` rejects: it cannot separate the two, and
/// they call for opposite responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithheldReason {
    /// No envagent is connected for this session's environment, so the question was
    /// never put.
    EnvironmentUnreachable,
    /// No native source exists at all, so there is nothing to wait for. Terminal:
    /// compact forever, prune never.
    NoNativeSource,
    /// The environment answered, but its record does not map cleanly onto an event — a
    /// store flushed mid-turn, or a file truncated mid-write, both real `pi` hazards.
    ///
    /// *Withhold rather than guess on any ambiguity* (`docs/storage.md`). A zero bound
    /// here would read as a completed check.
    Ambiguous,
}

impl WithheldReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnvironmentUnreachable => "environment_unreachable",
            Self::NoNativeSource => "no_native_source",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// What an authority confirmed, and what it was looking at when it did.
///
/// The provenance travels with the sequence number rather than being fetched
/// separately, because the two are one observation: a bound derived from one version of
/// a source, recorded beside a state read at another moment, would be a watermark that
/// cannot be checked against anything.
///
/// `archive_id` and `source_fingerprint` are gone with the separate archive. The first
/// held that store's own id for a session — the second id space the identity triple
/// deletes — and the second held its `file-v2:<sha256>:<bytes>:<mtime_ns>` over a
/// native file, read from a response nothing answers any more.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// Highest sequence number safe to drop. `None` means drop nothing.
    pub safe_to_drop: Option<i32>,
    /// What the environment reported about the native source, for the audit row.
    pub source: Option<NativeSourceState>,
    /// Why nothing was confirmed, when nothing was. Populated exactly when
    /// `safe_to_drop` is `None`.
    pub withheld: Option<WithheldReason>,
}

impl Confirmation {
    /// Nothing is safe to drop, with the reason **recorded rather than inferred**.
    pub fn nothing(source: Option<NativeSourceState>, withheld: WithheldReason) -> Self {
        Self {
            safe_to_drop: None,
            source,
            withheld: Some(withheld),
        }
    }
}

/// Who confirms that a session's history is safe to drop.
#[async_trait]
pub trait RetentionAuthority: Send + Sync {
    /// A name for the journal, so a stored watermark records what produced it.
    fn name(&self) -> &'static str;

    /// How far a downstream copy is confirmed to extend.
    async fn confirm(&self, session: &SessionId) -> Result<Confirmation>;
}

/// No downstream copy exists, so nothing is ever dropped.
///
/// The authority for a session whose vendor-native store is unreachable or gone — a
/// sandbox that no longer exists, an unpaired environment, a session the vendor never
/// wrote. There is no watermark to wait for and pruning would destroy the only copy.
/// Such a session is still *compacted* — deltas folded, blobs externalized, text
/// compressed — but never pruned.
///
/// It is also the default for anything unrecognized, because the safe direction to be
/// wrong in is keeping too much.
pub struct TerminalAuthority;

#[async_trait]
impl RetentionAuthority for TerminalAuthority {
    fn name(&self) -> &'static str {
        "terminal"
    }

    async fn confirm(&self, _session: &SessionId) -> Result<Confirmation> {
        Ok(Confirmation::nothing(
            Some(NativeSourceState::Absent),
            WithheldReason::NoNativeSource,
        ))
    }
}

/// The environment holding a session's native store confirms what it still has.
///
/// # A bound needs a native-to-sequence correspondence, and nothing holds one
///
/// An environment cannot report "I have ingested through event 400", because a vendor
/// CLI has no such notion. It can report what its own store holds, and one measured
/// property would make a bound derivable from that: a native store is written
/// **whole-message**, so its last record is complete and everything before it is too.
///
/// What is missing is the other half — which Sylph event a given native record produced.
/// That correspondence is not stored anywhere, and it cannot be recovered from
/// `sylph_protocol::Adapter::enumerate`: a [`sylph_protocol::NativeSummary`] describes a
/// session, not its events, and `event_count` is absent from all three adapters by
/// measurement rather than by omission — counting is a full parse, which is the cost
/// `enumerate` exists to avoid, and a record count is not an event count in any case
/// (one assistant record can hold several content blocks). A count those adapters
/// *estimated* would be worse than none: this is the one consumer that would subtract
/// from it.
///
/// So this answers [`WithheldReason::Ambiguous`] — **never a zero bound**, which would
/// read as a completed check. A pass records that it looked, keeps every block, and
/// `/diagnostics` reports the withholding rather than a silent zero. Pruning stays
/// unavailable until something durably records which event a native record produced;
/// until then growth is the deliberate direction, per this module's header.
pub struct EnvagentAuthority {
    environment: Arc<dyn Environment>,
}

impl EnvagentAuthority {
    pub fn new(environment: Arc<dyn Environment>) -> Self {
        Self { environment }
    }
}

#[async_trait]
impl RetentionAuthority for EnvagentAuthority {
    fn name(&self) -> &'static str {
        "envagent"
    }

    async fn confirm(&self, session: &SessionId) -> Result<Confirmation> {
        // The scheduler probes reachability **once per pass** and does not reach here
        // for a session whose environment it found unreachable — that is the layer that
        // owns the discipline, because an unreachable environment is one fact about the
        // pass rather than N session failures. This is the second line of defence for a
        // connection that dropped mid-pass.
        let health = self.environment.probe().await;
        if !health.available {
            return Ok(Confirmation::nothing(
                Some(NativeSourceState::Unreachable),
                WithheldReason::EnvironmentUnreachable,
            ));
        }

        debug!(
            "No native-to-sequence correspondence exists for {}, so no bound is derivable; keeping every block",
            session.encode()
        );
        Ok(Confirmation::nothing(
            Some(NativeSourceState::Present),
            WithheldReason::Ambiguous,
        ))
    }
}

/// Who confirms a given session, and why.
///
/// Returned rather than a bare `Box<dyn RetentionAuthority>` so the caller can report
/// *which* class a session fell into. A pass that prunes nothing is correct for a
/// terminal session and a symptom for a backed one, and without the class those two are
/// indistinguishable in a log.
pub enum Resolved {
    /// A reachable environment holds this session's native source.
    Backed(EnvagentAuthority),
    /// No reachable native source. Compact forever, prune never.
    Terminal(TerminalAuthority),
}

impl Resolved {
    pub fn authority(&self) -> &dyn RetentionAuthority {
        match self {
            Self::Backed(authority) => authority,
            Self::Terminal(authority) => authority,
        }
    }
}

/// Decide which authority governs a session.
///
/// # The boundary is reachability, never who started the session
///
/// `docs/storage.md` states it: whether a native source exists **and is reachable**.
/// The code this replaced read the *environment kind* — `matches!(kind, "local")` — and
/// derived an archive id from a native id, which is the "who started it" reading the
/// spec forbids. That failed in both directions: a sandbox session stayed prunable once
/// its store died with the sandbox, and reading it the other way leaves everything
/// terminal and prunes nothing, which is indistinguishable from retention working
/// correctly.
///
/// A session belongs to the environment named in its own identity, for life
/// (`docs/architecture.md` § Sessions do not migrate). So the only question is whether
/// *that* environment is one this server can currently reach — and because sessions do
/// not migrate, no other environment could answer for it even if it were connected.
pub fn resolve_authority(environment: &Arc<dyn Environment>, session: &SessionId) -> Resolved {
    let descriptor = environment.descriptor();
    if descriptor.id == session.environment.as_str() {
        Resolved::Backed(EnvagentAuthority::new(environment.clone()))
    } else {
        Resolved::Terminal(TerminalAuthority)
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
    session_id: &SessionId,
) -> Result<RetentionReport> {
    let confirmation = authority.confirm(session_id).await?;
    // `history.db` keys on the encoded triple, so the encoding happens once, here,
    // at the boundary between the two stores.
    let encoded = session_id.encode();

    history
        .set_watermark(&encoded, authority.name(), &confirmation)
        .await?;

    let mut report = RetentionReport {
        confirmed_seq: confirmation.safe_to_drop,
        withheld: confirmation.withheld,
        ..RetentionReport::default()
    };

    match confirmation.safe_to_drop {
        Some(seq) => {
            report.pruned = history.prune_below(&encoded, seq).await?;
            if report.pruned > 0 {
                info!(
                    "Pruned {} blocks from {} below seq {}",
                    report.pruned, encoded, seq
                );
            }
        }
        None => debug!(
            "Nothing confirmed for {} ({:?}); keeping every block",
            encoded,
            confirmation.withheld.map(WithheldReason::as_str)
        ),
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
    /// Why nothing was confirmed, when nothing was.
    ///
    /// **Reported rather than inferred**, which is the change this field is. The
    /// scheduler used to derive withholding from `confirmed_seq.is_none()`, and that
    /// cannot tell "the environment answered and nothing is safe yet" from "no envagent
    /// was connected, so nobody was asked" — the conflation `docs/storage.md` rejects,
    /// because those call for opposite responses.
    pub withheld: Option<WithheldReason>,
}

