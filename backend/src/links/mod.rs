//! The pool of live agent sessions, and what happens to what they say.
//!
//! A [`LiveSession`] is one agent held open **in process**. This module owns the set of
//! them, keyed by Sylph session id, and is the only thing that establishes or tears one
//! down.
//!
//! Its real job is being the single place where a session's output becomes durable
//! state. An event is journaled and published; a request is journaled *and*
//! published, because a request blocks the agent and therefore has to outlive
//! every client. Splitting that across the API handlers would mean each one
//! having to remember which of the two a given message was.
//!
//! Links are disposable on purpose (`docs/session-protocol.md`): they idle out,
//! they die with the server, and a turn on a detached session establishes a fresh
//! one by resuming the agent's native session id.
//!
//! # What the subprocess transport took with it
//!
//! This module used to hold an `adapter::Link`: a spawned `adapter.sh` speaking NDJSON on
//! stdio, with a handshake, correlation ids, and a reply timeout. All of that is gone —
//! an adapter is a Rust trait ([`sylph_protocol::Adapter`]) obtained from
//! [`crate::adapters::AdapterSource`], and what is left here is the pool and the drain.
//!
//! Three consequences worth naming, because each was a real behaviour rather than
//! plumbing:
//!
//! - **An exit code is not a concept an in-process adapter has.** There is no process of
//!   its own to exit, so `LinkEvent::Closed { exit_code, stderr_tail }` has no port. The
//!   stream simply ending is what a `Stream` already expresses, and a failure that needs
//!   explaining arrives as an `Err` item before it does — which is what
//!   [`Outcome::LinkLost`] now carries.
//! - **A refusal travels typed.** An adapter constructs
//!   [`sylph_protocol::AdapterError`] directly, so the `INVALID_VALUE`-versus-
//!   `UNSUPPORTED_FIELD` distinction can no longer be lost in framing. The string→code
//!   mapping that recovered it from a wire field is deleted, not moved.
//! - **A capability facet is merged here rather than inside the transport.** The v3 link
//!   applied facets before the server saw them; in-process that would put one rule in
//!   three adapters, so the facets travel and [`LiveSession`] is the one place
//!   [`sylph_protocol::CapabilityFacets::apply_to`] is called.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::adapters::AdapterSource;
use crate::db::JournalStore;
use crate::env::Environment;
use crate::events::{EventHub, SeqAllocator, StreamEvent};
use crate::models::PendingRequest;
use sylph_protocol::{
    AgentInfo, AgentName, CapabilityFacets, EnvironmentId, EventStream, Facet, Fidelity,
    LinkFacets, NativeId, RequestedNativeId, Resolution, RunSpec, Session, SessionCapabilities,
    SessionId, SessionMessage, TurnInput, UnavailableReason,
};

/// How long a link may sit unused before it is torn down.
///
/// A link costs a process and an agent's loaded context, and nothing is lost by
/// dropping it: history is journaled and the next turn resumes. Thirty minutes is
/// long enough that a user stepping away and returning finds the session still
/// warm, and short enough that a day of browsing does not accumulate processes.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How often the reaper looks for idle links.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// How long an agent may take to say what it is before it is called unusable.
///
/// Ten seconds, inherited from the deleted `HANDSHAKE_TIMEOUT` and for the same reason: a
/// CLI that cannot answer `--version` in that long is not going to run a turn. It is a
/// generous bound rather than a tight one because a cold vendor binary on a slow disk is a
/// real case, and the cost of being wrong is refusing a session that would have worked.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What starting a session needs to know.
///
/// A struct rather than seven arguments because they travel together and four of them
/// are `String` — the shape where swapping two compiles. `LinkConfig` is the adapter's
/// half of the same information; this is the server's, and it carries what only the
/// server decides (`inject_identity`, the parent) alongside what the client asked for.
#[derive(Debug, Clone)]
pub struct OpenSpec {
    /// Which machine runs it. Typed, so an unvalidated string cannot reach a primary
    /// key.
    pub environment: EnvironmentId,
    pub agent: String,
    /// **Already contained by the caller.** `api::sessions` canonicalizes and bounds
    /// this before it arrives, and the canonical spelling is what gets journalled —
    /// anything that later groups or compares a workdir has to use the same one.
    pub workdir: String,
    pub title: Option<String>,
    /// Whether to append `SOUL.md`. The server's decision, resolved before the spawn.
    pub inject_identity: bool,
    pub flags: HashMap<String, String>,
    /// The delegation parent or fork origin, as an encoded triple.
    pub parent_session: Option<String>,
}

/// A live link plus the bookkeeping the pool needs about it.
struct Entry {
    link: Arc<LiveSession>,
    /// Last time a turn or command touched this link, for idle reaping.
    last_used_ms: i64,
    /// The session's agent, so a caller need not re-read it to know what is
    /// attached.
    agent: String,
}

/// One agent held open in process, plus the surface the server built from it.
///
/// # Why this wraps [`Session`] rather than being it
///
/// Two facts the trait deliberately does not hold, both of which every caller in this
/// crate needs:
///
/// - **The merged capability surface.** [`SessionMessage::Capabilities`] carries a
///   *partial* and `docs/session-protocol.md` § Capability facets makes the server own the
///   merge, so there has to be somewhere the merged copy lives. A partial that could speak
///   for the whole surface erased a project's 46 slash commands on the first exchange of
///   every session; the merge existing exactly once is what makes that unreachable.
/// - **What the CLI reported about itself** ([`AgentInfo`]), which is where `flags` and
///   `supports` come from. `Session` answers about a session; this answers about the
///   agent, and `PATCH /sessions` validates a flag against it.
///
/// The session is boxed behind an `Arc` because the pool hands the same live session to
/// concurrent callers — a turn, a resolve arriving on another request, the reaper — and
/// `Session`'s methods take `&self` for exactly that reason.
pub struct LiveSession {
    session: Box<dyn Session>,
    /// What the agent's CLI reported about itself, read once on open.
    info: AgentInfo,
    /// The server's merged view, built from the surface the adapter answered on open and
    /// every facet it has sent since.
    surface: Mutex<SessionCapabilities>,
}

impl LiveSession {
    /// What the CLI reported about itself.
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// The id the vendor actually used.
    pub fn native_id(&self) -> &NativeId {
        self.session.native_id()
    }

    /// The server's merged capability surface.
    pub async fn capabilities(&self) -> SessionCapabilities {
        self.surface.lock().await.clone()
    }

    /// Fold one facet-scoped update into the merged surface, and answer with it.
    ///
    /// **Naming no facet is a no-op, never a clear.** An empty payload is an adapter
    /// saying nothing changed, and applying it as a whole surface is the erasure above.
    async fn apply(&self, facets: CapabilityFacets) -> Option<SessionCapabilities> {
        if facets.is_empty() {
            debug!("Ignoring a capability update naming no facet");
            return None;
        }
        let mut surface = self.surface.lock().await;
        facets.apply_to(&mut surface);
        Some(surface.clone())
    }

    /// Start a turn. Returns when the agent has **accepted** it, not when it has
    /// finished: completion arrives as an event.
    pub async fn send(&self, turn: TurnInput) -> Result<()> {
        self.session.send(turn).await
    }

    /// End the current turn, keep the session.
    ///
    /// **No local capability gate.** The adapter refuses with
    /// [`sylph_protocol::AdapterError`] when it does not offer
    /// [`sylph_protocol::Capability::Interrupt`], and it is the only thing that knows
    /// whether the vendor accepted — the same rule that forbids recording a `currentValue`
    /// from a request rather than from a confirmation. A second gate here would be one
    /// fact with two derivations, and the server's copy of `supports` is the stale one.
    pub async fn interrupt(&self) -> Result<()> {
        self.session.interrupt().await
    }

    /// Add text to a turn that is already running.
    pub async fn steer(&self, text: &str, expected_turn_id: Option<&str>) -> Result<()> {
        self.session.steer(text, expected_turn_id).await
    }

    /// Add text to run after the turn in flight.
    ///
    /// No turn precondition, unlike [`Self::steer`]: a queued follow-up is *for* after the
    /// turn, so a boundary crossing underneath it changes nothing.
    pub async fn queue(&self, text: &str) -> Result<()> {
        self.session.queue(text).await
    }

    /// Set one config option, and answer with the refreshed surface.
    ///
    /// The adapter answers with the refreshed **array** rather than an acknowledgement,
    /// because setting one option can change another's choices: `pi`'s thinking levels
    /// depend on the selected model, and `claude` carries per-model effort levels. Folded
    /// in as the `config` facet, so one merge rule governs this path and the streamed one.
    pub async fn set_config(
        &self,
        config_id: &str,
        value: serde_json::Value,
    ) -> Result<SessionCapabilities> {
        let config = self.session.set_config(config_id, value).await?;
        let mut surface = self.surface.lock().await;
        CapabilityFacets {
            config: Some(config),
            ..Default::default()
        }
        .apply_to(&mut surface);
        Ok(surface.clone())
    }

    /// Answer a request this session raised.
    pub async fn resolve(&self, resolution: Resolution) -> Result<()> {
        self.session.resolve(resolution).await
    }

    /// Ask the agent to exit, and make sure it did.
    ///
    /// Idempotent by contract, because the idle reaper, an explicit detach, and shutdown
    /// can all reach it. A failure is logged rather than propagated: every caller is
    /// tearing down and has nothing to do differently, and a `close` that errors must
    /// still remove the entry from the pool.
    pub async fn close(&self) {
        if let Err(error) = self.session.close().await {
            warn!("Closing a session's agent reported: {:#}", error);
        }
    }
}

/// What a client can be told about a session's link without holding one.
///
/// # Two typed facets, not a boolean
///
/// `docs/architecture.md` § One link, two typed capability facets. The previous shape was
/// `attached: bool` plus an optional failure, and it had two defects that this replaces
/// rather than refines.
///
/// **An unexplained `false` was the original bug.** The server logged the reason and
/// returned a bare `attached: false`, so a client could not distinguish a failed handshake
/// from an agent that simply declares no commands — and the only honest thing it could
/// render was nothing at all. That is how a stale install presented as a composer ignoring
/// the `/` key, three layers from the cause.
///
/// **And one boolean cannot describe a session running under someone else.** A Sylph-born
/// session with no link has no process; a session a human started in a terminal has one
/// that is alive and simply not ours. Both reported `attached: false`, so an empty
/// capability surface meant "Sylph cannot reach it" and "there is nothing there"
/// interchangeably — opposite facts. Splitting observe from control is what makes
/// observable-but-undrivable a *state* rather than a failure, and a third value of
/// `attached` could not have done it: *measured 2026-08-27*, the vendors do not sort into
/// three buckets — a terminal `pi` is observable and undrivable, `codex` is both, `claude`
/// is observable and semi-drivable behind a human approval gate — so a third variant would
/// have to pick one and misreport the rest.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    /// Can Sylph see this session, and can it drive it.
    pub link: LinkFacets,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// What this agent can actually do, so a UI never offers a control that would fail.
    ///
    /// Empty when control is unavailable: capability is a property of a live link, and
    /// reporting a remembered list would promise interrupt on a session Sylph holds no
    /// channel to. Read alongside `link.control.reason`, which is what says *why* — the
    /// two together are what the empty list could never say alone.
    pub supports: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<SessionCapabilities>,
}

/// The session names an agent whose CLI is not on this machine.
///
/// A distinct error type rather than a message to match on, so classification survives the
/// wording being edited — recovered by `downcast_ref`, never by parsing text back out.
///
/// **Renamed from `NoAdapter` with the transport, and the rename is a narrowing of
/// meaning.** There is no adapter to be absent: adapters are linked in, so availability is
/// whether the vendor CLI is on that machine's `PATH` — one question, one answer, one fix.
/// It is the link failure a retry cannot change, which is why a client must be able to
/// tell it apart: `sylph_protocol::UnavailableReason::is_retryable` declares that rule so
/// there is one answer rather than a condition buried in a query's `staleTime`.
#[derive(Debug, thiserror::Error)]
#[error("Agent '{0}' is not available on this machine")]
pub struct NoAgent(pub String);

impl LinkState {
    /// No link, and nobody has tried to establish one.
    ///
    /// **Observe is available**: history answers from the server's own store whether or
    /// not a link exists, which is the entire reason the two facets are separate. Control
    /// reports `environment_unreachable` — the honest answer for a session whose agent
    /// this server is not currently holding.
    fn detached() -> Self {
        Self {
            link: LinkFacets {
                observe: Facet::available(),
                control: Facet::unavailable(UnavailableReason::EnvironmentUnreachable),
            },
            agent: None,
            supports: Vec::new(),
            capabilities: None,
        }
    }

    /// Establishing a link was tried and failed, for a stated reason.
    fn failed(reason: UnavailableReason) -> Self {
        Self {
            link: LinkFacets {
                // Still readable: the record is the server's, and a failure to reach the
                // agent does not take the transcript with it.
                observe: Facet::available(),
                control: Facet::unavailable(reason),
            },
            ..Self::detached()
        }
    }
}

#[derive(Clone)]
pub struct LinkManager {
    store: JournalStore,
    /// The batching front for the journal, used by the drain path.
    ///
    /// Deltas arrive here thousands per turn, so this is the one write path where
    /// batching matters. Held beside `store` rather than replacing it: the manager
    /// also does ordinary single-row work (persisting a native id, a pending
    /// request) that has nothing to batch.
    writer: crate::db::JournalWriter,
    hub: EventHub,
    /// Held for `descriptor` and `probe`, which is what the session's environment id and
    /// per-environment health are read from. It no longer spawns anything: an adapter owns
    /// whatever process its vendor CLI needs (`crates/adapters/src/process.rs`).
    #[allow(dead_code)]
    environment: Arc<dyn Environment>,
    /// Where an adapter comes from.
    ///
    /// The field this replaces held **adapter roots** — a list of directories to join an
    /// agent name onto in search of an `adapter.sh`. That was the shape of a bug this repo
    /// had already paid for once (an agent found in one root, reported missing from
    /// another), and it is gone rather than fixed: a trait object has no path to get
    /// wrong. `crate::adapters` holds the seam, and step 4 replaces this one field with a
    /// per-environment source dialled in from an envagent.
    adapters: Arc<dyn AdapterSource>,
    soul_path: Option<PathBuf>,
    seq: SeqAllocator,
    /// Turns waiting to be told how they ended.
    ///
    /// A turn's completion arrives here as an event, and the supervisor is what
    /// records terminal state. Rather than call back into it — which would be a
    /// cycle, since it calls `ensure` — the drain task resolves the channel the
    /// supervisor is parked on. Set once at startup.
    waiters: Arc<RwLock<Option<crate::supervisor::turn::TurnWaiters>>>,
    /// Records terminal state for a turn no waiter exists for.
    ///
    /// An agent-initiated turn has no waiter by definition — nobody asked for it, so
    /// nothing is parked on its outcome — and resolving a channel is therefore not
    /// enough to settle one. This is the supervisor's own settling path, factored out
    /// so both can reach it without the cycle a direct call back would create
    /// (`supervisor::closer`). Set once at startup, like `waiters`.
    closer: Arc<RwLock<Option<crate::supervisor::closer::TurnCloser>>>,
    /// Applies a delegation's late outcome to the turn that owns it.
    ///
    /// A trajectory id is session-scoped, so an outcome arriving on a later turn is
    /// about an earlier turn's record (`crate::supervisor::reconcile`). Reaching the
    /// storage tier from here is what that needs, and it is held the same way
    /// `closer` is because the two are wired at the same point for the same reason:
    /// the drain task is where these events land.
    reconciler: Arc<RwLock<Option<crate::supervisor::TrajectoryReconciler>>>,
    /// Keyed by the **encoded** triple rather than by [`SessionId`].
    ///
    /// A `HashMap<SessionId, _>` would work and is worse here: every caller reaches
    /// this map from a path segment that is already a string, so keying on the type
    /// would mean decoding and re-encoding around each lookup. The encoding has one
    /// producer and one reader (`SessionId::encode`), so the two spellings cannot
    /// diverge.
    links: Arc<RwLock<HashMap<String, Entry>>>,
    /// Serializes establishment per session.
    ///
    /// Two turns arriving together on a detached session would otherwise both
    /// see "no link", both spawn an adapter, and both resume the same agent
    /// session — two processes writing one native store, which corrupts it.
    establishing: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl LinkManager {
    pub fn new(
        store: JournalStore,
        writer: crate::db::JournalWriter,
        hub: EventHub,
        environment: Arc<dyn Environment>,
        adapters: Arc<dyn AdapterSource>,
        soul_path: Option<PathBuf>,
        seq: SeqAllocator,
    ) -> Self {
        let manager = Self {
            store,
            writer,
            hub,
            environment,
            adapters,
            soul_path,
            seq,
            waiters: Arc::new(RwLock::new(None)),
            closer: Arc::new(RwLock::new(None)),
            reconciler: Arc::new(RwLock::new(None)),
            links: Arc::new(RwLock::new(HashMap::new())),
            establishing: Arc::new(Mutex::new(HashMap::new())),
        };
        manager.spawn_reaper();
        manager
    }

    /// Hand the manager the supervisor's waiter registry.
    ///
    /// Wired after construction because the two reference each other: the
    /// supervisor asks the manager for a link, and the manager settles the
    /// supervisor's turns. One of the two edges has to be late, and this is the
    /// cheaper one — a manager with no waiters yet simply cannot settle a turn,
    /// and no turn exists before the supervisor does.
    pub async fn set_waiters(&self, waiters: crate::supervisor::turn::TurnWaiters) {
        *self.waiters.write().await = Some(waiters);
    }

    /// Hand the manager the settling path for turns with no waiter.
    ///
    /// Late for the same reason as `set_waiters`, and safe for the same reason: an
    /// agent cannot have started a turn before the thing that settles turns exists.
    pub async fn set_closer(&self, closer: crate::supervisor::closer::TurnCloser) {
        *self.closer.write().await = Some(closer);
    }

    /// Hand the manager the path that records a delegation's late outcome.
    ///
    /// Late for the same reason as the two above. Absent, the only cost is that a
    /// cross-turn outcome is not reconciled — the event itself is still journaled and
    /// published — so this cannot wedge anything if wiring is ever missed.
    pub async fn set_reconciler(&self, reconciler: crate::supervisor::TrajectoryReconciler) {
        *self.reconciler.write().await = Some(reconciler);
    }

    /// Tell a waiting turn how it ended.
    async fn settle_turn(
        &self,
        session_id: &SessionId,
        turn_id: &str,
        outcome: crate::supervisor::turn::Outcome,
    ) {
        // Cloned out from under the read guard, so the guard is not held across
        // the `lock().await` below — holding both would order two locks and
        // invite a deadlock the moment anything takes them the other way.
        let waiters = self.waiters.read().await.clone();
        let waiting = match waiters {
            // Taken out of the map in its own statement so the guard drops before the
            // send. An `if let` here would hold the lock across it, and the receiving
            // supervisor may re-enter this manager while draining.
            Some(waiters) => waiters.lock().await.remove(turn_id),
            None => None,
        };

        if let Some(tx) = waiting {
            // A send error means the supervisor stopped waiting, which happens
            // when a stop already settled the turn. Benign.
            let _ = tx.send(outcome);
            return;
        }

        // No waiter. Two very different reasons for that, and settling both here is a
        // bug: an agent-initiated turn never had one, while a *user's* turn whose
        // waiter is already gone is one the supervisor has claimed and is about to
        // settle itself.
        //
        // Claiming the waiter first is how a stop wins over the link teardown it
        // causes (see `TurnSupervisor::settle`). Settling every waiter-less turn here
        // broke that: a stop tore down the link, `handle_close` raised `LinkLost`, and
        // this path recorded it before the stop's own `cancelled` arrived — so a
        // deliberate stop was reported as the agent having exited. The old early
        // return was accidentally right about user turns; `origin` is what makes it
        // deliberate.
        match self.store.get_turn(turn_id).await {
            Ok(Some(turn)) if turn.is_agent_initiated() => {}
            Ok(_) => return,
            Err(error) => {
                error!("Could not read turn {} to settle it: {}", turn_id, error);
                return;
            }
        }

        // An agent-initiated turn is settled through the same closer the supervisor
        // uses, so it consolidates, closes what it left open, and returns the session
        // to idle by exactly the path a user's turn does. Left unsettled it would sit
        // `running` forever, and the session with it: the composer refuses a message
        // while a turn is live, so one unprompted turn would wedge the session.
        if let Some(closer) = self.closer.read().await.clone() {
            closer.close(session_id, turn_id, outcome).await;
        } else {
            // Only reachable before wiring completes, which is startup-order rather
            // than a runtime condition — but a silent skip here is a wedged session,
            // so it is worth a line.
            error!(
                "Turn {} completed with no waiter and no closer; it stays running",
                turn_id
            );
        }
    }

    /// Tear down links nobody has used for [`IDLE_TIMEOUT`].
    fn spawn_reaper(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(REAP_INTERVAL);
            loop {
                ticker.tick().await;
                let cutoff = now_ms() - IDLE_TIMEOUT.as_millis() as i64;

                // Collected under a read lock and closed outside it: `close`
                // awaits a grace period, and holding the map through that would
                // stall every other session's turn.
                let idle: Vec<String> = {
                    let links = manager.links.read().await;
                    links
                        .iter()
                        .filter(|(_, entry)| entry.last_used_ms < cutoff)
                        .map(|(id, _)| id.clone())
                        .collect()
                };

                for encoded in idle {
                    // The pool is keyed by the encoded triple, so reaching the store
                    // needs a decode. A key that will not decode cannot name a row and
                    // cannot be reaped through the store, so it is dropped from the
                    // pool directly rather than left to accumulate — it is unreachable
                    // either way, and leaving it would leak the process behind it.
                    let Ok(session_id) = SessionId::decode(&encoded) else {
                        error!("Dropping unparseable link key {}", encoded);
                        manager.links.write().await.remove(&encoded);
                        continue;
                    };
                    // A turn may have started between the scan and here, so the busy
                    // check is repeated rather than trusted from the scan.
                    if manager.store.session_status(&session_id).await.as_deref() == Some("running")
                    {
                        continue;
                    }
                    info!("Reaping idle link for session {}", encoded);
                    manager.detach(&session_id, "idle").await;
                }
            }
        });
    }

    /// The link for a session, or `None` if it is detached.
    pub async fn get(&self, session_id: &SessionId) -> Option<Arc<LiveSession>> {
        let mut links = self.links.write().await;
        let entry = links.get_mut(&session_id.encode())?;
        entry.last_used_ms = now_ms();
        Some(entry.link.clone())
    }

    pub async fn state(&self, session_id: &SessionId) -> LinkState {
        let links = self.links.read().await;
        let Some(entry) = links.get(&session_id.encode()) else {
            return LinkState::detached();
        };
        LinkState {
            link: LinkFacets {
                observe: Facet::available(),
                control: Facet::available(),
            },
            agent: Some(entry.agent.clone()),
            supports: entry
                .link
                .info()
                .supports
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            capabilities: Some(entry.link.capabilities().await),
        }
    }

    /// The link state for a session whose establishment just failed.
    ///
    /// Classification happens here rather than in the handler because this module
    /// is what knows the difference: a name the registry does not hold can only be
    /// fixed by installing an adapter, while a handshake that timed out may work on
    /// the next try. Collapsing both into one error would make the UI offer a retry
    /// button for a condition no retry can change.
    pub async fn state_after_failure(
        &self,
        session_id: &SessionId,
        error: &anyhow::Error,
    ) -> LinkState {
        // A link may have been established concurrently — by a turn, or by another
        // client — between the failure and this call. The live answer wins.
        let live = self.state(session_id).await;
        if live.link.control.available {
            return live;
        }

        // Downcast rather than matching the message text: the reason a client sees must
        // not depend on the wording of a log line, which is edited freely. Same rule as
        // the adapter refusal codes — the text is the producer's and may be reworded,
        // while the code is declared contract.
        let reason = if error.downcast_ref::<NoAgent>().is_some() {
            UnavailableReason::NoAgent
        } else {
            UnavailableReason::HandshakeFailed
        };
        LinkState::failed(reason)
    }

    pub async fn attached_count(&self) -> usize {
        self.links.read().await.len()
    }

    /// The link for a session, establishing one if needed.
    ///
    /// This is the path every turn takes. Establishment is serialized per session,
    /// and the check is repeated inside the lock: the first caller through creates
    /// the link, and the second finds it rather than making a second one.
    pub async fn ensure(&self, session_id: &SessionId) -> Result<Arc<LiveSession>> {
        if let Some(link) = self.get(session_id).await {
            return Ok(link);
        }

        let gate = {
            let mut establishing = self.establishing.lock().await;
            establishing
                .entry(session_id.encode())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = gate.lock().await;

        // Whoever held the gate before us may have established it.
        if let Some(link) = self.get(session_id).await {
            return Ok(link);
        }

        self.establish(session_id).await
    }

    /// Re-attach to a session the journal already holds.
    ///
    /// Distinct from [`Self::open_session`], and the difference is which way identity
    /// flows. Here the id is **known** — it is the row's primary key — so the agent is
    /// asked to resume it. There, no id exists yet and the agent's answer is what
    /// creates one.
    async fn establish(&self, session_id: &SessionId) -> Result<Arc<LiveSession>> {
        let session = self
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow!("Unknown session {}", session_id.encode()))?;

        let flags: BTreeMap<String, String> = serde_json::from_str::<serde_json::Value>(
            &session.flags_json,
        )
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(|map| {
            map.into_iter()
                .filter_map(|(k, v)| {
                    v.as_str()
                        .map(|s| (k.clone(), s.to_string()))
                        .or_else(|| Some((k, v.to_string())))
                })
                .collect()
        })
        .unwrap_or_default();

        info!(
            "Establishing link for session {} ({})",
            session_id.encode(),
            session.id.agent
        );

        let (live, events) = self
            .run(session.id.agent.as_str(), |_| RunSpec {
                workdir: session.workdir.clone(),
                // Resume the id the row is keyed by, whatever the agent's preference is:
                // this session exists and its id is the primary key, so there is nothing to
                // dictate and nothing to assign. There is no second id space to reconcile.
                //
                // `Resume` rather than `Dictate`, and the distinction is load-bearing rather
                // than cosmetic: `codex` spells the two differently (`resume <id>` against
                // nothing), so an adapter handed one variant for both cases cannot choose
                // the right command line.
                native_id: RequestedNativeId::Resume(session.id.native_id.clone()),
                inject_identity: session.inject_identity,
                identity_preamble: self.identity_preamble(session.inject_identity),
                flags,
                permission_mode: None,
            })
            .await
            .with_context(|| {
                format!(
                    "Failed to establish a link for session {}",
                    session_id.encode()
                )
            })?;

        // **Nothing is persisted here, and that is the point of the rewrite.** This
        // branch used to learn the agent-native id after the row existed and write it
        // back — the whole purpose of the deleted `set_backend_session_id`. Under the
        // identity triple the id is the primary key, so there is nothing to learn and
        // nothing that could disagree.
        //
        // A vendor answering with a *different* id than the one asked for would be a
        // resumed session becoming a different session, which the type system cannot
        // catch and the journal must not absorb silently.
        if live.native_id() != &session.id.native_id {
            warn!(
                "Session {} asked agent {} to resume '{}' and it reported '{}'; \
                 the journal keeps the id it was keyed by",
                session_id.encode(),
                session.id.agent,
                session.id.native_id,
                live.native_id()
            );
        }

        self.insert(session_id, live.clone(), events).await;
        Ok(live)
    }

    /// Open one agent in process, and take its stream.
    ///
    /// The one place [`sylph_protocol::Adapter::run`] is called, shared by
    /// [`Self::establish`] and [`Self::open_session`] so the two cannot disagree about
    /// what a freshly opened session knows about itself.
    ///
    /// # An absent agent is a typed failure, not a message
    ///
    /// [`NoAgent`] is what a client renders as `UnavailableReason::NoAgent`, the reason no
    /// retry can fix, and it is recovered by `downcast_ref` rather than by matching text.
    /// This is now the *only* way an agent can fail to start for want of an adapter: there
    /// is no second question about a path, so a listed agent and a startable one are the
    /// same fact.
    ///
    /// # `events` is taken here, once
    ///
    /// [`Session::events`] yields its stream once by contract — two consumers of one
    /// vendor's stdio would each see half the turn — so it is taken before the session is
    /// shared behind an `Arc`, which is also the only point at which `&mut self` is
    /// available. A session that answers `None` has already been drained, which for a
    /// freshly opened one is an adapter bug rather than a state to tolerate.
    /// # The spec is built from the probe, not before it
    ///
    /// `spec` is a closure rather than a value because one of its fields is only decidable
    /// once the agent has said what it is: [`AgentInfo::dictates_native_id`] is what
    /// separates "open a new session under this id" from "assign one and report it", and
    /// `codex` spells those differently on its command line. Asking for a dictated id
    /// unconditionally happened to work — an adapter that cannot accept one ignores it and
    /// reports its own — but it put a value on the wire that the agent was known not to
    /// honour, which is the shape of a fact with two answers.
    async fn run(
        &self,
        agent: &str,
        spec: impl FnOnce(&AgentInfo) -> RunSpec,
    ) -> Result<(Arc<LiveSession>, EventStream)> {
        let adapter = self
            .adapters
            .for_agent(agent)
            .ok_or_else(|| anyhow::Error::new(NoAgent(agent.to_string())))?;

        // Before `run`, because it is what says whether this CLI is runnable at all and
        // its `flags` are what `PATCH /sessions` validates against. An agent whose binary
        // is on `PATH` but will not answer is a start failure, reported here rather than
        // as an empty flag schema three layers later.
        // Bounded, and the bound is what the deleted transport used to provide.
        //
        // `Link::establish` capped its `hello` at ten seconds on the reasoning that an
        // adapter which cannot say who it is in that long will not run a turn. Deleting the
        // subprocess deleted the cap with it, and nothing downstream replaced it: `probe`
        // spawns `<cli> --version` and awaits it with no deadline of its own, so a wedged
        // vendor binary — one waiting on a lock, a login prompt, or a dead network mount —
        // would hang `POST /sessions` for as long as the client waited rather than failing.
        //
        // Bounded here rather than inside each adapter for the reason a containment rule is
        // written once: three implementations of "how long is too long" would disagree, and
        // this is the one call site that has a *caller* waiting on the answer. The
        // per-command 30s bound did survive the deletion, one layer down in
        // `crates/adapters/src/process.rs::REPLY_TIMEOUT` — this is the half that had no
        // owner left.
        let info = match tokio::time::timeout(PROBE_TIMEOUT, adapter.probe()).await {
            Ok(probed) => {
                probed.with_context(|| format!("Agent '{agent}' would not report what it is"))?
            }
            Err(_) => {
                return Err(anyhow!(
                    "Agent '{agent}' did not report what it is within {PROBE_TIMEOUT:?}; \
                     its CLI is installed but not answering"
                ))
            }
        };

        let mut session = adapter.run(spec(&info)).await?;
        let events = session
            .events()
            .ok_or_else(|| anyhow!("Agent '{agent}' opened a session with no event stream"))?;

        // The surface the adapter answers on open, which is what a composer renders before
        // the first turn. Read from the vendor rather than assembled here: the adapter is
        // the only thing that knows whether a model was honoured, and a session can be
        // steered by the agent's own TUI besides.
        let surface = session.capabilities().await.unwrap_or_else(|error| {
            // Degraded rather than fatal: a session whose surface could not be read is
            // still a session whose transcript is readable and whose turns run. The
            // composer renders an empty surface, which `LinkState.supports` and
            // `link.control.reason` together explain — the alternative is refusing to open
            // a session because one query failed.
            warn!("Agent '{agent}' opened a session but would not report its capabilities: {error:#}");
            SessionCapabilities::default()
        });

        Ok((
            Arc::new(LiveSession {
                session,
                info,
                surface: Mutex::new(surface),
            }),
            events,
        ))
    }

    /// The identity text to inject, or `None`.
    ///
    /// # Content, not a path, and read per launch
    ///
    /// The file lives beside the server and the adapter runs wherever the agent does, so a
    /// path would name a file the adapter cannot open — which is why
    /// [`RunSpec::identity_preamble`] carries the text. Read per launch rather than cached
    /// at startup so editing `SOUL.md` takes effect on the next session rather than on the
    /// next restart.
    ///
    /// A configured-but-unreadable file resolves to `None`, matching what [`crate::config`]
    /// already does at resolution time: reporting a configured-but-absent file as present
    /// would let the UI promise an injection that never happens.
    fn identity_preamble(&self, inject: bool) -> Option<String> {
        if !inject {
            return None;
        }
        let path = self.soul_path.as_ref()?;
        match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => Some(text),
            Ok(_) => None,
            Err(error) => {
                // Worth a line at `warn`: identity was asked for and is not being
                // injected, which is a difference a reader of the transcript cannot see.
                warn!(
                    "Identity was requested but {} could not be read: {}",
                    path.display(),
                    error
                );
                None
            }
        }
    }

    /// Register a live link and start draining it.
    ///
    /// Shared by [`Self::establish`] and [`Self::open_session`] so the pool insert and
    /// the drain spawn cannot get out of step: a link in the map with nothing reading
    /// it wedges the session, and a drain with no map entry publishes events for a
    /// session nothing can find.
    ///
    /// The map write is awaited rather than spawned. A detached insert would return a
    /// link the pool does not yet hold, so the caller's own next `get` — and every
    /// concurrent one — could miss and establish a *second* adapter against one
    /// session, which is two processes writing one native store.
    async fn insert(&self, session_id: &SessionId, link: Arc<LiveSession>, events: EventStream) {
        let encoded = session_id.encode();
        self.links.write().await.insert(
            encoded.clone(),
            Entry {
                link,
                last_used_ms: now_ms(),
                agent: session_id.agent.as_str().to_string(),
            },
        );
        self.spawn_drain(encoded, events);
    }

    /// Start an agent, learn the id it assigned, and journal the session.
    ///
    /// # This is `POST /sessions`, and the order is the whole design
    ///
    /// A session has no identity until an agent has given it one
    /// (`docs/architecture.md` § Identity), so the row cannot come first. The previous
    /// shape minted a `Uuid::new_v4()`, wrote a row, and left the agent unstarted —
    /// which produced a **class of session with no work in it**, and three things go
    /// away with it: `DELETE /sessions/{id}` for empties, the frontend probe that
    /// called it on unmount, and the window in which a session existed with no native
    /// id and therefore could not be resumed.
    ///
    /// So: spawn, read the id off the handshake, then insert. A failed spawn reports at
    /// the start screen instead of leaving a dead row behind.
    ///
    /// # Why the insert can still fail, and why that is a conflict rather than a retry
    ///
    /// Two clients can ask for a session in the same directory at the same moment, and
    /// a vendor that was handed an id it already holds will happily resume it. The
    /// primary key is what refuses the second row, and the error is a genuine `409`:
    /// two rows for one conversation is exactly the state it exists to prevent. The
    /// link is closed on that path rather than leaked, because a process nobody holds
    /// a row for cannot be reached again.
    ///
    /// # Ordering on the failure path
    ///
    /// The link is registered **after** the insert commits. Registering first would
    /// leave a live entry in the pool keyed to a session the journal does not have, so
    /// a caller could find a link, send a turn, and have every event dropped for
    /// naming a session that does not exist.
    pub async fn open_session(&self, spec: OpenSpec) -> Result<crate::models::Session> {
        info!(
            "Starting {} in {} to open a session",
            spec.agent, spec.workdir
        );

        // The agent is validated before the id is minted, because minting one for an agent
        // that cannot run would put a `Dictate` on the wire for nothing — and the parse is
        // the one failure here that is about the *request* rather than about the machine.
        let agent = AgentName::parse(spec.agent.clone())?;

        let (live, events) = self
            .run(&spec.agent, |info| RunSpec {
                workdir: spec.workdir.clone(),
                // A fresh session, so the id is **dictated where the agent accepts one**:
                // the server mints it and hands it over, which is what makes identity known
                // before the first event and is the path `claude` and `pi` take. `codex`
                // assigns its own and reports it, so it gets `New` — and `native_id()`
                // below is *read* either way rather than assumed, because that is the only
                // value this can trust.
                native_id: if info.dictates_native_id {
                    RequestedNativeId::Dictate(NativeId::new(uuid::Uuid::new_v4().to_string()))
                } else {
                    RequestedNativeId::New
                },
                inject_identity: spec.inject_identity,
                identity_preamble: self.identity_preamble(spec.inject_identity),
                flags: spec.flags.clone().into_iter().collect(),
                permission_mode: None,
            })
            .await
            .with_context(|| format!("Failed to start {} in {}", spec.agent, spec.workdir))?;

        // **An `open` that reports no id is a failure, not a degraded success.**
        // Without one the session can never be resumed, so every link death — the idle
        // reaper, a restart, an explicit detach — would make it permanently
        // unlinkable. Better to refuse now, with the agent torn down, than to journal a
        // row nothing can ever reach again.
        //
        // The type makes an *absent* id unrepresentable, which is why this checks for an
        // empty one: `NativeId` is a newtype over `String`, so a blank is the remaining way
        // an adapter can answer with nothing while satisfying the signature.
        let native = live.native_id().clone();
        if native.as_str().trim().is_empty() {
            live.close().await;
            return Err(anyhow!(
                "Agent '{}' started but reported no session id, so the session \
                 could never be resumed",
                spec.agent
            ));
        }

        let session_id = SessionId::new(spec.environment.clone(), agent, native);

        let now = now_ms();
        let record = crate::models::Session {
            id: session_id.clone(),
            workdir: spec.workdir,
            title: spec.title,
            inject_identity: spec.inject_identity,
            // A live link holds the process's own stdio, which is the only source that
            // carries every delta and its arrival time.
            fidelity: Fidelity::Stream,
            flags_json: serde_json::to_string(&spec.flags).unwrap_or_else(|_| "{}".to_string()),
            status: "idle".to_string(),
            parent_session: spec.parent_session,
            created_at_ms: now,
            updated_at_ms: now,
        };

        let stored = match self.store.create_session(&record).await {
            Ok(stored) => stored,
            Err(error) => {
                // The row is what makes the process reachable, so a refused insert means
                // this link can never be found again.
                live.close().await;

                // A duplicate triple is a **conflict, not a start failure**. The primary
                // key refusing a second row for one conversation is the constraint doing
                // its job, and it is reachable whenever a vendor is handed an id it
                // already holds: two clients starting a session in one directory at the
                // same moment, or an adapter that dictates a fixed id. Reported as a
                // generic start failure it reads as "the agent is broken", which sends
                // the investigation to the agent install rather than to the collision.
                if let crate::error::Error::Database(sqlx::Error::Database(db)) = &error {
                    if db.is_unique_violation() {
                        return Err(anyhow::Error::new(
                            crate::error::Error::SessionAlreadyPresent,
                        ));
                    }
                }
                return Err(error.into());
            }
        };

        self.insert(&session_id, live, events).await;
        info!("Opened session {}", session_id.encode());
        Ok(stored)
    }

    /// Consume a link's output for as long as it lives.
    ///
    /// One task per link, and the only consumer: the channel is not a broadcast,
    /// so a second reader would steal messages rather than duplicate them.
    /// # The stream ending *is* the close
    ///
    /// There is no `Closed` message to wait for, because an in-process adapter has no exit
    /// code to report: the `Stream` finishing is what the transport used to say with one.
    /// So the close is handled after the loop rather than inside it, which also means it
    /// cannot be missed — the previous shape `break`ed on a message that a dropped sender
    /// would never deliver.
    ///
    /// # An `Err` item is a read failure, not the session ending
    ///
    /// A truncated native file or an unparseable vendor payload arrives as `Err` and the
    /// stream **may continue**: `pi`'s `_rewriteFile()` truncate-rewrites with no lock, so
    /// a reader can observe an empty or half-written file, and the correct response is to
    /// keep reading rather than to declare the session over. The last one is kept as the
    /// detail on [`Outcome::LinkLost`], which is what replaces the stderr tail — without
    /// it a turn cut short says only that the agent stopped, with no reason at all.
    fn spawn_drain(&self, encoded: String, mut events: EventStream) {
        let manager = self.clone();
        tokio::spawn(async move {
            // Decoded once, here, rather than per event. Every entry in the pool was
            // keyed from a `SessionId::encode`, so a failure is a corrupt pool rather
            // than a bad request — and draining a link whose session cannot be named
            // would journal events against nothing.
            let session_id = match SessionId::decode(&encoded) {
                Ok(session_id) => session_id,
                Err(error) => {
                    error!("Cannot drain link for {}: {}", encoded, error);
                    return;
                }
            };
            let session_id = &session_id;
            let mut last_error: Option<String> = None;
            while let Some(message) = events.next().await {
                match message {
                    Ok(SessionMessage::Event(event)) => {
                        manager.absorb_event(session_id, event).await;
                    }
                    Ok(SessionMessage::Request(request)) => {
                        manager.absorb_request(session_id, request).await;
                    }
                    Ok(SessionMessage::Capabilities(facets)) => {
                        // The merge lives in one place (`LiveSession::apply`), and only a
                        // surface that actually changed is published: a facet-less update
                        // is an adapter saying nothing changed, and republishing it would
                        // make every client re-render for no reason.
                        let Some(link) = manager.get(session_id).await else {
                            debug!("Capability update for a session with no link; dropping");
                            continue;
                        };
                        if let Some(surface) = link.apply(facets).await {
                            // Published rather than journaled: it is current state, not
                            // something that happened, and the link holds it for whoever
                            // asks later.
                            manager
                                .publish_session_event(
                                    session_id,
                                    "capabilities",
                                    serde_json::to_value(&surface).unwrap_or_default(),
                                )
                                .await;
                        }
                    }
                    Err(error) => {
                        warn!("Failed to read from session {}: {:#}", encoded, error);
                        last_error = Some(format!("{error:#}"));
                    }
                }
            }
            manager.handle_close(session_id, last_error).await;
            debug!("Drain finished for session {}", encoded);
        });
    }

    /// Journal a normalized event and publish it to attached clients.
    ///
    /// A `turn` event with `phase: completed` is also the signal that settles the
    /// waiting turn, because on a link a turn's outcome is an event rather than a
    /// process exit.
    async fn absorb_event(&self, session_id: &SessionId, event: sylph_protocol::Event) {
        let turn_id = match self.store.running_turn_id(session_id).await {
            Ok(Some(id)) => id,
            // No turn is running, and the agent is speaking anyway. That used to be
            // treated as impossible and the event was dropped — the reasoning being
            // that a guessed turn id puts an event in the wrong place in the
            // transcript forever, which is true, and which is why the answer is to
            // open the right turn rather than to guess one.
            //
            // Measured against claude 2.x: after a turn's `result`, a session with
            // backgrounded work emits `background_tasks_changed`, `task_updated`, and
            // `task_notification` when that work finishes, and then an entire exchange
            // of its own — `init`, an assistant message, another `result`. Its stdout
            // stays open and the process does not exit. So the drop discarded exactly
            // the completion a user had been promised, along with the agent's own
            // report of it, and no monitor, schedule, or background job could ever
            // reach a client however the rest of the stack was built.
            Ok(None) => match self.open_agent_turn(session_id).await {
                Some(id) => id,
                None => return,
            },
            Err(error) => {
                error!(
                    "Could not read the running turn for session {}: {}",
                    session_id, error
                );
                return;
            }
        };

        // Infallible now, and that is the change worth noting: the event arrives as a
        // `sylph_protocol::Event` rather than as a JSON object to inspect, so the
        // "unparseable adapter event" branch this replaced cannot be reached. A kind this
        // build does not model lands in `EventKind::Other` and is preserved verbatim
        // instead of being dropped.
        let mut event = StreamEvent::from_event(&event);

        match self.seq.next(session_id).await {
            Ok(seq) => event.seq = seq,
            Err(error) => {
                error!("Failed to allocate a sequence number: {}", error);
                return;
            }
        }

        // Everything is journaled, including deltas.
        //
        // This reverses v1, where a delta was ephemeral and dropped. Lossless
        // replay needs them: consolidation folds a block's deltas into ~8 bytes
        // each, and it can only fold what the journal actually kept. Dropping them
        // here would make `docs/storage.md`'s whole guarantee unreachable — the
        // fold would have nothing to fold.
        //
        // The cost is bounded and short-lived. Deltas sit in the hot journal only
        // until the turn settles, which is when consolidation replaces them with
        // the folded form and deletes the rows.
        //
        // A failure here is loud on purpose. This was a real bug: a CHECK
        // constraint rejected every adapter event, and because the error was only
        // logged at debug level the turn streamed perfectly and came back empty on
        // reload. An event published but not journaled is worse than one that never
        // arrived, because the client has already been told it happened.
        // Stable events commit immediately; ephemeral ones join a ~50 ms window.
        // The writer holds that distinction rather than this call site, because
        // which events may be batched is a property of where a reconnect replays
        // them from — see [`crate::db::writer`].
        if let Err(error) = self
            .writer
            .append(&event.to_row(session_id, &turn_id), event.is_stable())
            .await
        {
            error!(
                "DATA LOSS: event seq {} kind '{}' on session {} was published but not journaled: {}",
                event.seq, event.kind, session_id, error
            );
        }

        // A delegation's outcome may belong to an earlier turn, because a trajectory id
        // is session-scoped and backgrounded work reports back after the turn that
        // dispatched it has settled. Done here rather than when the turn settles so the
        // correction is already in place by the time a client refetches history, which
        // it does on the settle it would otherwise race.
        if event.kind == "subagent" && event.phase.as_deref() == Some("completed") {
            if let Some(stream_id) = event.stream_id.clone() {
                let reconciler = self.reconciler.read().await.clone();
                if let Some(reconciler) = reconciler {
                    reconciler
                        .record(
                            session_id,
                            &stream_id,
                            &turn_id,
                            &crate::supervisor::TrajectoryOutcome {
                                text: &event.text,
                                is_error: event.is_error,
                                reported_on_turn_id: &turn_id,
                            },
                        )
                        .await;
                }
            }
        }

        let settles = event.kind == "turn" && event.phase.as_deref() == Some("completed");
        let is_error = event.is_error;
        let stop_reason = event
            .data
            .as_ref()
            .and_then(|d| d.get("stopReason"))
            .and_then(|r| r.as_str())
            .map(|s| s.to_string());
        let text = event.text.clone();

        self.hub.publish(&turn_id, event).await;

        if settles {
            use crate::supervisor::turn::Outcome;
            // The agent's own error flag decides this, not the transport: a turn
            // that ran and reported failure is a completed exchange the user
            // needs to see, not a broken link.
            let outcome = if is_error {
                Outcome::Failed {
                    message: if text.is_empty() {
                        "The agent reported a failure".to_string()
                    } else {
                        text
                    },
                }
            } else if stop_reason.as_deref() == Some("interrupted") {
                Outcome::Interrupted
            } else {
                Outcome::Completed { stop_reason }
            };
            self.settle_turn(session_id, &turn_id, outcome).await;
        }
    }

    /// Open a turn for an agent that spoke without being asked.
    ///
    /// A real turn, not a container: it consolidates, its events order by sequence,
    /// and it appears in the transcript where it happened. `origin` records that
    /// nobody prompted it, which a reader needs and an empty `request_text` cannot
    /// say on its own (`migrations/006_turn_origin.sql`).
    ///
    /// Opened on the *first* out-of-turn event of any kind rather than on a
    /// `turn`/`started`. The vendor's own turn boundary arrives partway through:
    /// `task_notification` — the completion this exists to deliver — precedes it. So
    /// waiting for the bracket would drop the payload and keep only the announcement.
    /// The adapter's later `turn`/`started` then lands inside this turn instead of
    /// opening a second one, because a turn is already running by then.
    ///
    /// `None` means the turn could not be recorded, and the caller drops the event.
    /// That is the same outcome as before for a session whose row is gone — a link
    /// outliving its session — and it stays a drop rather than an unattributed write.
    async fn open_agent_turn(&self, session_id: &SessionId) -> Option<String> {
        let turn = crate::models::Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            status: "running".to_string(),
            origin: "agent".to_string(),
            // Empty by construction: there was no request. `origin` is what
            // distinguishes this from a user who sent nothing.
            request_text: String::new(),
            command_invocation_id: None,
            idempotency_key: None,
            error_code: None,
            error_message: None,
            created_at_ms: now_ms(),
            started_at_ms: Some(now_ms()),
            completed_at_ms: None,
            stopped_at_ms: None,
        };

        match self.store.create_turn(&turn).await {
            Ok(created) => {
                info!(
                    "Session {} started a turn of its own ({})",
                    session_id, created.id
                );
                // The session is running again, so the composer reflects that rather
                // than inviting a message the agent is busy to receive.
                let _ = self.store.update_session_status(session_id, "running").await;
                // Announced on the session stream, which is the one a client holds
                // between turns. Without this a client learns about the turn only by
                // polling, so the notification it exists to carry arrives late or not
                // at all.
                self.publish_session_event(
                    session_id,
                    "turn-opened",
                    serde_json::json!({"turnId": created.id, "origin": "agent"}),
                )
                .await;
                Some(created.id)
            }
            Err(error) => {
                error!(
                    "Could not open an agent-initiated turn on session {}: {}",
                    session_id, error
                );
                None
            }
        }
    }

    /// Persist a request and announce it.
    ///
    /// Journaled first. An approval blocks the agent, so if the process dies
    /// between here and the publish, the record of what was being asked is what
    /// explains the stall — and it is answerable later from any frontend.
    async fn absorb_request(&self, session_id: &SessionId, request: sylph_protocol::Request) {
        let turn_id = self
            .store
            .running_turn_id(session_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

        let record = PendingRequest {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            turn_id,
            // The agent's own id, read off the request rather than passed beside it. The v3
            // transport carried the two separately because a wire frame had a
            // correlation id of its own; `Request::id` is the same value and the only one,
            // so there is nowhere for a mismatch to live.
            adapter_request_id: request.id.clone(),
            kind: request.kind.to_string(),
            tool: request.tool.clone(),
            tool_use_id: request.tool_use_id.clone(),
            title: request.title.clone(),
            detail_json: request
                .detail
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "{}".to_string()),
            options_json: serde_json::to_string(&request.options).unwrap_or_else(|_| "[]".into()),
            // Serialized rather than stored as sent, so what a client renders and what
            // the server validates an answer against are the same bytes. A form that
            // fails to serialize is dropped to `None`, which degrades to answering by
            // option rather than leaving the request unrenderable.
            form_json: request
                .form
                .as_ref()
                .and_then(|form| serde_json::to_string(form).ok()),
            default_option: request.default.clone(),
            risk: request.risk.map(|risk| risk.to_string()),
            allow_persistent: request.allow_persistent,
            expires_at_ms: request.expires_at_ms,
            status: "pending".to_string(),
            chosen_option: None,
            resolved_input_json: None,
            resolved_by: None,
            created_at_ms: now_ms(),
            resolved_at_ms: None,
        };

        let stored = match self.store.create_pending_request(&record).await {
            Ok(stored) => stored,
            Err(error) => {
                error!("Failed to persist a pending request: {}", error);
                return;
            }
        };

        info!(
            "Session {} is waiting on a {} request ({})",
            session_id, stored.kind, stored.id
        );

        self.announce_request(session_id, &stored).await;
    }

    /// A link died. Record why, and free anything waiting on it.
    /// `detail` is the last read failure the stream reported, when it reported one.
    ///
    /// It is what replaces the stderr tail an exiting subprocess used to carry. Not a
    /// synonym for it: a tail was whatever the adapter happened to print, while this is a
    /// failure the adapter chose to report — so it is absent far more often, and absent is
    /// the honest answer for a session that simply ended.
    ///
    /// # UNRESOLVED: this is where "why did it end" is lost, and four subsystems arrive here
    ///
    /// **A `Stream` that ends says only that it ended.** `LinkEvent::Closed { exit_code,
    /// stderr_tail }` carried two facts an in-process adapter has no equivalent for, and
    /// `detail` recovers one of them only when the adapter chose to emit an `Err` before
    /// dropping its sender. When it did not — the ordinary case — a client is told the turn
    /// failed `LINK_LOST` with no reason, and **these three become indistinguishable**:
    ///
    /// - the vendor CLI crashed or was killed,
    /// - the agent finished and closed its stdio normally,
    /// - the adapter dropped the stream through a bug of its own.
    ///
    /// Three different fixes, one appearance — which is the shape `AGENTS.md` records as the
    /// hardest outcome to debug, and the reason nothing may produce silence by default.
    ///
    /// **Deliberately not solved here, because a branch cannot settle it.** Four subsystems
    /// have now reached the same hole from different directions: `claude`'s adapter cannot
    /// tell a wedged CLI from a quiet one, `codex` has no message for an ask another
    /// subscriber retired, `adopt`'s `watch` cannot tell a store that stopped growing
    /// because the vendor died from one that stopped because the session ended, and this one
    /// is a session dying mid-turn becoming `abandoned` with no reason. Four arrivals make it
    /// a `sylph_protocol` decision — the vocabulary needs a way for a stream to say *why* it
    /// is ending — and inventing a fourth local shape here would make the fix land four
    /// times differently, which is exactly what a shared vocabulary exists to prevent.
    ///
    /// What is available to build on when it is taken up: `crates/adapters`'s
    /// `process::Child::error(context)` already builds an error carrying the stderr tail, and
    /// `has_ended()` answers whether the vendor's stdout has closed. So an adapter *can*
    /// report the reason today; what is missing is anything in the protocol that obliges it
    /// to, and any way for this function to tell "reported nothing" from "had nothing to
    /// report".
    async fn handle_close(&self, session_id: &SessionId, detail: Option<String>) {
        self.links.write().await.remove(&session_id.encode());

        // A turn still in flight will never hear from this agent again, so it is
        // settled here rather than left waiting on a channel whose sender is
        // about to drop. Doing it explicitly carries whatever the stream said last, which
        // is the only diagnostic there is — without it the timeline cannot distinguish
        // "the agent stopped" from "the agent finished".
        if let Ok(Some(turn_id)) = self.store.running_turn_id(session_id).await {
            self.settle_turn(
                session_id,
                &turn_id,
                crate::supervisor::turn::Outcome::LinkLost {
                    detail: detail.clone(),
                },
            )
            .await;
        }

        // Announced per row, not merely counted. A client with an approval panel open
        // is showing a question that can no longer be answered — the adapter waiting on
        // it is gone — and the `link` event below does not carry that: a client would
        // have to infer "so my dialog is dead" from "the link closed", which is exactly
        // the inference the request's own status states outright.
        match self.store.abandon_open_requests(session_id).await {
            Ok(abandoned) if abandoned.is_empty() => {}
            Ok(abandoned) => {
                warn!(
                    "Abandoned {} unanswered request(s) on session {}: its agent exited",
                    abandoned.len(),
                    session_id
                );
                for request in &abandoned {
                    self.announce_request(session_id, request).await;
                }
            }
            Err(error) => error!("Failed to abandon open requests: {}", error),
        }

        // No `exitCode`: there is no process of Sylph's own to exit, so the field would be
        // a constant `null` claiming a fact nothing holds. `detail` carries what there is.
        let mut data = serde_json::json!({"reason": "closed", "attached": false});
        if let Some(detail) = detail {
            data["detail"] = detail.into();
        }
        self.publish_session_event(session_id, "link", data).await;
    }

    /// Close a link deliberately.
    pub async fn detach(&self, session_id: &SessionId, reason: &str) {
        let entry = self.links.write().await.remove(&session_id.encode());
        let Some(entry) = entry else { return };

        entry.link.close().await;
        // Same announcement as the unexpected close above. A deliberate detach kills
        // outstanding questions just as thoroughly, and the client that asked for the
        // detach is not necessarily the one holding the dialog.
        if let Ok(abandoned) = self.store.abandon_open_requests(session_id).await {
            for request in &abandoned {
                self.announce_request(session_id, request).await;
            }
        }
        self.publish_session_event(
            session_id,
            "link",
            serde_json::json!({"reason": reason, "attached": false}),
        )
        .await;
    }

    /// Re-publish the current capability surface to attached clients.
    ///
    /// Used after a config change made through the API: the client that made it
    /// learns the outcome from its own response, but every other attached client
    /// would otherwise keep showing the previous value.
    pub async fn announce_capabilities(&self, session_id: &SessionId) {
        let Some(link) = self.get(session_id).await else {
            return;
        };
        let surface = link.capabilities().await;
        self.publish_session_event(
            session_id,
            "capabilities",
            serde_json::to_value(&surface).unwrap_or_default(),
        )
        .await;
    }

    /// Announce one request's current state to every attached client.
    ///
    /// The single publisher for a request's whole life, and the reason it exists is a
    /// bug of omission: *arrival* was announced and every *departure* was silent. So a
    /// question answered on a phone stayed on screen on a laptop, and one killed with
    /// its agent stayed on screen everywhere — both until a 20-second poll happened to
    /// fire, which a backgrounded tab does not run at all. The user's report was
    /// "要刷新才会消失", and refreshing was genuinely the only reliable way.
    ///
    /// The payload is the **row**, not the transition, and the row carries `status`.
    /// That is deliberate: a client reconciles against a state rather than replaying a
    /// vocabulary of verbs, so an `expired` status added later needs no new event kind
    /// and no client change. It is the same reasoning as the frame parser's — read the
    /// name, dispatch on the content — and the opposite of the mistake `capabilities`
    /// facets exist to prevent, since a row speaks only for the request it names.
    ///
    /// One event per row rather than one per batch, even when a link death abandons
    /// several. A batch would make a client diff two sets to learn what changed, which
    /// is the work this event exists to save it.
    pub async fn announce_request(&self, session_id: &SessionId, request: &PendingRequest) {
        self.publish_session_event(
            session_id,
            "request",
            serde_json::to_value(request).unwrap_or_default(),
        )
        .await;
    }

    /// Publish a session-scoped fact.
    ///
    /// Links, capability changes, and requests are properties of a session rather
    /// than of a turn, so a client watching only turns would miss an approval
    /// raised while it was idle.
    async fn publish_session_event(
        &self,
        session_id: &SessionId,
        kind: &str,
        data: serde_json::Value,
    ) {
        let event = StreamEvent {
            seq: self.seq.next(session_id).await.unwrap_or(0),
            kind: kind.to_string(),
            phase: None,
            stream_id: None,
            // Session-scoped, so never inside a delegation: this fires between
            // turns, when no trajectory is open.
            span: None,
            name: None,
            text: String::new(),
            data: Some(data),
            is_error: false,
            stability: "stable".to_string(),
            source: "server".to_string(),
            created_at_ms: now_ms(),
            // A live session fact is one arrival.
            started_at_ms: None,
        };
        self.hub.publish_session(&session_id.encode(), event).await;
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
