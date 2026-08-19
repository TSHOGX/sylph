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

    if let Some(version) = state.tracedb.probe().await {
        tracing::info!("TraceDB available: {}", version);
    } else {
        tracing::warn!(
            "TraceDB unavailable; native session import and cross-agent search will fail"
        );
    }

    let app = router(state);

    // Loopback only. Binding elsewhere requires the authentication and
    // threat-model change described in docs/architecture.md.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}
