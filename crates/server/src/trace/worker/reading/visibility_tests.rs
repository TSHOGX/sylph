//! HTTP authorization over rows produced by the normalized reader projection.
use super::*;
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use base64::Engine;
use serde_json::{json, Value};
use sylph_protocol::{AgentName, EnvironmentId, NativeId, SessionId};
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    state: crate::AppState,
    app: Router,
    cookie: String,
    subject: String,
    parent: AgentRef,
    visible: AgentRef,
    hidden: Vec<AgentRef>,
}

impl Fixture {
    async fn new(hidden_count: Option<usize>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let environment = BTreeMap::from([
            ("HOME".into(), directory.path().display().to_string()),
            ("SYLPH_HOME".into(), directory.path().display().to_string()),
        ]);
        let resolved =
            crate::config::Resolved::from_parts(&environment, &Default::default(), false);
        crate::config::seed(&resolved).unwrap();
        let state = crate::AppState::build(crate::config::Config::from_resolved(&resolved))
            .await
            .unwrap();
        let generation = state.trace.active().await.unwrap();
        let hidden_count = hidden_count.unwrap_or(generation.limits().max_candidates + 1);
        let app = crate::router(state.clone());
        let (administrator_cookie, administrator) = register(&app, "fixture-administrator").await;
        let (cookie, subject) = register(&app, "restricted-reader").await;
        assert_ne!(administrator_cookie, cookie);
        assert!(!state.store.is_admin(&subject).await.unwrap());
        let allowed = EnvironmentId::parse("zzzz").unwrap();
        let denied = EnvironmentId::parse("aaaa").unwrap();
        for environment in [&allowed, &denied] {
            state
                .store
                .upsert_environment(
                    environment,
                    &format!("installation-{environment}"),
                    "Visibility fixture",
                    "development",
                    "unused-token-hash",
                    "[]",
                )
                .await
                .unwrap();
        }
        state
            .store
            .issue_grant(&subject, allowed.as_str(), "observe", &administrator, None)
            .await
            .unwrap();
        let agent = |environment: &EnvironmentId, id: String| {
            AgentRef::Session(SessionId::new(
                environment.clone(),
                AgentName::parse("codex").unwrap(),
                NativeId::new(id),
            ))
        };
        let parent = agent(&allowed, "parent".into());
        let visible = agent(&allowed, "visible-sibling".into());
        let hidden: Vec<_> = (0..hidden_count)
            .map(|index| agent(&denied, format!("private-child-{index:04}")))
            .collect();
        let mut facts = Vec::new();
        let mut epochs = BTreeMap::new();
        for agent in std::iter::once(&parent)
            .chain(hidden.iter())
            .chain(std::iter::once(&visible))
        {
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
            epochs.insert(AnyObjectRef::Agent(agent.clone()), epoch);
            facts.push(Fact::Declare {
                object: TraceObject::Agent(AgentThread {
                    agent_ref: agent.clone(),
                    vendor: AgentName::parse("codex").unwrap(),
                    name: Some(agent.anchor_session().native_id.as_str().into()),
                    model: None,
                }),
            });
        }
        for child in hidden.iter().chain(std::iter::once(&visible)) {
            facts.push(Fact::Relation {
                relation: Relation::Spawn {
                    parent: parent.clone(),
                    child: child.clone(),
                    operation: None,
                },
            });
        }
        let claims: Vec<_> = facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| Claim {
                claim_ref: ClaimRef::native(
                    parent.clone(),
                    "visibility-fixture",
                    vec![index.to_string()],
                )
                .unwrap(),
                fact,
                evidence: vec![ObservationRef {
                    issuer: Issuer::Server("fixture".into()),
                    producer_id: "normalized".into(),
                    producer_epoch: "one".into(),
                    producer_seq: index as u64 + 1,
                }],
                rule_id: "visibility-fixture".into(),
                mapper_build: "test".into(),
                supersedes: vec![],
                source_revision: None,
            })
            .collect();
        let mut reducer = sylph_trace::reducer::Reducer::default();
        reducer
            .apply(
                &claims,
                &claims
                    .iter()
                    .flat_map(|claim| claim.evidence.clone())
                    .collect(),
                claims.len(),
            )
            .unwrap();
        let reduction = reducer.resolve();
        assert!(
            reduction.diagnostics.is_empty(),
            "{:?}",
            reduction.diagnostics
        );
        let projection = sylph_trace::reading::project(&reduction, &BTreeMap::new(), 32);
        assert_eq!(projection.directories[&parent].len(), hidden_count + 1);
        assert_eq!(
            projection.directories[&parent]
                .last()
                .unwrap()
                .agent
                .address
                .agent(),
            visible
        );
        let published_rows: Vec<_> = rows(&projection, &epochs).unwrap().into_values().collect();
        // Publish bounded production row batches. No vendor mapper is being
        // certified here: the fixture starts at the normalized facts boundary.
        for chunk in published_rows.chunks(generation.limits().max_batch_changes) {
            let checkpoint = generation.checkpoint().await.unwrap();
            generation
                .apply(
                    checkpoint.fence_token,
                    &checkpoint.snapshot,
                    &ProjectionBatch {
                        through_cut: checkpoint.snapshot.evidence_cut,
                        continuation: true,
                        read_rows: chunk.to_vec(),
                        ..ProjectionBatch::default()
                    },
                )
                .await
                .unwrap();
        }
        Self {
            _directory: directory,
            state,
            app,
            cookie,
            subject,
            parent,
            visible,
            hidden,
        }
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        response(
            self.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::HOST, "localhost")
                        .header(header::COOKIE, &self.cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await
    }

    async fn publish_parts(&self, count: usize) {
        let message =
            MessageRef::native(self.parent.clone(), "multipart", vec!["message".into()]).unwrap();
        let revision =
            MessageRevisionRef::native(self.parent.clone(), "multipart", vec!["revision".into()])
                .unwrap();
        let entry =
            EntryRef::native(self.parent.clone(), "multipart", vec!["entry".into()]).unwrap();
        let mut previews = BTreeMap::new();
        let parts = (0..count)
            .map(|index| {
                let text = format!("Observed part {index}");
                let payload = PayloadRef::bytes(blake3::hash(text.as_bytes()).to_hex()).unwrap();
                previews.insert(
                    payload.clone(),
                    PayloadPreview {
                        preview: text.clone(),
                        preview_limit: 2048,
                        total_bytes: Some(text.len() as u64),
                        ranges: vec![PayloadRange {
                            start: 0,
                            end_exclusive: text.len() as u64,
                        }],
                        availability: Some(ReadAvailability::Available),
                        ..PayloadPreview::default()
                    },
                );
                ContentPart {
                    part_ref: MessagePartRef::native(
                        self.parent.clone(),
                        "multipart",
                        vec![index.to_string()],
                    )
                    .unwrap(),
                    kind: ContentKind::Text,
                    payload,
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }
            })
            .collect();
        let facts = vec![
            Fact::Declare {
                object: TraceObject::Agent(AgentThread {
                    agent_ref: self.parent.clone(),
                    vendor: AgentName::parse("codex").unwrap(),
                    name: None,
                    model: None,
                }),
            },
            Fact::Declare {
                object: TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Unknown,
                    protocol_role: None,
                    purpose: MessagePurpose::Unclassified,
                    production: None,
                }),
            },
            Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: revision.clone(),
                    message,
                    representation: Representation::Conversation,
                    form: RevisionForm::Complete,
                    parts,
                }),
            },
            Fact::Declare {
                object: TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry.clone(),
                    containing_agent: self.parent.clone(),
                    predecessor: None,
                    original_owner: None,
                }),
            },
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry.clone(),
                    SourcePosition {
                        source: SourceRef {
                            issuer: Issuer::Server("fixture".into()),
                            instance: "multipart".into(),
                        },
                        generation: 0,
                        locator: SourceLocator::Frame { ordinal: 1 },
                    },
                    "multipart-fixture",
                    vec![HistoryContent::Message(revision.clone())],
                ),
            },
        ];
        let claims: Vec<_> = facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| Claim {
                claim_ref: ClaimRef::native(
                    self.parent.clone(),
                    "multipart-fixture",
                    vec![index.to_string()],
                )
                .unwrap(),
                fact,
                evidence: vec![ObservationRef {
                    issuer: Issuer::Server("fixture".into()),
                    producer_id: "multipart".into(),
                    producer_epoch: "one".into(),
                    producer_seq: index as u64 + 1,
                }],
                rule_id: "multipart-fixture".into(),
                mapper_build: "test".into(),
                supersedes: vec![],
                source_revision: None,
            })
            .collect();
        let mut reducer = sylph_trace::reducer::Reducer::default();
        reducer
            .apply(
                &claims,
                &claims
                    .iter()
                    .flat_map(|claim| claim.evidence.clone())
                    .collect(),
                claims.len(),
            )
            .unwrap();
        let projection = sylph_trace::reading::project(&reducer.resolve(), &previews, 32);
        let epoch = self
            .state
            .trace
            .evidence()
            .authorize_scope(
                &IngestionScope::Agent(self.parent.clone()),
                std::slice::from_ref(&self.parent),
            )
            .await
            .unwrap();
        let epochs = BTreeMap::from([(AnyObjectRef::Agent(self.parent.clone()), epoch)]);
        let history = read_scope(&self.parent, None, "history");
        let parts = sylph_trace::reading::parts_key(
            &self.parent,
            None,
            &AnyObjectRef::HistoryEntry(entry),
            &revision,
        );
        // Add these new history/parts collections to the existing graph fixture;
        // their rows and ordering still come from the production projection.
        let rows = rows(&projection, &epochs)
            .unwrap()
            .into_values()
            .filter(|row| row.scope == history || row.scope == parts)
            .collect::<Vec<_>>();
        let generation = self.state.trace.active().await.unwrap();
        let checkpoint = generation.checkpoint().await.unwrap();
        generation
            .apply(
                checkpoint.fence_token,
                &checkpoint.snapshot,
                &ProjectionBatch {
                    through_cut: checkpoint.snapshot.evidence_cut,
                    continuation: true,
                    read_rows: rows,
                    ..ProjectionBatch::default()
                },
            )
            .await
            .unwrap();
    }

    fn directory(&self) -> String {
        format!("/api/v1/trace/agents/{}/relations", self.parent.encode())
    }

    fn assert_private(&self, body: &Value) {
        let body = body.to_string();
        for hidden in &self.hidden {
            assert!(
                !body.contains(&hidden.encode()),
                "hidden identity leaked: {body}"
            );
            assert!(
                !body.contains(hidden.anchor_session().native_id.as_str()),
                "hidden name leaked: {body}"
            );
        }
    }
}

async fn response(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn register(app: &Router, name: &str) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/register")
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"name":name}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (status, body) = self::response(response).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (cookie, body["id"].as_str().unwrap().into())
}

fn assert_visible_count(value: &Value) {
    assert!(
        value["value"].is_null() || value["value"] == 1,
        "count includes an unreadable relation: {value}"
    );
    if value["value"].is_null() {
        assert_eq!(value["complete"], false);
    }
}

#[tokio::test]
async fn denied_direct_relations_do_not_fail_visible_pages_or_leak_identity_and_counts() {
    let fixture = Fixture::new(Some(1)).await;
    let (status, directory) = fixture.get(&fixture.directory()).await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    fixture.assert_private(&directory);
    assert_eq!(directory["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        directory["items"][0]["agent"]["address"],
        fixture.visible.encode()
    );
    assert_visible_count(&directory["total"]);
    let token = directory["view"]["token"].as_str().unwrap();
    let (status, bootstrap) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={token}",
            fixture.parent.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    fixture.assert_private(&bootstrap);
    assert_visible_count(&bootstrap["header"]["descendantCount"]);
    let reference = AnyObjectRef::Agent(fixture.parent.clone()).encode();
    let (status, detail) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}?at_snapshot={token}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    fixture.assert_private(&detail);
    assert_eq!(detail["relations"].as_array().unwrap().len(), 1);
    assert_eq!(detail["relations"][0]["target"], fixture.visible.encode());
    let (status, relations) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{reference}/relations?at_snapshot={token}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{relations}");
    fixture.assert_private(&relations);
    assert_eq!(relations["items"].as_array().unwrap().len(), 1);
    assert_visible_count(&relations["total"]);
}

#[tokio::test]
async fn denied_first_candidates_advance_an_opaque_cursor_and_changed_grants_reject_it() {
    // Read the actual configured budget; the hidden prefix must exceed it to
    // distinguish bounded progress from scanning until a readable child appears.
    let fixture = Fixture::new(None).await;
    let budget = fixture
        .state
        .trace
        .active()
        .await
        .unwrap()
        .limits()
        .max_candidates;
    assert_eq!(fixture.hidden.len(), budget + 1);
    let path = format!("{}?limit=1", fixture.directory());
    let (status, first) = fixture.get(&path).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    fixture.assert_private(&first);
    assert!(first["items"].as_array().unwrap().is_empty());
    assert_eq!(first["hasAfter"], true);
    assert_eq!(first["state"], "work_budget_exhausted");
    let cursor = first["afterCursor"].as_str().unwrap();
    if let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(cursor) {
        assert!(
            serde_json::from_slice::<Value>(&decoded).is_err(),
            "a continuation must not expose hidden address metadata as readable JSON"
        );
    }
    let token = first["view"]["token"].as_str().unwrap();
    let continuation = format!("{path}&at_snapshot={token}&after={cursor}");
    let (status, second) = fixture.get(&continuation).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    fixture.assert_private(&second);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        second["items"][0]["agent"]["address"],
        fixture.visible.encode()
    );
    assert_eq!(second["view"], first["view"]);
    fixture
        .state
        .store
        .issue_grant(
            &fixture.subject,
            fixture.hidden[0].environment().as_str(),
            "observe",
            &fixture.subject,
            None,
        )
        .await
        .unwrap();
    let (status, rejected) = fixture.get(&continuation).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["error"]["code"], "invalid_cursor");
}

#[tokio::test]
async fn deleting_a_relation_target_hides_it_from_a_retained_parent_view() {
    let fixture = Fixture::new(Some(1)).await;
    let child_path = format!("/api/v1/trace/agents/{}", fixture.visible.encode());
    let (status, child) = fixture.get(&child_path).await;
    assert_eq!(status, StatusCode::OK, "{child}");
    let (status, parent) = fixture
        .get(&format!("/api/v1/trace/agents/{}", fixture.parent.encode()))
        .await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    let parent_view = parent["view"]["token"].as_str().unwrap();
    let child_view = child["view"]["token"].as_str().unwrap();
    assert_ne!(parent_view, child_view);
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(fixture.visible.clone()))
        .await
        .unwrap();

    let (status, gone) = fixture
        .get(&format!("{child_path}?at_snapshot={child_view}"))
        .await;
    assert_eq!(status, StatusCode::GONE, "{gone}");
    assert_eq!(gone["error"]["code"], "snapshot_expired");
    for path in [
        format!("{}?at_snapshot={parent_view}", fixture.directory()),
        format!(
            "/api/v1/trace/objects/{}/relations?at_snapshot={parent_view}",
            fixture.parent.encode()
        ),
    ] {
        let (status, page) = fixture.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        fixture.assert_private(&page);
        assert!(page["items"].as_array().unwrap().is_empty());
        assert!(!page.to_string().contains(&fixture.visible.encode()));
        assert!(
            page["total"]["value"].is_null() || page["total"]["value"] == 0,
            "{page}"
        );
    }
}

#[tokio::test]
async fn deleting_a_directory_owner_removes_membership_from_an_unrelated_retained_view() {
    let fixture = Fixture::new(Some(1)).await;
    let (status, child) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}",
            fixture.visible.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{child}");
    let child_view = child["view"]["token"].as_str().unwrap();
    let (status, directory) = fixture.get(&fixture.directory()).await;
    assert_eq!(status, StatusCode::OK, "{directory}");
    let directory_view = directory["view"]["token"].as_str().unwrap();
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(fixture.parent.clone()))
        .await
        .unwrap();

    let (status, page) = fixture
        .get(&format!("{}?at_snapshot={child_view}", fixture.directory()))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page["items"].as_array().unwrap().is_empty(), "{page}");
    assert_eq!(page["total"]["value"], 0);
    let (status, gone) = fixture
        .get(&format!(
            "{}?at_snapshot={directory_view}",
            fixture.directory()
        ))
        .await;
    assert_eq!(status, StatusCode::GONE, "{gone}");
    assert_eq!(gone["error"]["code"], "snapshot_expired");
}

#[tokio::test]
async fn bootstrap_parts_cursor_reads_the_remaining_part_only_with_its_original_lease() {
    let fixture = Fixture::new(Some(1)).await;
    fixture.publish_parts(33).await;
    let bootstrap_path = format!("/api/v1/trace/agents/{}", fixture.parent.encode());
    let (status, bootstrap) = fixture.get(&bootstrap_path).await;
    assert_eq!(status, StatusCode::OK, "{bootstrap}");
    let units = bootstrap["window"]["items"].as_array().unwrap();
    assert_eq!(units.len(), 1);
    let message = &units[0]["content"];
    assert_eq!(message["partsTotal"], 33);
    assert_eq!(message["parts"].as_array().unwrap().len(), 32);
    assert_eq!(message["partsComplete"], false);
    let cursor = message["partsCursor"]
        .as_str()
        .expect("initial part window needs continuation");
    let view = bootstrap["view"]["token"].as_str().unwrap();
    let revision = message["revisionRef"].as_str().unwrap();
    let anchor = units[0]["anchor"].as_str().unwrap();
    let path = |view: &str, cursor: &str| {
        format!("/api/v1/trace/objects/{revision}/parts?via_ref={anchor}&at_snapshot={view}&after={cursor}")
    };
    let (status, remaining) = fixture.get(&path(view, cursor)).await;
    assert_eq!(status, StatusCode::OK, "{remaining}");
    assert_eq!(remaining["view"], bootstrap["view"]);
    assert_eq!(remaining["items"].as_array().unwrap().len(), 1);
    assert_eq!(remaining["items"][0]["preview"], "Observed part 32");
    assert_eq!(remaining["hasAfter"], false);
    assert_eq!(remaining["items"][0]["payload"]["viaRef"], anchor);

    let (status, another) = fixture.get(&bootstrap_path).await;
    assert_eq!(status, StatusCode::OK, "{another}");
    let other_view = another["view"]["token"].as_str().unwrap();
    assert_ne!(other_view, view);
    let (status, replay) = fixture.get(&path(other_view, cursor)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{replay}");
    assert_eq!(replay["error"]["code"], "invalid_cursor");
    let mut changed = cursor.as_bytes().to_vec();
    let middle = changed.len() / 2;
    changed[middle] = if changed[middle] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(changed).unwrap();
    let (status, rejected) = fixture.get(&path(view, &tampered)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["error"]["code"], "invalid_cursor");
}
