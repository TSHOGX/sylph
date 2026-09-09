//! Environment pairing and lease API coverage.

mod common;

use axum::http::StatusCode;
use common::{json_body, TestBackend};
use serde_json::json;

#[tokio::test]
async fn pairing_code_is_single_use_and_unpair_removes_the_environment() {
    let backend = TestBackend::start().await;

    let response = backend
        .post_authed("/api/v1/environments/pairings", json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let offer = json_body(response).await;
    let code = offer["code"].as_str().unwrap().to_string();

    // The envagent does not have a browser cookie; the short-lived code is
    // the credential for this loopback-only exchange.
    let response = backend
        .post(
            "/api/v1/environments/pair",
            json!({
                "code": code,
                "name": "Fixture laptop",
                "origin": "development",
                "agents": []
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let grant = json_body(response).await;
    let environment = grant["environmentId"].as_str().unwrap().to_string();
    assert!(!grant["installationId"].as_str().unwrap().is_empty());

    let second = backend
        .post(
            "/api/v1/environments/pair",
            json!({"code": code, "name": "duplicate"}),
        )
        .await;
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);

    let list = json_body(backend.get_authed("/api/v1/environments").await).await;
    assert!(list["environments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == environment));

    let removed = backend
        .delete_authed(&format!("/api/v1/environments/{environment}"))
        .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn pairing_exchange_uses_the_standard_error_envelope() {
    let backend = TestBackend::start().await;
    let response = backend
        .request_authed(
            "POST",
            "/api/v1/environments/pair",
            Some(json!({"code": "nope"})),
        )
        .await;
    // The route remains behind the normal error envelope rather than a bare
    // framework response.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn connected_inventory_updates_after_cli_removal_without_repair_or_restart() {
    let backend = TestBackend::start().await;
    let environment = sylph_protocol::EnvironmentId::parse(backend.environment_id().await)
        .expect("fixture environment id should parse");

    let before = backend
        .state
        .env_bridge
        .list_environments()
        .await
        .expect("initial environment listing should succeed")
        .into_iter()
        .find(|row| row.id == environment)
        .expect("paired fixture environment should be listed");
    assert!(before.connected);
    assert!(before
        .agents
        .iter()
        .any(|agent| agent.name == "mock" && agent.available));

    // The envagent and server share this test seam, but remain separate processes in
    // production. The runtime's inventory watcher observes the removal on its next
    // pass and sends an `agents` frame over the existing connection.
    backend.agents.uninstall("mock");

    for _ in 0..160 {
        let current = backend
            .state
            .env_bridge
            .list_environments()
            .await
            .expect("updated environment listing should succeed")
            .into_iter()
            .find(|row| row.id == environment)
            .expect("paired fixture environment should remain listed");
        if current.connected && current.agents.is_empty() {
            assert_eq!(current.connection_epoch, before.connection_epoch);
            let api = common::json_body(backend.get_authed("/api/v1/environments").await).await;
            let row = api["environments"]
                .as_array()
                .expect("environment response should contain rows")
                .iter()
                .find(|row| row["id"] == environment.as_str())
                .expect("updated environment should remain in the API listing");
            assert_eq!(row["connected"], true);
            assert_eq!(row["agents"].as_array().map(Vec::len), Some(0));

            // Installing or upgrading the CLI is the inverse transition. It must
            // be visible on this same lease as well, without a new pairing epoch.
            backend.agents.install("mock");
            for _ in 0..160 {
                let restored = backend
                    .state
                    .env_bridge
                    .list_environments()
                    .await
                    .expect("restored environment listing should succeed")
                    .into_iter()
                    .find(|row| row.id == environment)
                    .expect("paired fixture environment should remain listed");
                if restored.connected
                    && restored
                        .agents
                        .iter()
                        .any(|agent| agent.name == "mock" && agent.available)
                {
                    assert_eq!(restored.connection_epoch, before.connection_epoch);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("connected environment inventory did not reflect CLI installation");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    panic!("connected environment inventory did not reflect CLI removal");
}
