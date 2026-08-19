use axum::{
    routing::get,
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod api;
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

#[derive(Clone)]
pub struct AppState {
    pub store: LiveStore,
    pub config: Config,
    pub registry: Arc<Mutex<Registry>>,
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
    let registry = Registry::new(&config.runners_dir).await?;
    let agent_count = registry.list_agents().len();
    let broken_count = registry.broken_runners().len();
    tracing::info!(
        "Runner discovery: {} agents found, {} broken",
        agent_count,
        broken_count
    );

    // Build application state
    let state = AppState {
        store,
        config: config.clone(),
        registry: Arc::new(Mutex::new(registry)),
    };

    // Build router
    let app = Router::new()
        .route("/api/v1/health", get(api::health::health))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
        .route("/api/v1/agents", get(api::agents::list).post(api::agents::refresh))
        .route("/api/v1/sessions", get(api::sessions::list).post(api::sessions::create))
        .route("/api/v1/sessions/{id}", get(api::sessions::get))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start server
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    tracing::info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
