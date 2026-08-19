use std::net::SocketAddr;

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use sylph_web_backend::{api, config::Config, router, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;
    api::health::mark_start();
    tracing::info!(
        "Starting Sylph Web backend on {}:{}",
        config.host,
        config.port
    );

    let port = config.port;
    let state = AppState::build(config).await?;

    match state.tracedb.archive_counts().await {
        Some(counts) => tracing::info!(
            "TraceDB available: {} archived sessions, {} events",
            counts.sessions.unwrap_or(0),
            counts.events.unwrap_or(0)
        ),
        None => tracing::warn!(
            "TraceDB unavailable; native session import and archive search will return \
             no results. Set TRACEDB_BIN if trace-db is installed elsewhere."
        ),
    }

    let app = router(state);

    // Loopback only. Binding elsewhere requires the authentication and
    // threat-model change described in docs/architecture.md.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        // A hand-started local server hits this constantly: an instance from an
        // earlier run is still holding the port. The bare OS message ("Address
        // already in use (os error 48)") does not say which port or what to do
        // about it.
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!(
                "Port {port} is already in use. Another Sylph Web backend is probably \
                 still running — check with `lsof -ti :{port}`, then stop it or start \
                 this one on a different port with SYLPH_WEB_PORT."
            )
        } else {
            anyhow::anyhow!("Could not bind {addr}: {e}")
        }
    })?;
    tracing::info!("Listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}
