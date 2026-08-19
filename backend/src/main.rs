use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod api;
mod attachments;
mod config;
mod db;
mod error;
mod events;
mod middleware;
mod models;
mod runners;
mod supervisor;

mod tracedb;

use crate::config::Config;
use crate::db::LiveStore;
use crate::runners::Registry;

use crate::events::EventHub;
use crate::supervisor::TurnSupervisor;

#[derive(Clone)]
pub struct AppState {
    pub store: LiveStore,
    pub config: Config,
    pub registry: Arc<Mutex<Registry>>,
    pub hub: EventHub,
    pub supervisor: TurnSupervisor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config = Config::from_env()?;
    tracing::info!("Starting Sylph Web backend on {}:{}", config.host, config.port);

    // Initialize database
    let store = LiveStore::new(&config.database_path).await?;
    tracing::info!("Database initialized at {}", config.database_path);

    // Discover runners
    let registry = Arc::new(Mutex::new(Registry::new(&config.runners_dir).await?));
    let agent_count = registry.lock().await.list_agents().len();
    let broken_count = registry.lock().await.broken_runners().len();
    tracing::info!(
        "Runner discovery: {} agents found, {} broken",
        agent_count,
        broken_count
    );

    // Interrupt turns left running by a previous process.
    let interrupted = store.interrupt_stale_turns().await?;
    if interrupted > 0 {
        tracing::warn!("Interrupted {} stale turns from a previous backend session", interrupted);
    }

    let soul_path = std::env::var("SYLPH_SOUL")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);

    // Event hub for SSE, and supervisor to run turns detached from HTTP.
    let hub = EventHub::new();
    let supervisor = TurnSupervisor::new(store.clone(), registry.clone(), hub.clone(), soul_path);

    // Build application state
    let state = AppState {
        store,
        config: config.clone(),
        registry: registry.clone(),
        hub,
        supervisor,
    };

    // Build router
    let app = Router::new()
        .route("/api/v1/health", get(api::health::health))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
        .route("/api/v1/agents", get(api::agents::list).post(api::agents::refresh))
        .route("/api/v1/sessions", get(api::sessions::list).post(api::sessions::create))
        .route("/api/v1/sessions/{id}/turns", post(api::turns::create))
                .route("/api/v1/sessions/{id}", get(api::sessions::get))
        .route("/api/v1/turns/{id}", get(api::turns::get))
        .route("/api/v1/turns/{id}/events", get(api::turns::events))
        .route("/api/v1/turns/{id}/stop", post(api::turns::stop))
                .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    tracing::info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
