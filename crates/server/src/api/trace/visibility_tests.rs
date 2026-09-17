use super::*;
use crate::trace::{generation::ProjectionBatch, reading_store::ReadRowChange};
use axum::{body::Body, http::Request};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    state: AppState,
    app: Router,
    parent: AgentRef,
    child: AgentRef,
    parent_epoch: IngestionEpoch,
    relation_source: AgentRef,
}

fn header(agent: &AgentRef, name: &str, parent: Option<AgentLink>) -> AgentHeader {
    AgentHeader {
        agent: AgentLink {
            address: AgentAddress::of(agent),
            name: Some(name.into()),
            identity_suffix: agent.anchor_session().native_id.as_str().into(),
            vendor: "codex".into(),
        },
        environment: agent.environment().as_str().into(),
        environment_name: None,
        model: None,
        own_execution: ExecutionSummary {
            reference: None,
            state: ReadExecutionState::Unavailable,
            durations: vec![],
        },
        history: HistoryLocation {
            selection: None,
            label: "Retained history".into(),
            is_current: false,
            selection_known: false,
            availability: ReadAvailability::Available,
        },
        parent,
        origin: None,
        coverage: vec![],
        descendant_count: ReadCount {
            value: Some(0),
            scope: "direct spawned agents".into(),
            complete: true,
        },
        continuing_resources: ReadCount {
            value: None,
            scope: "resources".into(),
            complete: false,
        },
        required_input: ReadCount {
            value: None,
            scope: "required input".into(),
            complete: false,
        },
        file_context: None,
    }
}

fn header_row(header: AgentHeader, epoch: &IngestionEpoch) -> ReadRowChange {
    let agent = header.agent.address.agent();
    ReadRowChange {
        scope: read_scope(&agent, None, "header"),
        address: agent.encode(),
        anchor: Some(TraceAddress::of(&AnyObjectRef::Agent(agent.clone()))),
        order_key: 1u64.to_be_bytes().to_vec(),
        body: Some(serde_json::to_value(header).unwrap()),
        scope_epoch: epoch.clone(),
        required_environments: [agent.environment().as_str().to_owned()].into(),
        dependency_epochs: [epoch.clone()].into(),
    }
}

impl Fixture {
    async fn new(include_parent: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let environment = BTreeMap::from([
            ("HOME".into(), directory.path().display().to_string()),
            ("SYLPH_HOME".into(), directory.path().display().to_string()),
        ]);
        let resolved =
            crate::config::Resolved::from_parts(&environment, &Default::default(), false);
        crate::config::seed(&resolved).unwrap();
        let state = AppState::build(crate::config::Config::from_resolved(&resolved))
            .await
            .unwrap();
        state
            .store
            .upsert_subject("reader", "Reader", "unused-hash", false)
            .await
            .unwrap();
        let parent = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:codex:deleted-parent").unwrap(),
        );
        let child = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:codex:surviving-child").unwrap(),
        );
        let relation_source = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:codex:relation-witness").unwrap(),
        );
        state
            .store
            .upsert_environment(
                parent.environment(),
                "fixture-installation",
                "Fixture",
                "development",
                "unused-token",
                "[]",
            )
            .await
            .unwrap();
        state
            .store
            .issue_grant(
                "reader",
                parent.environment().as_str(),
                "observe",
                "reader",
                None,
            )
            .await
            .unwrap();
        let generation = state.trace.active().await.unwrap();
        let mut epochs = Vec::new();
        for agent in [&parent, &child, &relation_source] {
            let epoch = state
                .trace
                .evidence()
                .authorize_scope(
                    &IngestionScope::Agent(agent.clone()),
                    std::slice::from_ref(agent),
                )
                .await
                .unwrap();
            let proof = state.trace.catalogue().scope_proof(&epoch).await.unwrap();
            state
                .trace
                .catalogue()
                .pin_generation_scope(
                    &generation.manifest().generation,
                    &proof,
                    generation.checkpoint().await.unwrap().snapshot.evidence_cut,
                )
                .await
                .unwrap();
            epochs.push(epoch);
        }
        let parent_header = header(&parent, "Sensitive parent metadata", None);
        let child_header = header(&child, "Surviving child", Some(parent_header.agent.clone()));
        let mut read_rows = vec![header_row(child_header, &epochs[1])];
        if include_parent {
            read_rows.push(header_row(parent_header, &epochs[0]));
            read_rows.push(ReadRowChange {
                scope: read_scope(&parent, None, "relations"),
                address: child.encode(),
                anchor: Some(TraceAddress::of(&AnyObjectRef::Agent(child.clone()))),
                order_key: 1u64.to_be_bytes().to_vec(),
                body: Some(json!({})),
                scope_epoch: epochs[2].clone(),
                required_environments: [parent.environment().as_str().to_owned()].into(),
                dependency_epochs: [epochs[0].clone(), epochs[1].clone()].into(),
            });
        }
        let checkpoint = generation.checkpoint().await.unwrap();
        generation
            .apply(
                checkpoint.fence_token,
                &checkpoint.snapshot,
                &ProjectionBatch {
                    through_cut: checkpoint.snapshot.evidence_cut,
                    continuation: true,
                    read_rows,
                    ..ProjectionBatch::default()
                },
            )
            .await
            .unwrap();
        let app = router()
            .layer(Extension(SubjectId("reader".into())))
            .with_state(state.clone());
        Self {
            _directory: directory,
            state,
            app,
            parent,
            child,
            parent_epoch: epochs.remove(0),
            relation_source,
        }
    }

    async fn bootstrap(&self, agent: &AgentRef, view: Option<&str>) -> Value {
        let mut path = format!("/api/v1/trace/agents/{}", agent.encode());
        if let Some(view) = view {
            path.push_str(&format!("?at_snapshot={view}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn delete_parent(&self) {
        self.state
            .trace
            .evidence()
            .delete_scope(&IngestionScope::Agent(self.parent.clone()))
            .await
            .unwrap();
    }

    fn assert_parent_hidden(&self, child: &Value) {
        assert_eq!(child["header"]["parent"], Value::Null);
        assert_eq!(
            child["header"]["agent"]["address"],
            json!(AgentAddress::of(&self.child))
        );
        assert!(!child.to_string().contains("Sensitive parent metadata"));
        assert!(
            !child
                .to_string()
                .contains(self.parent.anchor_session().native_id.as_str())
        );
    }
}

#[tokio::test]
async fn retained_child_bootstrap_redacts_a_deleted_parent_without_losing_its_own_view() {
    let fixture = Fixture::new(true).await;
    let before = fixture.bootstrap(&fixture.child, None).await;
    assert_eq!(
        before["header"]["parent"]["name"],
        "Sensitive parent metadata"
    );
    let view = before["view"]["token"].as_str().unwrap();
    fixture.delete_parent().await;
    let after = fixture.bootstrap(&fixture.child, Some(view)).await;
    fixture.assert_parent_hidden(&after);
    assert_eq!(after["view"], before["view"]);
}

#[tokio::test]
async fn readoption_cannot_reauthorize_parent_metadata_from_a_retained_child_view() {
    let fixture = Fixture::new(true).await;
    let before = fixture.bootstrap(&fixture.child, None).await;
    let view = before["view"]["token"].as_str().unwrap();
    fixture.delete_parent().await;
    let fresh_epoch = fixture
        .state
        .trace
        .evidence()
        .readopt_scope(
            &IngestionScope::Agent(fixture.parent.clone()),
            std::slice::from_ref(&fixture.parent),
        )
        .await
        .unwrap();
    assert_ne!(fresh_epoch, fixture.parent_epoch);
    let proof = fixture
        .state
        .trace
        .catalogue()
        .scope_proof(&fresh_epoch)
        .await
        .unwrap();
    assert!(
        fixture
            .state
            .trace
            .catalogue()
            .scope_is_readable(&proof)
            .await
            .unwrap()
    );
    fixture.assert_parent_hidden(&fixture.bootstrap(&fixture.child, Some(view)).await);
}

#[tokio::test]
async fn an_embedded_link_without_a_retained_target_header_is_unresolved() {
    let fixture = Fixture::new(false).await;
    fixture.assert_parent_hidden(&fixture.bootstrap(&fixture.child, None).await);
}

#[tokio::test]
async fn deleting_relationship_evidence_redacts_the_link_while_both_endpoints_remain_readable() {
    let fixture = Fixture::new(true).await;
    let before = fixture.bootstrap(&fixture.child, None).await;
    assert_eq!(
        before["header"]["parent"]["name"],
        "Sensitive parent metadata"
    );
    let view = before["view"]["token"].as_str().unwrap();
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(fixture.relation_source.clone()))
        .await
        .unwrap();
    fixture
        .state
        .trace
        .evidence()
        .readopt_scope(
            &IngestionScope::Agent(fixture.relation_source.clone()),
            std::slice::from_ref(&fixture.relation_source),
        )
        .await
        .unwrap();
    let child = fixture.bootstrap(&fixture.child, Some(view)).await;
    fixture.assert_parent_hidden(&child);
    assert_eq!(child["view"], before["view"]);
    let parent = fixture.bootstrap(&fixture.parent, None).await;
    assert_eq!(
        parent["header"]["agent"]["name"],
        "Sensitive parent metadata"
    );
}

#[tokio::test]
async fn embedded_link_authorization_does_not_hydrate_the_target_header_body() {
    use sqlx::Connection;
    let fixture = Fixture::new(true).await;
    let generation = fixture.state.trace.active().await.unwrap();
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(generation.path()),
    )
    .await
    .unwrap();
    assert_eq!(sqlx::query("UPDATE read_rows SET body_json='not JSON' WHERE scope_key=? AND address=? AND valid_to IS NULL")
        .bind(read_scope(&fixture.parent, None, "header")).bind(fixture.parent.encode())
        .execute(&mut connection).await.unwrap().rows_affected(), 1);
    assert_eq!(sqlx::query("UPDATE read_rows SET body_json='not JSON' WHERE scope_key=? AND address=? AND valid_to IS NULL")
        .bind(read_scope(&fixture.parent, None, "relations")).bind(fixture.child.encode())
        .execute(&mut connection).await.unwrap().rows_affected(), 1);
    let child = fixture.bootstrap(&fixture.child, None).await;
    assert_eq!(
        child["header"]["parent"]["name"],
        "Sensitive parent metadata"
    );
}
