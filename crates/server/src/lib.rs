//! Sylph server composition. Operational policy and trace evidence have independent writers.
use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::trace::TraceLayer;
pub mod api;
pub mod attachments;
pub mod config;
pub mod db;
pub mod documents;
pub mod env;
pub mod error;
pub mod link_state;
pub mod middleware;
pub mod models;
pub mod paths;
pub mod trace;
use config::Config;
use db::ControlStore;
use error::Error;

#[derive(Clone)]
pub struct AppState {
    pub store: ControlStore,
    pub config: Config,
    pub trace: Arc<trace::service::TraceService>,
    pub native_index: api::native_index::NativeIndex,
    pub env_bridge: Arc<env::EnvBridge>,
    pub exporter: trace::exporter::OutboxExporter,
}
/// Periodic work may retain durable handles but never owns the product lifetime.
#[derive(Clone)]
pub(crate) struct WeakAppState {
    store: ControlStore,
    config: Config,
    trace: std::sync::Weak<trace::service::TraceService>,
    native_index: api::native_index::NativeIndex,
    env_bridge: std::sync::Weak<env::EnvBridge>,
    exporter: trace::exporter::OutboxExporter,
}
impl WeakAppState {
    pub(crate) fn upgrade(&self) -> Option<AppState> {
        Some(AppState {
            store: self.store.clone(),
            config: self.config.clone(),
            trace: self.trace.upgrade()?,
            native_index: self.native_index.clone(),
            env_bridge: self.env_bridge.upgrade()?,
            exporter: self.exporter.clone(),
        })
    }
}

impl AppState {
    pub(crate) fn downgrade(&self) -> WeakAppState {
        WeakAppState {
            store: self.store.clone(),
            config: self.config.clone(),
            trace: Arc::downgrade(&self.trace),
            native_index: self.native_index.clone(),
            env_bridge: Arc::downgrade(&self.env_bridge),
            exporter: self.exporter.clone(),
        }
    }

    pub async fn build(config: Config) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&config.data_dir).await?;
        let trace = Arc::new(trace::service::TraceService::open(&config.data_dir).await?);
        let store = ControlStore::open(
            &config.control_path,
            sylph_protocol::trace::Issuer::Server("sylph-server".into()),
            db::control::OutboxLimits::default(),
        )
        .await?;
        store
            .upsert_subject(
                "bootstrap",
                "Bootstrap",
                &middleware::auth::hash_token_for_storage(&config.cookie_secret),
                true,
            )
            .await?;
        store.recover_inflight(models::now_ms()).await?;
        store.reset_runtime_connections().await?;
        let exporter =
            trace::exporter::OutboxExporter::new(store.clone(), trace.uploads().clone())?;
        let env_bridge = Arc::new(env::EnvBridge::new(store.clone(), trace.clone()));
        env_bridge.spawn_health_reaper();
        let state = Self {
            store,
            config,
            trace,
            native_index: api::native_index::NativeIndex::new(),
            env_bridge,
            exporter,
        };
        state.trace.spawn();
        let exporter = state.exporter.clone();
        let trace = Arc::downgrade(&state.trace);
        let control = state.store.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let Some(trace) = trace.upgrade() else {
                    break;
                };
                match exporter.run_once().await {
                    Ok(progress) if progress.transferred > 0 => trace.notify_admission(),
                    Ok(_) => {}
                    Err(error) => tracing::error!(%error,"operational evidence export failed"),
                };
                if let Err(error) = control.erase_pending_inputs().await {
                    tracing::error!(%error,"control input deletion failed");
                }
            }
        });
        api::imports::recover_state(&state.store).await?;
        api::imports::spawn_sync(state.clone());
        api::controls::spawn_dispatcher(state.clone());
        Ok(state)
    }
}
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(api::health::health))
        .route("/api/v1/auth/token", get(middleware::auth::bootstrap_token))
        .route("/api/v1/auth/register", post(middleware::auth::register))
        .route("/api/v1/auth/login", post(middleware::auth::login))
        .route("/api/v1/auth/logout", post(middleware::auth::logout))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
        .route("/api/v1/config", get(api::config::get))
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
            "/api/v1/environments/{id}/grants/{subject}",
            axum::routing::delete(api::environments::revoke_grant),
        )
        .route(
            "/api/v1/environments/{id}/grants",
            post(api::environments::issue_grant),
        )
        .route(
            "/api/v1/environments/{id}/invites",
            post(api::environments::issue_invite),
        )
        .route(
            "/api/v1/environments/{id}/invites/redeem",
            post(api::environments::redeem_invite),
        )
        .route(
            "/api/v1/sessions",
            get(api::sessions::list).post(api::sessions::create),
        )
        .route(
            "/api/v1/sessions/draft/{id}",
            get(api::sessions::prepared_get).delete(api::links::discard_prepared),
        )
        .route(
            "/api/v1/sessions/draft/{id}/commands",
            post(api::controls::create_prepared),
        )
        .route(
            "/api/v1/sessions/draft/{id}/capabilities",
            get(api::links::prepared_capabilities),
        )
        .route(
            "/api/v1/sessions/draft/{id}/config",
            post(api::links::prepared_set_config),
        )
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
        .route("/api/v1/sessions/adopt", post(api::sessions::adopt))
        .route("/api/v1/sessions/enumerate", get(api::sessions::enumerate))
        .route(
            "/api/v1/sessions/{id}",
            get(api::sessions::get)
                .patch(api::sessions::update)
                .delete(api::sessions::delete),
        )
        .route(
            "/api/v1/sessions/{id}/commands",
            get(api::controls::list).post(api::controls::create),
        )
        .route("/api/v1/commands/{id}", get(api::controls::get))
        .route("/api/v1/sessions/{id}/reread", post(api::sessions::reread))
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
        .route(
            "/api/v1/sessions/{id}/files",
            get(api::files::tree)
                .post(api::files::upload)
                .delete(api::files::remove),
        )
        .route(
            "/api/v1/sessions/{id}/files/directory",
            post(api::files::mkdir),
        )
        .route(
            "/api/v1/sessions/{id}/files/content",
            get(api::files::content),
        )
        .route(
            "/api/v1/sessions/{id}/files/document",
            get(api::files::document),
        )
        .route("/api/v1/sessions/{id}/files/raw", get(api::files::raw))
        .route(
            "/api/v1/environments/{environment}/imports",
            post(api::imports::create),
        )
        .route(
            "/api/v1/environments/{environment}/imports/list",
            get(api::imports::list),
        )
        .route("/api/v1/imports/{id}", get(api::imports::get))
        .route("/api/v1/imports/{id}/items", get(api::imports::items))
        .route("/api/v1/imports/{id}/events", get(api::imports::events))
        .route("/api/v1/imports/{id}/cancel", post(api::imports::cancel))
        .route("/api/v1/imports/{id}/retry", post(api::imports::retry))
        .route("/api/v1/fs/roots", get(api::fs::roots))
        .route("/api/v1/fs/tree", get(api::fs::tree))
        .route("/api/v1/soul", get(api::soul::get))
        .route("/api/v1/workspaces", get(api::workspaces::list))
        .merge(api::trace::router())
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
