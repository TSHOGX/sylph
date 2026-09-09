//! Sylph Web backend library.
//!
//! The binary in `main.rs` is a thin wrapper: it loads configuration, builds
//! [`AppState`], and serves [`router`]. Everything else lives here so
//! integration tests can construct the same router in-process and drive it
//! through `tower::ServiceExt::oneshot`, with no ports to bind and no race
//! against a spawning server.

use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

/// Which agents this machine can run, and where the server gets an adapter from.
pub mod adapters;
pub mod api;
pub mod attachments;
pub mod config;
pub mod db;
/// Parsing spreadsheets, documents, and presentations. A library with no HTTP
/// knowledge, so it sits beside `attachments` rather than inside `api`.
pub mod documents;
/// Where a session's work happens: local today, a Khora sandbox or SSH next.
/// The seam local fork/exec used to occupy implicitly.
pub mod env;
pub mod error;
pub mod events;
/// The pool of live agent sessions, and the one place a link's output becomes
/// durable state.
pub mod links;
pub mod middleware;
pub mod models;
pub mod paths;
/// Durable, replayable history: folded deltas, content-addressed blobs, and
/// watermark-driven retention.
pub mod storage;
pub mod supervisor;

use crate::config::Config;
use crate::db::JournalStore;
use crate::error::Error;
use crate::events::EventHub;

#[cfg(test)]
mod fixtures;

#[derive(Clone)]
pub struct AppState {
    pub store: JournalStore,
    pub config: Config,
    pub hub: EventHub,
    /// Where a session's work runs.
    ///
    /// One environment today, and it is held here rather than constructed per
    /// caller so that pairing (`docs/roadmap.md` § Milestone 1)
    /// replaces one field instead of reaching into every consumer. This is also what
    /// `GET /environments` reports availability from — the surface that replaced
    /// discovery's `agents.available` and `broken_adapters`.
    pub environment: Arc<dyn crate::env::Environment>,
    /// Runs turns on live links.
    pub turns: crate::supervisor::turn::LinkedSupervisor,
    pub links: crate::links::LinkManager,
    /// The warm tier. Reads come here once a turn has been consolidated.
    pub history: crate::storage::HistoryStore,
    /// Moves settled turns into history. Exposed so a test can trigger the same
    /// work the background sweeper does, rather than waiting on it.
    pub consolidator: crate::storage::Consolidator,
    /// Prunes history down to what a downstream authority confirms. Exposed for the
    /// same reason as the consolidator: a test drives one pass instead of waiting
    /// five minutes for the interval.
    pub retention: crate::storage::RetentionScheduler,
    /// Writes an adapter's parse of a native store into the journal.
    ///
    /// Held here, sharing the **one** sequence allocator and the **one** batching
    /// writer, because that sharing is the correctness property rather than a
    /// convenience: a second allocator over one session would hand out numbers a live
    /// link is also handing out and `UNIQUE(session, seq)` would refuse one of the two
    /// writes, intermittently and under concurrency (`crate::events::seq`). A second
    /// writer would leave the fold reading a buffer nobody flushed.
    pub ingest: crate::db::NativeIngest,
    /// A short-lived snapshot of what this machine's native stores hold.
    ///
    /// Both reasons it exists are measured and neither is a micro-optimization: listing
    /// this machine's stores costs ~5.6 s, and an offset cursor into a store being
    /// written silently skips and duplicates rows. See `api::native_index`.
    pub native_index: crate::api::native_index::NativeIndex,
    /// Inbound envagent protocol bridge. The local adapter seam remains in
    /// place for compatibility fixtures; paired installations use this
    /// bridge for durable ingest and command routing.
    pub env_bridge: Arc<crate::env::EnvBridge>,
}

impl AppState {
    /// Consolidate every settled turn now, returning how many moved.
    pub async fn consolidate_now(&self) -> usize {
        self.consolidator.sweep().await
    }

    /// Assemble application state from configuration.
    ///
    /// An install that can run no agents is **reported, not fatal**: history stays
    /// readable and every other surface works, which is why the report has to be
    /// loud rather than the startup being aborted.
    pub async fn build(config: Config) -> anyhow::Result<Self> {
        Self::build_with_adapters(config, crate::adapters::linked()).await
    }

    /// Assemble state against a given adapter source.
    ///
    /// Separate from [`Self::build`] because the integration suite needs an agent that is
    /// deterministic, has no vendor CLI, and can be told to fail — and a *different* one
    /// per test, since tests run concurrently in one process. A name-keyed global registry
    /// cannot provide that, so the source is injected rather than discovered. It is the
    /// same reason `environment` is a field: step 4 replaces the local source with a
    /// per-environment one and this signature already accepts it.
    pub async fn build_with_adapters(
        config: Config,
        adapters: Arc<dyn crate::adapters::AdapterSource>,
    ) -> anyhow::Result<Self> {
        let store = JournalStore::new(&config.journal_path).await?;
        tracing::info!("Journal initialized at {}", config.journal_path);

        // Zero runnable agents is not a quiet degradation: no session can be created
        // and no existing one can offer a command, so every feature depending on a
        // live link fails while the server reports healthy. Discovery logged that at
        // the same level as success, one line among hundreds, which is how a stale
        // install presented as a composer ignoring the `/` key.
        //
        // Availability is now per environment (`docs/environments.md`), so
        // `GET /environments` is where a client learns *which* agent is missing on
        // *which* machine. This line is the server-side half, and it stays at `error`
        // for the same reason it was raised to `error` before: it names a total
        // failure whose symptom appears three layers away.
        let runnable = adapters.available();
        if runnable.is_empty() {
            tracing::error!(
                "No agent CLI is on this machine's PATH, so this build can run nothing. \
                 Sessions cannot be created and no slash commands will be offered. \
                 Install one of {:?} and restart; see `GET /api/v1/environments` for what \
                 each environment reports.",
                // From the source, not from `sylph_adapters`: the two can disagree, and a
                // line telling an operator to install one of three agents when this build
                // knows a different set is worse than saying nothing.
                adapters.known_agents()
            );
        } else {
            tracing::info!("Agents this build can run: {}", runnable.join(", "));
        }

        // Child processes do not survive a restart, so anything the journal
        // still lists as live is unrecoverable.
        let interrupted = store.interrupt_stale_turns().await?;
        if interrupted > 0 {
            tracing::warn!(
                "Interrupted {} stale turns from a previous backend session",
                interrupted
            );
        }

        // Before the consolidator can move those turns. A crash skips `settle`, so
        // a delegation open at crash time has no closing event — and consolidation
        // folds a trajectory with no close into no block at all, which loses the
        // delegation while keeping its children, reattaching them to the root.
        let closed = store.close_stale_trajectories().await?;
        if closed > 0 {
            tracing::warn!(
                "Closed {} delegated trajectory(ies) left open by a previous backend session",
                closed
            );
        }

        // No link survives a restart, so a request still marked pending is
        // waiting on a process that no longer exists. Left alone it would show a
        // session forever blocked on a question nobody can answer.
        let abandoned = store.abandon_all_open_requests().await?;
        if abandoned > 0 {
            tracing::warn!(
                "Abandoned {} unanswered agent request(s) from a previous backend session",
                abandoned
            );
        }

        // Resolved once, in `Config`, so the supervisor injects the same file
        // `GET /soul` displays. Deriving it here as well is how the two would
        // drift into disagreeing about whether identity is active.
        let soul_path = config.soul_path.clone();

        // The warm tier, opened before anything can settle a turn into it.
        let history = crate::storage::HistoryStore::open(
            &config.history_path,
            crate::storage::cas::BlobStore::new(config.blobs_dir.clone()),
        )
        .await?;
        // The same repair as `close_stale_trajectories` above, one tier down.
        // Consolidation preserves a trajectory the turn-settle sweep deliberately left
        // open — backgrounded work, which is genuinely still running — and `started` is
        // a true statement only while the process that would report it is alive. No
        // link survives a restart, so anything still `started` here can never be
        // reported on and would spin forever.
        //
        // Before the sweeper, so a turn consolidated by this run is not immediately
        // re-examined, and after the journal sweep above so the two cannot both claim
        // the same trajectory.
        let stranded = history.close_running_trajectories().await?;
        if stranded > 0 {
            tracing::warn!(
                "Closed {} delegated trajectory(ies) left running in history by a previous backend session",
                stranded
            );
        }

        let consolidator = crate::storage::Consolidator::new(store.clone(), history.clone());
        // Catches whatever a crash or a restart left unconsolidated, then keeps
        // watching. A turn that settles normally is handled the moment it does.
        consolidator.spawn_sweeper();

        let hub = EventHub::new();
        // The host is the only environment today. It is constructed here rather
        // than inside the supervisor so a session-chosen environment later
        // replaces one argument instead of reaching into the supervisor.
        let environment: Arc<dyn crate::env::Environment> =
            Arc::new(crate::env::LocalEnvironment::new());
        // One allocator, shared. Both the supervisor and each link's drain task
        // emit events for the same session, and separate counters would hand out
        // the same sequence number.
        let seq = crate::events::SeqAllocator::new(store.clone());
        let environment_registry = crate::env::EnvironmentRegistry::new();
        let env_bridge = Arc::new(crate::env::EnvBridge::new(
            store.clone(),
            history.clone(),
            seq.clone(),
            hub.clone(),
            environment_registry,
        ));
        env_bridge.spawn_health_reaper();

        // Batches the journal's hot path. One instance, shared: the drain task
        // buffers a turn's deltas and the supervisor flushes them before the fold,
        // so separate writers would leave the fold reading a buffer nobody flushed.
        let writer = crate::db::JournalWriter::new(store.clone());

        let links = crate::links::LinkManager::new(crate::links::LinkManagerConfig {
            store: store.clone(),
            history: history.clone(),
            data_dir: config.data_dir.clone(),
            writer: writer.clone(),
            hub: hub.clone(),
            adapters: adapters.clone(),
            soul_path,
            seq: seq.clone(),
        });

        // Turns on a live link. The two reference each other — the supervisor
        // asks for a link, the manager settles the supervisor's turns — so the
        // waiter registry is handed over after both exist.
        // Adopting a native session writes through the same three collaborators the live
        // drain path uses, which is what makes an adopted row indistinguishable from a
        // live one downstream (`docs/architecture.md` § One authority).
        let ingest = crate::db::NativeIngest::new(
            store.clone(),
            writer.clone(),
            seq.clone(),
            history.clone(),
        )
        .with_hub(hub.clone());

        let turns = crate::supervisor::turn::LinkedSupervisor::new(
            store.clone(),
            writer.clone(),
            hub.clone(),
            links.clone(),
            seq,
            consolidator.clone(),
        );
        links.set_waiters(turns.waiters()).await;
        // The other half of settling: a turn the *agent* started has no waiter, so
        // resolving a channel cannot end it. Without this the first unprompted turn
        // would sit `running` forever and wedge its session, because the composer
        // refuses a message while a turn is live.
        links.set_closer(turns.closer()).await;
        // Remote envagent event batches use the same terminal settlement path as
        // in-process links. Install it only after the supervisor owns its
        // consolidator so the bridge never grows a second definition of a
        // finished turn.
        env_bridge.set_turn_closer(turns.closer()).await;
        env_bridge
            .set_trajectory_reconciler(crate::supervisor::TrajectoryReconciler::new(
                store.clone(),
                history.clone(),
            ))
            .await;
        // And the half that reaches *backwards*: a delegation that outlived its turn
        // reports its outcome on a later one, and the record it corrects belongs to the
        // turn that opened it (`crate::supervisor::reconcile`). Both stores, because
        // consolidation decides which tier holds that record and no caller can observe
        // when it runs.
        links
            .set_reconciler(crate::supervisor::TrajectoryReconciler::new(
                store.clone(),
                history.clone(),
            ))
            .await;

        // Retention's authority is now the **envagent that holds the vendor's own
        // native store** rather than a separate archive (`docs/storage.md`
        // § Two retention classes). The environment is passed rather than assumed inside so
        // a per-session environment later replaces an argument.
        let retention = crate::storage::RetentionScheduler::new(
            store.clone(),
            history.clone(),
            environment.clone(),
        );
        retention.spawn();

        Ok(Self {
            store,
            config,
            environment,
            hub,
            turns,
            links,
            history,
            consolidator,
            retention,
            ingest,
            native_index: crate::api::native_index::NativeIndex::new(),
            env_bridge,
        })
    }
}

/// Largest request body the API accepts.
///
/// `docs/api/files.md` caps a turn at 10 MiB of attachments, and base64
/// inflates that by about a third before JSON overhead, so the wire limit must
/// exceed the payload limit or a valid upload would be rejected before
/// [`attachments::stage`] could report which file was too large.
///
/// This must be applied as [`DefaultBodyLimit`], not `tower_http`'s
/// `RequestBodyLimitLayer`: the `Json` extractor consults `DefaultBodyLimit`
/// and enforces its own 2 MiB default, so raising only the outer tower limit
/// still yields a 413 from the extractor before any handler runs.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Build the API router.
///
/// Route order matters for the auth layer: `/health` and `/auth/token` are
/// exempt inside the middleware rather than being registered outside it, so
/// that the exempt list stays in one place next to the checks it bypasses.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(api::health::health))
        .route("/api/v1/auth/token", get(middleware::auth::bootstrap_token))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
        // What configuration resolved to, and which source supplied each value.
        // Read-only: see `api::config` for why a loopback HTTP surface must not be
        // able to rewrite `browse_roots` or `agents_dir`.
        .route("/api/v1/config", get(api::config::get))
        // Which environments are paired, whether they are reachable, and what each
        // can run. This is what replaced `GET /agents`: availability is per machine,
        // so a merged global list would be true of nowhere.
        .route("/api/v1/environments", get(api::environments::list))
        .route(
            "/api/v1/environments/pairings",
            post(api::environments::create_pairing),
        )
        .route("/api/v1/environments/pair", post(api::environments::pair))
        .route(
            "/api/v1/environments/{id}",
            axum::routing::delete(api::environments::unpair),
        )
        .route(
            "/api/v1/sessions",
            get(api::sessions::list).post(api::sessions::create),
        )
        .route(
            "/api/v1/sessions/draft/{id}/capabilities",
            get(api::links::prepared_capabilities),
        )
        .route(
            "/api/v1/sessions/draft/{id}",
            get(api::sessions::prepared_get).delete(api::links::discard_prepared),
        )
        .route(
            "/api/v1/sessions/draft/{id}/turns",
            post(api::turns::create_prepared),
        )
        .route(
            "/api/v1/sessions/draft/{id}/config",
            post(api::links::prepared_set_config),
        )
        // A prepared link already knows its contained workdir, even though it has
        // no durable identity yet.  Reuse the session file handlers; `resolve`
        // distinguishes the server-issued draft handle from an encoded session id.
        .route(
            "/api/v1/sessions/draft/{id}/files",
            get(api::files::tree)
                .post(api::files::upload)
                .delete(api::files::remove),
        )
        .route(
            "/api/v1/sessions/draft/{id}/files/directory",
            post(api::files::mkdir),
        )
        .route(
            "/api/v1/sessions/draft/{id}/files/content",
            get(api::files::content),
        )
        .route(
            "/api/v1/sessions/draft/{id}/files/document",
            get(api::files::document),
        )
        .route(
            "/api/v1/sessions/draft/{id}/files/raw",
            get(api::files::raw),
        )
        // Bring in a session an agent already has, from that agent's own native
        // store. Replaces `POST /sessions/import`, which named a row in a separate
        // archive that no longer exists.
        .route("/api/v1/sessions/adopt", post(api::sessions::adopt))
        // What native sessions an environment holds, without adopting any. This is what
        // makes adopt offerable in a UI rather than requiring a user to know a vendor
        // id. A literal path, so it is declared beside `adopt` and above
        // `/sessions/{id}` — axum prefers a static segment over a capture, but keeping
        // the two literals together is what makes that visible to a reader.
        .route("/api/v1/sessions/enumerate", get(api::sessions::enumerate))
        // No `DELETE`: there is no empty draft to discard, because `POST /sessions`
        // now starts an agent and a row exists only once one has assigned an id
        // (`docs/api/sessions.md` § `DELETE /api/v1/sessions/{id}` — removed).
        .route(
            "/api/v1/sessions/{id}",
            get(api::sessions::get).patch(api::sessions::update),
        )
        .route(
            "/api/v1/sessions/{id}/turns",
            get(api::sessions::history).post(api::turns::create),
        )
        .route("/api/v1/sessions/{id}/edges", get(api::sessions::edges))
        // Files before `{id}` would be ambiguous the other way round: axum
        // matches literal segments before captures, so the more specific
        // `files/content` route must be registered, but order among these
        // does not matter. They are grouped for readability.
        .route(
            "/api/v1/sessions/{id}/files",
            get(api::files::tree)
                .post(api::files::upload)
                .delete(api::files::remove),
        )
        // Creating a directory is its own route rather than a mode of the POST
        // above: one takes a body of bytes and the other takes a name, and a
        // handler switching on which field is present is how an upload with a
        // typo'd key silently becomes a mkdir.
        .route(
            "/api/v1/sessions/{id}/files/directory",
            post(api::files::mkdir),
        )
        .route(
            "/api/v1/sessions/{id}/files/content",
            get(api::files::content),
        )
        // Parsed documents and raw bytes, both rooted at the session workdir
        // like the tree above.
        .route(
            "/api/v1/sessions/{id}/files/document",
            get(api::files::document),
        )
        .route("/api/v1/sessions/{id}/files/raw", get(api::files::raw))
        // Machine-level directory browsing, bounded by configured roots rather
        // than by a session. Separate from the `sessions/{id}/files` tree
        // because the root differs: see `api::fs` for the threat model.
        .route("/api/v1/fs/roots", get(api::fs::roots))
        .route("/api/v1/fs/tree", get(api::fs::tree))
        // The identity text itself, so the injection toggle is not a decision
        // made blind. Read-only: editing SOUL.md is a filesystem act, and a
        // loopback HTTP surface that rewrote a prompt preamble would be a much
        // larger security question than one that displays it.
        .route("/api/v1/soul", get(api::soul::get))
        .route("/api/v1/workspaces", get(api::workspaces::list))
        .route("/api/v1/search", get(api::search::search))
        // A session's live agent: what it can do, and what it is waiting on.
        .route(
            "/api/v1/sessions/{id}/capabilities",
            get(api::links::capabilities),
        )
        .route(
            "/api/v1/sessions/{id}/link",
            post(api::links::attach).delete(api::links::detach),
        )
        .route(
            "/api/v1/sessions/{id}/requests",
            get(api::links::list_requests),
        )
        .route("/api/v1/requests/{id}/resolve", post(api::links::resolve))
        // Scoped to the session rather than the turn: both act on the agent, and
        // the session is what holds a link. A turn-scoped route would need the
        // caller to know a turn id to interrupt the only turn running.
        .route(
            "/api/v1/sessions/{id}/interrupt",
            post(api::links::interrupt),
        )
        .route("/api/v1/sessions/{id}/steer", post(api::links::steer))
        // Separate from `steer` because the two differ by *when the agent reads the text*,
        // which decides what the request means: a steer carries a turn precondition and a
        // queued follow-up cannot have one. See `api::links::queue`.
        .route("/api/v1/sessions/{id}/queue", post(api::links::queue))
        // Typed session config: what `/model` and `/permissions` resolve to, so a
        // control command is a typed set rather than prose sent as a prompt.
        .route("/api/v1/sessions/{id}/config", post(api::links::set_config))
        // Replay is session-scoped and server-paced: reconstruction is subtle
        // enough that doing it per frontend would let frontends disagree.
        .route("/api/v1/sessions/{id}/replay", get(api::replay::replay))
        // Session-scoped facts: a link attaching or dying, capabilities changing,
        // a request raised. Distinct from the turn stream, which only exists while
        // a turn does.
        .route("/api/v1/sessions/{id}/events", get(api::links::events))
        // Complete reconnectable session trace, including turn and delegated trajectory
        // events. The facts stream above remains a lightweight invalidation channel.
        .route(
            "/api/v1/sessions/{id}/trace/events",
            get(api::links::trace_events),
        )
        .route("/api/v1/turns/{id}", get(api::turns::get))
        .route("/api/v1/turns/{id}/events", get(api::turns::events))
        .route("/api/v1/turns/{id}/stop", post(api::turns::stop))
        // Registered before `.layer`, which only wraps routes that already
        // exist: a fallback added afterwards would bypass the auth and trace
        // layers, and would answer unmatched paths without being logged.
        //
        // These two exist so the error envelope has no holes. axum's default
        // fallback returns a bodiless 404, which a client reads as the bare
        // string "Not Found" — identical to what it shows when a *file* is
        // missing. Making the router speak the same envelope as every handler
        // is what lets a client distinguish a stale backend from a bad path.
        .fallback(|| async { Error::RouteNotFound })
        .method_not_allowed_fallback(|| async { Error::MethodNotAllowed })
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::auth_middleware,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
