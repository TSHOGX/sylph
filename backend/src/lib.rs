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
pub mod error;
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
use crate::supervisor::TurnSupervisor;
use crate::tracedb::Client as TraceDbClient;

#[derive(Clone)]
pub struct AppState {
    pub store: LiveStore,
    pub config: Config,
    pub registry: Arc<Mutex<Registry>>,
    pub hub: EventHub,
    pub supervisor: TurnSupervisor,
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

        let registry = Arc::new(Mutex::new(Registry::new(&config.runners_dir).await?));
        {
            let registry = registry.lock().await;
            tracing::info!(
                "Runner discovery: {} agents found, {} broken",
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

        // An explicit empty SYLPH_SOUL disables injection, which is how the
        // runner protocol spells "no identity" and how tests keep runs cheap.
        let soul_path = std::env::var("SYLPH_SOUL")
            .ok()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from);

        let hub = EventHub::new();
        let supervisor =
            TurnSupervisor::new(store.clone(), registry.clone(), hub.clone(), soul_path);
        let tracedb = TraceDbClient::new(config.tracedb_bin.clone());

        Ok(Self {
            store,
            config,
            registry,
            hub,
            supervisor,
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
            get(api::sessions::get).patch(api::sessions::update),
        )
        .route(
            "/api/v1/sessions/{id}/turns",
            get(api::sessions::history).post(api::turns::create),
        )
        // Files before `{id}` would be ambiguous the other way round: axum
        // matches literal segments before captures, so the more specific
        // `files/content` route must be registered, but order among these three
        // does not matter. They are grouped for readability.
        .route(
            "/api/v1/sessions/{id}/files",
            get(api::files::tree).post(api::files::upload),
        )
        .route(
            "/api/v1/sessions/{id}/files/content",
            get(api::files::content),
        )
        // Machine-level directory browsing, bounded by configured roots rather
        // than by a session. Separate from the `sessions/{id}/files` tree
        // because the root differs: see `api::fs` for the threat model.
        .route("/api/v1/fs/roots", get(api::fs::roots))
        .route("/api/v1/fs/tree", get(api::fs::tree))
        .route("/api/v1/workspaces", get(api::workspaces::list))
        .route("/api/v1/search", get(api::search::search))
        .route("/api/v1/archive/sessions", get(api::archive::list))
        // Greedy capture, not `{id}`: a subagent session id contains a slash
        // (`claude:uuid/agent-name`), which is 43% of the local archive.
        .route(
            "/api/v1/archive/sessions/{*trace_id}",
            get(api::archive::transcript),
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
