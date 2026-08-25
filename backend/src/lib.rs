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
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;

pub mod api;
pub mod attachments;
pub mod config;
pub mod db;
/// Parsing spreadsheets, documents, and presentations. A library with no HTTP
/// knowledge, so it sits beside `attachments` rather than inside `api`.
/// The duplex agent-session protocol. A live adapter process is a *link*, and
/// what it can do is negotiated rather than assumed.
pub mod adapter;
pub mod documents;
/// Where a session's work happens: local today, a Khora sandbox or SSH next.
/// The seam local fork/exec used to occupy implicitly.
pub mod env;
pub mod error;
/// The pool of live agent sessions, and the one place a link's output becomes
/// durable state.
pub mod links;
pub mod events;
pub mod middleware;
pub mod models;
pub mod paths;
pub mod runners;
/// Durable, replayable history: folded deltas, content-addressed blobs, and
/// watermark-driven retention.
pub mod storage;
pub mod supervisor;
pub mod tracedb;

use crate::config::Config;
use crate::error::Error;
use crate::db::JournalStore;
use crate::events::EventHub;
use crate::runners::Registry;
use crate::tracedb::Client as TraceDbClient;

#[derive(Clone)]
pub struct AppState {
    pub store: JournalStore,
    pub config: Config,
    pub registry: Arc<Mutex<Registry>>,
    pub hub: EventHub,
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
    pub tracedb: TraceDbClient,
}

impl AppState {
    /// Consolidate every settled turn now, returning how many moved.
    pub async fn consolidate_now(&self) -> usize {
        self.consolidator.sweep().await
    }

    /// Assemble application state from configuration.
    ///
    /// Runner discovery and the TraceDB probe are both non-fatal: a missing
    /// runners directory or a misconfigured TraceDB binary leaves the backend
    /// serving what it can, with the failure visible in `/diagnostics`.
    pub async fn build(config: Config) -> anyhow::Result<Self> {
        // Before the store opens the path, so an install predating the rename
        // carries its history forward instead of starting empty beside it.
        crate::db::migrate::rename_legacy_journal(&config.journal_path).await;
        let store = JournalStore::new(&config.journal_path).await?;
        tracing::info!("Journal initialized at {}", config.journal_path);

        // Bundled adapters first, so a user-installed one of the same name
        // shadows rather than duplicates. Discovery handshakes with each, which
        // is why it needs the environment.
        let discovery_env: Arc<dyn crate::env::Environment> =
            Arc::new(crate::env::LocalEnvironment::new());
        let registry = Arc::new(Mutex::new(
            Registry::new(config.agent_roots(), discovery_env).await?,
        ));
        {
            let registry = registry.lock().await;
            let usable = registry.list_agents().len();
            tracing::info!(
                "Agent discovery: {} usable, {} broken",
                usable,
                registry.broken_runners().len()
            );
            // Zero usable agents is not a quiet degradation: no session can be
            // created and no existing one can offer a command, so every feature
            // that depends on a live link fails while the server reports healthy.
            // It was logged at the same level as success, one line among hundreds,
            // which is how a stale binary with no adapters presented as a UI bug.
            if usable == 0 {
                tracing::error!(
                    "No usable agents. Sessions cannot run and no slash commands will be \
                     offered. Bundled adapters were looked for in {:?}; your own agents in \
                     {:?}. Run `sylph-web doctor` to see what resolved.",
                    config.bundled_agents_candidates,
                    config.agents_dir,
                );
            }
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

        // Batches the journal's hot path. One instance, shared: the drain task
        // buffers a turn's deltas and the supervisor flushes them before the fold,
        // so separate writers would leave the fold reading a buffer nobody flushed.
        let writer = crate::db::JournalWriter::new(store.clone());

        let links = crate::links::LinkManager::new(
            store.clone(),
            writer.clone(),
            hub.clone(),
            environment,
            registry.clone(),
            soul_path,
            seq.clone(),
        );

        // Turns on a live link. The two reference each other — the supervisor
        // asks for a link, the manager settles the supervisor's turns — so the
        // waiter registry is handed over after both exist.
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
        let tracedb = TraceDbClient::new(config.tracedb_bin.clone());

        // Retention needs the archive client, so it is built after it. The
        // environment kind is `local` because that is the only environment today;
        // it is passed rather than assumed inside so a per-session environment
        // later replaces an argument instead of reaching into the scheduler.
        let retention = crate::storage::RetentionScheduler::new(
            store.clone(),
            history.clone(),
            tracedb.clone(),
            crate::env::Environment::descriptor(&crate::env::LocalEnvironment::new()).kind,
        );
        retention.spawn();

        Ok(Self {
            store,
            config,
            registry,
            hub,
            turns,
            links,
            history,
            consolidator,
            retention,
            tracedb,
        })
    }
}

/// Largest request body the API accepts.
///
/// `docs/api-design.md` caps a turn at 10 MiB of attachments, and base64
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
        .route(
            "/api/v1/agents",
            get(api::agents::list).post(api::agents::refresh),
        )
        .route(
            "/api/v1/sessions",
            get(api::sessions::list).post(api::sessions::create),
        )
        .route("/api/v1/sessions/import", post(api::sessions::import))
        .route(
            "/api/v1/sessions/{id}",
            get(api::sessions::get)
                .patch(api::sessions::update)
                .delete(api::sessions::delete_empty),
        )
        .route(
            "/api/v1/sessions/{id}/turns",
            get(api::sessions::history).post(api::turns::create),
        )
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
        .route("/api/v1/archive/sessions", get(api::archive::list))
        // Greedy capture, not `{id}`: a subagent session id contains a slash
        // (`claude:uuid/agent-name`), which is 43% of the local archive.
        .route(
            "/api/v1/archive/sessions/{*trace_id}",
            get(api::archive::transcript),
        )
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
