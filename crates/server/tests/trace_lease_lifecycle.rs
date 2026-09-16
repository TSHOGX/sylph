//! Request failures release new views; successful responses transfer their ownership.
mod common;

use axum::http::StatusCode;
use common::{json_body, TestBackend};
use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection};
use sylph_protocol::trace::AgentRef;

async fn lease_reader(backend: &TestBackend) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(backend.state.config.data_dir.join("evidence.db"))
            .read_only(true),
    )
    .await
    .unwrap()
}

async fn live_views(reader: &mut SqliteConnection) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM trace_view_leases WHERE subject='bootstrap' AND state IN ('pending','active') AND expires_at_ms>?")
        .bind(sylph_server::models::now_ms())
        .fetch_one(reader).await.unwrap()
}

#[tokio::test]
async fn repeated_invalid_searches_do_not_exhaust_bootstrap_view_capacity() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    backend.state.trace.project_pending().await.unwrap();
    backend.state.trace.index_pending().await.unwrap();
    let mut reader = lease_reader(&backend).await;
    assert_eq!(live_views(&mut reader).await, 0);

    // The real API creates a view before FTS validates its query. More than one
    // subject's permitted view count exposes failed-request ownership leaks.
    for attempt in 0..70 {
        let response = backend.get_authed("/api/v1/trace/search?q=%22").await;
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "attempt {attempt}: {body}");
        assert_eq!(body["error"]["code"], "invalid_search_query");
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while live_views(&mut reader).await != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed searches must release all their views");

    let agent = AgentRef::Session(TestBackend::session_id(&id)).encode();
    let response = backend
        .get_authed(&format!("/api/v1/trace/agents/{agent}"))
        .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["view"]["token"].as_str().unwrap();
    assert_eq!(
        live_views(&mut reader).await,
        1,
        "a returned bootstrap owns its view"
    );
    let borrowed = backend
        .get_authed(&format!("/api/v1/trace/agents/{agent}?at_snapshot={token}"))
        .await;
    assert_eq!(borrowed.status(), StatusCode::OK);
    drop(borrowed);
    assert_eq!(
        live_views(&mut reader).await,
        1,
        "borrowing creates no new view"
    );
    assert_eq!(
        backend
            .delete_authed(&format!("/api/v1/trace/views/{token}"))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(live_views(&mut reader).await, 0);
}
