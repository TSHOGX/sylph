mod common;
use common::{body_string, json_body, TestBackend};
use serde_json::json;

async fn boot(backend: &TestBackend, id: &str) -> serde_json::Value {
    backend.state.trace.project_pending().await.unwrap();
    let agent = sylph_protocol::trace::AgentRef::Session(TestBackend::session_id(id)).encode();
    let response = backend
        .get_authed(&format!("/api/v1/trace/agents/{agent}"))
        .await;
    assert_eq!(response.status(), 200, "{}", body_string(response).await);
    json_body(
        backend
            .get_authed(&format!("/api/v1/trace/agents/{agent}"))
            .await,
    )
    .await
}

#[tokio::test]
async fn reading_deliveries_keep_submitted_bytes_and_payload_access_is_snapshot_anchored() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let units = bootstrap["window"]["items"].as_array().unwrap();
    let human = units
        .iter()
        .find(|row| row["content"]["author"]["kind"] == "human")
        .unwrap();
    assert_eq!(human["content"]["parts"][0]["preview"], "Fixture input");
    assert!(human["activation"].is_null());
    let payload = &human["content"]["parts"][0]["payload"];
    let token = bootstrap["view"]["token"].as_str().unwrap();
    let url=format!("/api/v1/trace/payloads/{}?via_ref={}&at_snapshot={token}&revision={}&representation=submitted&start=0&end=7",payload["payloadRef"].as_str().unwrap(),payload["viaRef"].as_str().unwrap(),payload["bodyRevision"].as_str().unwrap());
    let response = backend.get_authed(&url).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-range"], "bytes 0-6/13");
    assert_eq!(body_string(response).await, "Fixture");
    let wrong = url.replace(
        payload["bodyRevision"].as_str().unwrap(),
        "another-revision",
    );
    assert_ne!(backend.get_authed(&wrong).await.status(), 200);
    backend
        .delete_authed(&format!("/api/v1/sessions/{id}"))
        .await;
    assert_eq!(backend.get_authed(&url).await.status(), 410);
}

#[tokio::test]
async fn search_returns_its_own_revision_and_old_pages_remain_readable() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    backend.state.trace.index_pending().await.unwrap();
    let agent = bootstrap["header"]["agent"]["address"].as_str().unwrap();
    let response = backend
        .get_authed(&format!("/api/v1/trace/search?q=answer&agent={agent}"))
        .await;
    assert_eq!(response.status(), 200, "{}", body_string(response).await);
    let hits = json_body(
        backend
            .get_authed(&format!("/api/v1/trace/search?q=answer&agent={agent}"))
            .await,
    )
    .await;
    assert_eq!(hits["items"].as_array().unwrap().len(), 1);
    assert!(hits["items"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("answer"));
    let view = hits["view"]["token"].as_str().unwrap();
    let anchor = hits["items"][0]["anchor"].as_str().unwrap();
    assert_eq!(
        backend
            .get_authed(&format!(
                "/api/v1/trace/agents/{agent}?at_snapshot={view}&around={anchor}"
            ))
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn an_observer_cannot_reuse_another_subjects_view_or_read_ungranted_trace_metadata() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let registration = backend
        .post("/api/v1/auth/register", json!({"name":"other-reader"}))
        .await;
    let cookie = registration.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_string();
    let who = json_body(registration).await;
    let agent = bootstrap["header"]["agent"]["address"].as_str().unwrap();
    let path = format!("/api/v1/trace/agents/{agent}");
    assert_eq!(backend.get_with_cookie(&path, &cookie).await.status(), 404);
    backend
        .state
        .store
        .issue_grant(
            who["id"].as_str().unwrap(),
            &backend.environment_id().await,
            "observe",
            "bootstrap",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .get_with_cookie(
                &format!(
                    "{path}?at_snapshot={}",
                    bootstrap["view"]["token"].as_str().unwrap()
                ),
                &cookie
            )
            .await
            .status(),
        403
    );
    assert_eq!(backend.get_with_cookie(&path, &cookie).await.status(), 200);
}

#[tokio::test]
async fn change_stream_forbids_proxy_transformation_before_a_revision_arrives() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let agent = bootstrap["header"]["agent"]["address"].as_str().unwrap();
    let query = [
        ("interests", json!([agent]).to_string()),
        (
            "views",
            json!([{ "agent": agent, "atSnapshot": bootstrap["view"]["token"] }]).to_string(),
        ),
        (
            "cursor",
            bootstrap["changeCursor"].as_str().unwrap().to_owned(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| {
        format!(
            "{key}={}",
            percent_encoding::utf8_percent_encode(&value, percent_encoding::NON_ALPHANUMERIC)
        )
    })
    .collect::<Vec<_>>()
    .join("&");
    let response = backend
        .get_authed(&format!("/api/v1/trace/changes?{query}"))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert!(response.headers()["cache-control"]
        .to_str()
        .unwrap()
        .split(',')
        .any(|directive| directive.trim() == "no-transform"));
}
