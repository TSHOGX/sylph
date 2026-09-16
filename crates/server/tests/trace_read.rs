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
async fn activation_pages_preserve_own_outcomes_and_snapshot_membership() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let agent = bootstrap["header"]["agent"]["address"].as_str().unwrap();
    let token = bootstrap["view"]["token"].as_str().unwrap();
    let path = format!("/api/v1/trace/agents/{agent}/activations");
    let first = backend
        .get_authed(&format!("{path}?at_snapshot={token}&limit=1"))
        .await;
    assert_eq!(first.status(), 200, "{}", body_string(first).await);
    let first = json_body(
        backend
            .get_authed(&format!("{path}?at_snapshot={token}&limit=1"))
            .await,
    )
    .await;
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["total"]["scope"], "own activations");
    assert_eq!(first["total"]["value"], 1);
    assert_eq!(first["items"][0]["state"]["kind"], "terminated");
    assert_eq!(first["items"][0]["state"]["outcome"], "succeeded");
    let initial = first["items"][0]["reference"].as_str().unwrap();
    let detail = json_body(
        backend
            .get_authed(&format!(
                "/api/v1/trace/objects/{initial}?at_snapshot={token}"
            ))
            .await,
    )
    .await;
    assert_eq!(detail["execution"], first["items"][0]);

    for key in ["follow-up-one", "follow-up-two"] {
        let response = backend
            .post_authed(
                "/api/v1/trace/commands",
                json!({
                    "target":{"kind":"session","agent":agent,"sessionId":id},
                    "action":"send","idempotencyKey":key,"text":key,"attachments":[]
                }),
            )
            .await;
        assert_eq!(response.status(), 202, "{}", body_string(response).await);
        backend.state.trace.project_pending().await.unwrap();
    }
    let latest = json_body(backend.get_authed(&format!("{path}?limit=1")).await).await;
    assert_eq!(latest["total"]["value"], 3);
    assert_eq!(latest["items"].as_array().unwrap().len(), 1);
    assert_eq!(latest["hasAfter"], true);
    let view = latest["view"]["token"].as_str().unwrap();
    let mut references = std::collections::BTreeSet::from([latest["items"][0]["reference"]
        .as_str()
        .unwrap()
        .to_owned()]);
    let mut next = latest["afterCursor"].as_str().map(str::to_owned);
    while let Some(cursor) = next {
        let page = json_body(
            backend
                .get_authed(&format!("{path}?at_snapshot={view}&limit=1&after={cursor}"))
                .await,
        )
        .await;
        assert_eq!(page["view"], latest["view"]);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert!(references.insert(page["items"][0]["reference"].as_str().unwrap().to_owned()));
        next = page["afterCursor"]
            .as_str()
            .map(str::to_owned)
            .filter(|_| page["hasAfter"] == true);
    }
    assert_eq!(references.len(), 3);
    let retained = json_body(
        backend
            .get_authed(&format!("{path}?at_snapshot={token}&limit=1"))
            .await,
    )
    .await;
    assert_eq!(
        retained["items"], first["items"],
        "later activations cannot enter an old snapshot"
    );
    assert_eq!(retained["total"], first["total"]);
    assert_eq!(retained["view"], first["view"]);
    // New temporal rows may require another bounded candidate scan, but none
    // may become visible through the original snapshot's continuation.
    let mut continuation = retained;
    for _ in 0..10 {
        if continuation["hasAfter"] != true {
            break;
        }
        let cursor = continuation["afterCursor"].as_str().unwrap();
        continuation = json_body(
            backend
                .get_authed(&format!(
                    "{path}?at_snapshot={token}&limit=1&after={cursor}"
                ))
                .await,
        )
        .await;
        assert!(continuation["items"].as_array().unwrap().is_empty());
    }
    assert_eq!(continuation["hasAfter"], false);
    let history_cursor = latest["afterCursor"].as_str().unwrap();
    assert_eq!(
        backend
            .get_authed(&format!(
                "/api/v1/trace/agents/{agent}/history?at_snapshot={view}&after={history_cursor}"
            ))
            .await
            .status(),
        400
    );
    backend
        .delete_authed(&format!("/api/v1/sessions/{id}"))
        .await;
    assert_eq!(
        backend
            .get_authed(&format!("{path}?at_snapshot={token}"))
            .await
            .status(),
        410
    );
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

fn changes_url(bootstrap: &serde_json::Value, views: serde_json::Value, cursor: &str) -> String {
    let query = [
        (
            "interests",
            json!([bootstrap["header"]["agent"]["address"]]).to_string(),
        ),
        ("views", views.to_string()),
        ("cursor", cursor.into()),
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
    format!("/api/v1/trace/changes?{query}")
}
fn interest(bootstrap: &serde_json::Value) -> serde_json::Value {
    json!({"agent":bootstrap["header"]["agent"]["address"],"atSnapshot":bootstrap["view"]["token"]})
}
async fn next_change(response: axum::response::Response) -> serde_json::Value {
    use futures::StreamExt;
    assert_eq!(response.status(), 200);
    let mut body = response.into_body().into_data_stream();
    tokio::time::timeout(std::time::Duration::from_secs(3), async move {
        while let Some(data) = body.next().await {
            let data = data.unwrap();
            {
                let text = std::str::from_utf8(&data).unwrap();
                if let Some(data) = text.lines().find_map(|line| {
                    line.strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"))
                }) {
                    return serde_json::from_str(data).unwrap();
                }
            }
        }
        panic!("stream ended without a trace change")
    })
    .await
    .expect("a replay or explicit restart must arrive without a later producer event")
}

#[tokio::test]
async fn change_replay_names_committed_objects_and_accepts_multiple_views_of_one_agent() {
    use sylph_protocol::trace::*;
    use sylph_server::trace::generation::{ObjectUpdate, ProjectionBatch};
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let before = boot(&backend, &id).await;
    let generation = backend.state.trace.active().await.unwrap();
    let checkpoint = generation.checkpoint().await.unwrap();
    let agent = AgentRef::Session(TestBackend::session_id(&id));
    generation
        .apply(
            checkpoint.fence_token,
            &checkpoint.snapshot,
            &ProjectionBatch {
                through_cut: checkpoint.snapshot.evidence_cut,
                continuation: true,
                objects: vec![ObjectUpdate::put(
                    TraceObject::Agent(AgentThread {
                        agent_ref: agent.clone(),
                        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                        name: Some("Renamed from retained evidence".into()),
                        model: None,
                    }),
                    "Renamed from retained evidence",
                )],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let after = boot(&backend, &id).await;
    let cursor = before["changeCursor"].as_str().unwrap();
    let url = changes_url(
        &before,
        json!([interest(&before), interest(&after)]),
        cursor,
    );
    let event = next_change(backend.get_authed(&url).await).await;
    assert_eq!(event["kind"], "revision");
    assert_eq!(event["viewRevision"], after["view"]["viewRevision"]);
    assert_eq!(
        event["changed"],
        json!([TraceAddress::of(&AnyObjectRef::Agent(agent))])
    );
}

#[tokio::test]
async fn change_cursor_from_a_replaced_generation_requires_an_explicit_restart() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let before = boot(&backend, &id).await;
    backend.state.trace.rebuild().await.unwrap();
    let event = next_change(
        backend
            .get_authed(&changes_url(
                &before,
                json!([interest(&before)]),
                before["changeCursor"].as_str().unwrap(),
            ))
            .await,
    )
    .await;
    assert_eq!(event["kind"], "reset_required");
    assert!(event["reason"].as_str().unwrap().contains("generation"));
}

#[tokio::test]
async fn a_future_change_cursor_cannot_skip_committed_revisions() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let cursor = format!(
        "{}:{}",
        bootstrap["view"]["generation"].as_str().unwrap(),
        bootstrap["view"]["viewRevision"].as_u64().unwrap() + 100
    );
    let event = next_change(
        backend
            .get_authed(&changes_url(
                &bootstrap,
                json!([interest(&bootstrap)]),
                &cursor,
            ))
            .await,
    )
    .await;
    assert_eq!(event["kind"], "reset_required");
}

#[tokio::test]
async fn an_open_change_stream_stops_when_its_retained_scope_is_deleted() {
    let backend = TestBackend::start().await;
    let id = backend.create_session("codex").await;
    let bootstrap = boot(&backend, &id).await;
    let response = backend
        .get_authed(&changes_url(
            &bootstrap,
            json!([interest(&bootstrap)]),
            bootstrap["changeCursor"].as_str().unwrap(),
        ))
        .await;
    assert_eq!(
        backend
            .delete_authed(&format!("/api/v1/sessions/{id}"))
            .await
            .status(),
        204
    );
    let event = next_change(response).await;
    assert_eq!(event["kind"], "access_changed");
}
