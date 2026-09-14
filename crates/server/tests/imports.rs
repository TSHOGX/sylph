mod common;

use axum::http::StatusCode;
use common::{json_body, MockAgents, TestBackend};
use serde_json::{json, Value};
use std::time::Duration;
use sylph_protocol::SessionId;

async fn import(backend: &TestBackend, mode: &str, key: &str) -> Value {
    let environment = backend.environment_id().await;
    let response = backend
        .post_authed(
            &format!("/api/v1/environments/{environment}/imports"),
            json!({
                "mode": mode, "agents": ["mock"], "workdir": backend.workdir(),
                "idempotencyKey": key,
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let created = json_body(response).await;
    let id = created["job"]["id"].as_str().unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current =
                json_body(backend.get_authed(&format!("/api/v1/imports/{id}")).await).await;
            match current["job"]["status"].as_str().unwrap() {
                "queued" | "running" => tokio::time::sleep(Duration::from_millis(10)).await,
                "completed" => {
                    assert_eq!(current["job"]["failed"], 0, "{current}");
                    return current["job"].clone();
                }
                _ => panic!("import must finish its finite pass: {current}"),
            }
        }
    })
    .await;
    match finished {
        Ok(job) => job,
        Err(_) => {
            let job = json_body(backend.get_authed(&format!("/api/v1/imports/{id}")).await).await;
            panic!("small native imports must complete: {job}");
        }
    }
}

async fn stored_session(backend: &TestBackend) -> String {
    format!(
        "{}:mock:{}",
        backend.environment_id().await,
        MockAgents::stored_native_id()
    )
}

async fn transcript(backend: &TestBackend) -> Value {
    let session = stored_session(backend).await;
    json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{session}/turns"))
            .await,
    )
    .await
}

#[tokio::test]
async fn keep_syncing_existing_sessions_survives_restart_without_reimporting_unchanged_history() {
    let mut backend = TestBackend::start().await;
    let first = import(&backend, "import_once", "once").await;
    assert_eq!(first["imported"], 1);
    let initial_reads = backend.agents.native_read_count();
    let following = import(&backend, "import_and_watch", "follow").await;
    assert_eq!(following["refreshed"], 1);
    assert_eq!(backend.agents.native_read_count(), initial_reads);
    assert_eq!(backend.agents.native_watch_count(), 0);

    let reads = backend.agents.native_read_count();
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    assert_eq!(backend.agents.native_read_count(), reads);

    backend
        .agents
        .grow_stored_session("written during keep syncing");
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        1
    );
    assert_eq!(backend.agents.native_read_count(), reads + 1);
    assert!(transcript(&backend)
        .await
        .to_string()
        .contains("written during keep syncing"));

    backend.restart().await;
    let reads = backend.agents.native_read_count();
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    assert_eq!(backend.agents.native_read_count(), reads);
    backend
        .agents
        .grow_stored_session("written after both processes restarted");
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        1
    );
    assert!(transcript(&backend)
        .await
        .to_string()
        .contains("written after both processes restarted"));
    assert_eq!(backend.agents.native_watch_count(), 0);
}

#[tokio::test]
async fn later_import_once_stops_syncing_the_same_snapshot() {
    let backend = TestBackend::start().await;
    import(&backend, "import_and_watch", "follow").await;
    import(&backend, "import_once", "stop-following").await;
    let reads = backend.agents.native_read_count();
    backend
        .agents
        .grow_stored_session("outside the retained synchronization intent");
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    assert_eq!(backend.agents.native_read_count(), reads);
    assert!(!transcript(&backend)
        .await
        .to_string()
        .contains("outside the retained synchronization intent"));
}

#[tokio::test]
async fn synchronization_discovers_new_scope_members_and_shares_one_catalogue() {
    let backend = TestBackend::start().await;
    backend.agents.add_stored_session("also-authorized");
    let initial = import(&backend, "import_and_watch", "authorized-snapshot").await;
    assert_eq!(initial["imported"], 2);
    backend.agents.add_stored_session("discovered-later");
    let reads = backend.agents.native_read_count();
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        1
    );
    assert!(backend.agents.native_read_count() > reads);
    let unrequested = SessionId::decode(&format!(
        "{}:mock:discovered-later",
        backend.environment_id().await,
    ))
    .unwrap();
    assert!(backend
        .state
        .store
        .get_session(&unrequested)
        .await
        .unwrap()
        .is_some());
    // Once adopted, discovery and refresh share one catalogue and no body re-read.
    let reads = backend.agents.native_read_count();
    let enumerations = backend.agents.native_enumeration_count();
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    assert_eq!(backend.agents.native_read_count(), reads);
    assert_eq!(backend.agents.native_enumeration_count(), enumerations + 1);
}

#[tokio::test]
async fn partial_native_reads_remain_failed_until_the_same_revision_can_be_read() {
    let backend = TestBackend::start().await;
    let job = import(&backend, "import_and_watch", "read-error").await;
    backend
        .agents
        .grow_stored_session("present alongside the unreadable record");
    backend.agents.set_native_read_error(true);
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    let current = json_body(
        backend
            .get_authed(&format!("/api/v1/imports/{}", job["id"].as_str().unwrap(),))
            .await,
    )
    .await;
    assert_eq!(current["job"]["failed"], 1);
    backend.agents.set_native_read_error(false);
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        1
    );
    let current = json_body(
        backend
            .get_authed(&format!("/api/v1/imports/{}", job["id"].as_str().unwrap(),))
            .await,
    )
    .await;
    assert_eq!(current["job"]["failed"], 0);
}

#[tokio::test]
async fn an_empty_subscription_discovers_later_sessions_only_in_its_exact_workdir() {
    let backend = TestBackend::start().await;
    let other = format!("{}-neighbour", backend.workdir().display());
    backend.agents.set_store_workdir(Some(other.clone()));
    let initial = import(&backend, "import_and_watch", "empty-workspace").await;
    assert_eq!(initial["total"], 0);
    backend.agents.add_stored_session("later-member");
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        0
    );
    backend
        .agents
        .set_store_workdir(Some(backend.workdir().display().to_string()));
    assert_eq!(
        sylph_server::api::imports::sync_once(&backend.state)
            .await
            .unwrap(),
        2
    );
    let id = SessionId::decode(&format!(
        "{}:mock:later-member",
        backend.environment_id().await
    ))
    .unwrap();
    assert!(backend
        .state
        .store
        .get_session(&id)
        .await
        .unwrap()
        .is_some());
}
