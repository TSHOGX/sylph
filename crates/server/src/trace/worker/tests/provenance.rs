//! Authorization acceptance at the normalized-fact boundary, backed by admitted evidence.
use super::*;
use crate::trace::reading_store::{ReadRow, ReadRowChange, read_scope};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct ProvenanceFixture {
    _directory: tempfile::TempDir,
    state: crate::AppState,
    app: Router,
    cookie: String,
    subject: String,
    generation: Arc<GenerationStore>,
    committed: WorkerState,
    sequence: u64,
    _projection_guard: tokio::sync::OwnedMutexGuard<()>,
}

fn agent(environment: &str, native: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("{environment}:codex:{native}")).unwrap())
}

fn agent_object(agent: &AgentRef, name: &str) -> TraceObject {
    TraceObject::Agent(AgentThread {
        agent_ref: agent.clone(),
        vendor: AgentName::parse("codex").unwrap(),
        name: Some(name.into()),
        model: None,
    })
}

fn claim(owner: &AgentRef, key: &str, object: TraceObject, evidence: &[&Capture]) -> Claim {
    Claim {
        claim_ref: ClaimRef::native(owner.clone(), "provenance-test", vec![key.into()]).unwrap(),
        fact: Fact::Declare { object },
        evidence: evidence
            .iter()
            .map(|capture| capture.observation_ref.clone())
            .collect(),
        rule_id: "normalized-provenance-test".into(),
        mapper_build: "test".into(),
        supersedes: vec![],
        source_revision: None,
    }
}

impl ProvenanceFixture {
    async fn new() -> Self {
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
        // These tests publish normalized facts themselves. Hold the production
        // projector's gate so its periodic pass cannot race fixture publication
        // after a slow payload setup; same-cut safety must remain enforced.
        let projection_guard = state.trace.hold_fixture_projection().await;
        let app = crate::router(state.clone());
        let (_, administrator) = register(&app, "provenance-administrator").await;
        let (cookie, subject) = register(&app, "provenance-reader").await;
        for environment in ["recipient", "origin"] {
            let id = EnvironmentId::parse(environment).unwrap();
            state
                .store
                .upsert_environment(
                    &id,
                    &format!("installation-{environment}"),
                    "Provenance fixture",
                    "development",
                    "unused-token",
                    "[]",
                )
                .await
                .unwrap();
            state
                .store
                .issue_grant(&subject, environment, "observe", &administrator, None)
                .await
                .unwrap();
        }
        let generation = state.trace.active().await.unwrap();
        let committed = WorkerState {
            snapshot: Some(generation.checkpoint().await.unwrap().snapshot),
            ..Default::default()
        };
        Self {
            _directory: directory,
            state,
            app,
            cookie,
            subject,
            generation,
            committed,
            sequence: 0,
            _projection_guard: projection_guard,
        }
    }

    async fn capture(&mut self, owner: &AgentRef, label: &str, bytes: &[u8]) -> Capture {
        self.sequence += 1;
        let epoch = self
            .state
            .trace
            .evidence()
            .authorize_scope(
                &IngestionScope::Agent(owner.clone()),
                std::slice::from_ref(owner),
            )
            .await
            .unwrap();
        let issuer = Issuer::Environment(owner.environment().clone());
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: label.into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: label.into(),
                },
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 0 },
            },
            ingestion_authorization_epoch: epoch,
            declared_subject_refs: vec![owner.clone()],
            captured_at: TimeEvidence {
                domain: "fixture".into(),
                value: self.sequence as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::SourceState,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: self
                .state
                .trace
                .evidence()
                .stage_payload(bytes)
                .await
                .unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        self.state
            .trace
            .evidence()
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        capture
    }

    async fn publish(&mut self, claims: Vec<Claim>, captures: &[&Capture]) -> Vec<ReadRowChange> {
        let cut = self.state.trace.evidence().evidence_cut().await.unwrap();
        let mut unit = ProjectionUnit::new(&self.committed, cut);
        unit.batch.continuation = cut == self.committed.cut();
        unit.batch.claims = claims.clone();
        for capture in captures {
            unit.next
                .available_evidence
                .insert(capture.observation_ref.clone());
            unit.scopes
                .push((capture.ingestion_authorization_epoch.clone(), cut));
        }
        unit.next
            .reducer
            .apply(&claims, &unit.next.available_evidence, 1000)
            .unwrap();
        let worker = self.state.trace.worker();
        worker.reduce_unit(&mut unit).await.unwrap();
        worker
            .materialize(&self.generation, &self.committed, &mut unit)
            .await
            .unwrap();
        worker
            .commit_effects(&self.generation, &unit)
            .await
            .unwrap();
        let emitted = unit.batch.read_rows.clone();
        let checkpoint = self.generation.checkpoint().await.unwrap();
        let committed = self
            .generation
            .apply(checkpoint.fence_token, &checkpoint.snapshot, &unit.batch)
            .await
            .unwrap();
        unit.next.snapshot = Some(committed.snapshot);
        self.committed = unit.next;
        emitted
    }

    async fn row(&self, agent: &AgentRef, collection: &str, address: &str) -> ReadRow {
        self.generation
            .read_row(
                self.committed.committed_snapshot(),
                &read_scope(agent, None, collection),
                address,
            )
            .await
            .unwrap()
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        response(self.get_response(path).await).await
    }

    async fn get_response(&self, path: &str) -> axum::response::Response {
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
            .unwrap()
    }

    async fn bootstrap(&self, agent: &AgentRef) -> Value {
        let (status, body) = self
            .get(&format!("/api/v1/trace/agents/{}", agent.encode()))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
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

#[tokio::test]
async fn every_observation_supporting_one_declaration_contributes_its_epoch() {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "reader");
    let origin = agent("origin", "supporter");
    // The earliest-cited observation must not also be the least one, or selecting
    // the first-arriving epoch would satisfy this assertion by coincidence.
    // ObservationRef orders by issuer first, so the recipient's own environment
    // sorts after the origin's while arriving ahead of it.
    let arrived_first = fixture
        .capture(&recipient, "a-recipient", b"recipient declaration evidence")
        .await;
    let least = fixture
        .capture(&origin, "z-origin", b"origin declaration evidence")
        .await;
    assert_ne!(
        arrived_first.ingestion_authorization_epoch,
        least.ingestion_authorization_epoch
    );
    let object = agent_object(&recipient, "Merged name");
    let declaration = claim(
        &recipient,
        "same-declaration",
        object,
        &[&arrived_first, &least],
    );
    let primary = declaration.evidence.iter().min().unwrap().clone();
    assert_eq!(
        primary, least.observation_ref,
        "the least observation must differ from the first-cited one"
    );
    fixture
        .publish(vec![declaration.clone()], &[&arrived_first, &least])
        .await;
    let reference = AnyObjectRef::Agent(recipient.clone());
    let provenance = &fixture.committed.object_provenance[&reference];
    assert_eq!(provenance.primary_claim, declaration.claim_ref);
    assert_eq!(provenance.primary_observation, primary);
    assert_eq!(
        provenance.dependency_epochs,
        imbl::ordset![
            arrived_first.ingestion_authorization_epoch.clone(),
            least.ingestion_authorization_epoch.clone()
        ]
    );
    // The primary epoch follows the least observation, not the earliest arrival.
    assert_eq!(
        provenance.primary_epoch,
        least.ingestion_authorization_epoch
    );
    let row = fixture.row(&recipient, "header", &recipient.encode()).await;
    assert_eq!(row.epoch, least.ingestion_authorization_epoch);
    assert_eq!(
        row.dependency_epochs,
        BTreeSet::from([
            arrived_first.ingestion_authorization_epoch,
            least.ingestion_authorization_epoch
        ])
    );
    fixture.bootstrap(&recipient).await;
}

#[tokio::test]
async fn later_agreeing_declarations_replace_authorization_metadata_even_when_the_body_is_equal() {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "reader");
    let origin = agent("origin", "supporter");
    let first = fixture
        .capture(&recipient, "first", b"first supporting occurrence")
        .await;
    let object = agent_object(&recipient, "Same name");
    fixture
        .publish(
            vec![claim(&recipient, "z-first", object.clone(), &[&first])],
            &[&first],
        )
        .await;
    let previous = fixture.row(&recipient, "header", &recipient.encode()).await;
    let old_view = fixture.bootstrap(&recipient).await;
    let later = fixture
        .capture(&origin, "later", b"later agreeing occurrence")
        .await;
    let agreeing = claim(&recipient, "a-later", object, &[&later]);
    let emitted = fixture.publish(vec![agreeing.clone()], &[&later]).await;
    let current = fixture.row(&recipient, "header", &recipient.encode()).await;
    assert_eq!(current.body, previous.body);
    assert_ne!(current.dependency_epochs, previous.dependency_epochs);
    assert_eq!(
        current.dependency_epochs,
        BTreeSet::from([
            first.ingestion_authorization_epoch.clone(),
            later.ingestion_authorization_epoch.clone()
        ])
    );
    assert!(
        emitted
            .iter()
            .any(|row| row.scope == read_scope(&recipient, None, "header")
                && row.address == recipient.encode())
    );
    assert_eq!(
        fixture.committed.object_provenance[&AnyObjectRef::Agent(recipient.clone())].primary_claim,
        agreeing.claim_ref
    );
    let current_view = fixture.bootstrap(&recipient).await;
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(origin.clone()))
        .await
        .unwrap();
    let fresh = fixture
        .state
        .trace
        .evidence()
        .readopt_scope(
            &IngestionScope::Agent(origin.clone()),
            std::slice::from_ref(&origin),
        )
        .await
        .unwrap();
    assert_ne!(fresh, later.ingestion_authorization_epoch);
    let (status, hidden) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={}",
            recipient.encode(),
            current_view["view"]["token"].as_str().unwrap()
        ))
        .await;
    assert_eq!(status, StatusCode::GONE, "{hidden}");
    assert!(!hidden.to_string().contains("Same name"));
    let (status, retained) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={}",
            recipient.encode(),
            old_view["view"]["token"].as_str().unwrap()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert_eq!(retained["header"]["agent"]["name"], "Same name");
}

#[tokio::test]
async fn deleting_and_readopting_an_origin_hides_only_its_link_from_recipient_owned_copy() {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "reader");
    let origin = agent("origin", "private-origin");
    let origin_capture = fixture
        .capture(&origin, "origin", b"private origin metadata")
        .await;
    let body = "Recipient-retained copied bytes";
    let copied = fixture
        .capture(&recipient, "recipient-copy", body.as_bytes())
        .await;
    let message = MessageRef::native(recipient.clone(), "copy", vec!["message".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(recipient.clone(), "copy", vec!["revision".into()]).unwrap();
    let entry = EntryRef::native(recipient.clone(), "copy", vec!["entry".into()]).unwrap();
    let mut claims = vec![
        claim(
            &recipient,
            "recipient",
            agent_object(&recipient, "Recipient"),
            &[&copied],
        ),
        claim(
            &origin,
            "origin",
            agent_object(&origin, "Private origin name"),
            &[&origin_capture],
        ),
        claim(
            &recipient,
            "message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Agent {
                    agent: OwnerRef::Agent {
                        agent: origin.clone(),
                    },
                },
                protocol_role: Some(ProtocolRole::Assistant),
                purpose: MessagePurpose::Answer,
                production: None,
            }),
            &[&copied],
        ),
        claim(
            &recipient,
            "revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(
                        recipient.clone(),
                        "copy",
                        vec!["part".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Text,
                    payload: copied.payload_ref.clone(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
            &[&copied],
        ),
        claim(
            &recipient,
            "entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: recipient.clone(),
                predecessor: None,
                original_owner: Some(OwnerRef::Agent {
                    agent: origin.clone(),
                }),
            }),
            &[&copied],
        ),
    ];
    let mut contents = claim(
        &recipient,
        "contents",
        agent_object(&recipient, "Recipient"),
        &[&copied],
    );
    contents.fact = Fact::HistoryContentObserved {
        observation: HistoryContentObservation::complete(
            entry,
            copied.source_position.clone(),
            "recipient-copy",
            vec![HistoryContent::Message(revision)],
        ),
    };
    claims.push(contents);
    fixture.committed.previews.insert(
        copied.payload_ref.clone(),
        Arc::new(sylph_trace::reading::PayloadPreview {
            preview: body.into(),
            preview_limit: 2048,
            total_bytes: Some(body.len() as u64),
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: body.len() as u64,
            }],
            availability: Some(ReadAvailability::Available),
            ..Default::default()
        }),
    );
    fixture.publish(claims, &[&origin_capture, &copied]).await;
    let before = fixture.bootstrap(&recipient).await;
    assert!(before.to_string().contains("Private origin name"));
    let view = before["view"]["token"].as_str().unwrap();
    let access = &before["window"]["items"][0]["content"]["parts"][0]["payload"];
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(origin.clone()))
        .await
        .unwrap();
    fixture
        .state
        .trace
        .evidence()
        .readopt_scope(
            &IngestionScope::Agent(origin.clone()),
            std::slice::from_ref(&origin),
        )
        .await
        .unwrap();
    let (status, after) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={view}",
            recipient.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(after.to_string().contains(body));
    assert!(!after.to_string().contains("Private origin name"));
    assert!(!after.to_string().contains(&origin.encode()));
    let path = format!(
        "/api/v1/trace/payloads/{}?via_ref={}&at_snapshot={view}&revision={}&representation=conversation",
        copied.payload_ref,
        access["viaRef"].as_str().unwrap(),
        access["bodyRevision"].as_str().unwrap(),
    );
    let response = fixture.get_response(&path).await;
    let status = response.status();
    let payload = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&payload)
    );
    assert_eq!(payload.as_ref(), body.as_bytes());
}

#[tokio::test]
async fn deleting_separately_retained_content_blocks_reader_payload_and_search_at_the_old_view() {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "reader");
    let contributor = agent("origin", "content-source");
    let entry_capture = fixture
        .capture(&recipient, "entry", b"entry evidence")
        .await;
    let body = "independentcontributortext";
    let content_capture = fixture
        .capture(&contributor, "content", body.as_bytes())
        .await;
    let message = MessageRef::native(recipient.clone(), "content", vec!["message".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(recipient.clone(), "content", vec!["revision".into()]).unwrap();
    let entry = EntryRef::native(recipient.clone(), "content", vec!["entry".into()]).unwrap();
    let mut claims = vec![
        claim(
            &recipient,
            "recipient",
            agent_object(&recipient, "Recipient"),
            &[&entry_capture],
        ),
        claim(
            &recipient,
            "message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Human { subject: None },
                protocol_role: Some(ProtocolRole::User),
                purpose: MessagePurpose::TaskInput,
                production: None,
            }),
            &[&entry_capture],
        ),
        claim(
            &recipient,
            "revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(
                        recipient.clone(),
                        "content",
                        vec!["part".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Text,
                    payload: content_capture.payload_ref.clone(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
            &[&content_capture],
        ),
        claim(
            &recipient,
            "entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: recipient.clone(),
                predecessor: None,
                original_owner: None,
            }),
            &[&entry_capture],
        ),
    ];
    let mut binding = claim(
        &recipient,
        "binding",
        agent_object(&recipient, "Recipient"),
        &[&entry_capture],
    );
    binding.fact = Fact::HistoryContentObserved {
        observation: HistoryContentObservation::complete(
            entry,
            entry_capture.source_position.clone(),
            "entry-content",
            vec![HistoryContent::Message(revision)],
        ),
    };
    claims.push(binding);
    fixture.committed.previews.insert(
        content_capture.payload_ref.clone(),
        Arc::new(sylph_trace::reading::PayloadPreview {
            preview: body.into(),
            preview_limit: 2048,
            total_bytes: Some(body.len() as u64),
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: body.len() as u64,
            }],
            availability: Some(ReadAvailability::Available),
            ..Default::default()
        }),
    );
    fixture
        .publish(claims, &[&entry_capture, &content_capture])
        .await;
    let bootstrap = fixture.bootstrap(&recipient).await;
    assert!(bootstrap.to_string().contains(body));
    let view = bootstrap["view"]["token"].as_str().unwrap();
    let access = &bootstrap["window"]["items"][0]["content"]["parts"][0]["payload"];
    let payload_path = format!(
        "/api/v1/trace/payloads/{}?via_ref={}&at_snapshot={view}&revision={}&representation=conversation",
        content_capture.payload_ref,
        access["viaRef"].as_str().unwrap(),
        access["bodyRevision"].as_str().unwrap()
    );
    let before = fixture.get_response(&payload_path).await;
    assert_eq!(before.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(before.into_body(), 1024)
            .await
            .unwrap()
            .as_ref(),
        body.as_bytes()
    );
    fixture.state.trace.index_pending().await.unwrap();
    let (status, search) = fixture.get(&format!("/api/v1/trace/search?q={body}")).await;
    assert_eq!(status, StatusCode::OK, "{search}");
    assert!(search.to_string().contains(body), "{search}");
    // A second lease has never read the contributor. Its first payload request
    // must check the body's support rather than rely on a prior lease binding.
    let untouched = fixture
        .state
        .trace
        .lease_guard(&fixture.subject, None)
        .await
        .unwrap();
    let untouched_path = payload_path.replace(view, &untouched.token);
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(contributor))
        .await
        .unwrap();
    let first_access = fixture.get_response(&untouched_path).await;
    assert_ne!(
        first_access.status(),
        StatusCode::OK,
        "first access must authorize separately retained bytes"
    );
    let after = fixture.get_response(&payload_path).await;
    assert_ne!(after.status(), StatusCode::OK);
    let (status, reader) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={view}",
            recipient.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::GONE, "{reader}");
    let (_, search) = fixture.get(&format!("/api/v1/trace/search?q={body}")).await;
    assert!(!search.to_string().contains(body), "{search}");
}

mod coverage;

mod resources;

mod transfers;

mod inspection;

mod directory;

mod lineage;

mod copy_links;

mod unknown_owners;

mod result_representations;

mod payload_formats;
mod structure;
