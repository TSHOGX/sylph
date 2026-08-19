use axum::{
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod api;
mod config;
mod db;
mod error;
mod middleware;
mod models;
mod runners;
mod tracedb;

use crate::config::Config;
use crate::db::LiveStore;

#[derive(Clone)]
pub struct AppState {
    pub store: LiveStore,
    pub config: Config,
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

    // Build application state
    let state = AppState {
        store,
        config: config.clone(),
    };

    // Build router
    let app = Router::new()
        .route("/api/v1/health", get(api::health::health))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
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
