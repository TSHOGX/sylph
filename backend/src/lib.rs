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
pub mod supervisor;
pub mod tracedb;

use crate::config::Config;
use crate::error::Error;
use crate::db::LiveStore;
use crate::events::EventHub;
use crate::runners::Registry;
use crate::tracedb::Client as TraceDbClient;

#[derive(Clone)]
pub struct AppState {
    pub store: LiveStore,
    pub config: Config,
    pub registry: Arc<Mutex<Registry>>,
    pub hub: EventHub,
    /// Runs turns on live links.
    pub turns: crate::supervisor::turn::LinkedSupervisor,
    pub links: crate::links::LinkManager,
    pub tracedb: TraceDbClient,
}

impl AppState {
    /// Assemble application state from configuration.
    ///
    /// Runner discovery and the TraceDB probe are both non-fatal: a missing
    /// runners directory or a misconfigured TraceDB binary leaves the backend
    /// serving what it can, with the failure visible in `/diagnostics`.
    pub async fn build(config: Config) -> anyhow::Result<Self> {
        let store = LiveStore::new(&config.database_path).await?;
        tracing::info!("Database initialized at {}", config.database_path);

        // Bundled adapters first, so a user-installed one of the same name
        // shadows rather than duplicates. Discovery handshakes with each, which
        // is why it needs the environment.
        let discovery_env: Arc<dyn crate::env::Environment> =
            Arc::new(crate::env::LocalEnvironment::new());
        let registry = Arc::new(Mutex::new(
            Registry::new(
                vec![config.agents_dir.clone(), config.runners_dir.clone()],
                discovery_env,
            )
            .await?,
        ));
        {
            let registry = registry.lock().await;
            tracing::info!(
                "Agent discovery: {} usable, {} broken",
                registry.list_agents().len(),
                registry.broken_runners().len()
            );
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

        let links = crate::links::LinkManager::new(
            store.clone(),
            hub.clone(),
            environment,
            config.agents_dir.clone(),
            soul_path,
            seq.clone(),
        );

        // Turns on a live link. The two reference each other — the supervisor
        // asks for a link, the manager settles the supervisor's turns — so the
        // waiter registry is handed over after both exist.
        let turns = crate::supervisor::turn::LinkedSupervisor::new(
            store.clone(),
            hub.clone(),
            links.clone(),
            seq,
        );
        links.set_waiters(turns.waiters()).await;
        let tracedb = TraceDbClient::new(config.tracedb_bin.clone());

        Ok(Self {
            store,
            config,
            registry,
            hub,
            turns,
            links,
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
