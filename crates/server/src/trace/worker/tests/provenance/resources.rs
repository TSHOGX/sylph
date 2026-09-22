use super::*;

fn state_claim(
    owner: &AgentRef,
    resource: &ResourceRef,
    capture: &Capture,
    ordinal: u64,
    state: ResourceState,
) -> Claim {
    let mut position = capture.source_position.clone();
    position.locator = SourceLocator::Frame { ordinal };
    Claim {
        claim_ref: ClaimRef::occurrence(owner.clone(), position, vec!["resource-state".into()])
            .unwrap(),
        fact: Fact::ResourceState {
            resource: resource.clone(),
            state,
        },
        evidence: vec![capture.observation_ref.clone()],
        rule_id: "reader-resource-fixture".into(),
        mapper_build: "fixture".into(),
        supersedes: vec![],
        source_revision: None,
    }
}
fn payload_url(payload: &Value, token: &str) -> String {
    format!(
        "/api/v1/trace/payloads/{}?at_snapshot={token}&via_ref={}&revision={}&representation={}&start=0&end={}",
        payload["payloadRef"].as_str().unwrap(),
        payload["viaRef"].as_str().unwrap(),
        payload["bodyRevision"].as_str().unwrap(),
        payload["representation"].as_str().unwrap(),
        payload["totalBytes"].as_u64().unwrap(),
    )
}
#[tokio::test]
async fn resource_state_pages_preserve_source_order_payloads_and_old_snapshot_membership() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "resources");
    let capture = fixture
        .capture(
            &owner,
            "resource-source",
            br#"{"objective":"retained goal detail"}"#,
        )
        .await;
    let resource = ResourceRef::native(owner.clone(), "goal", vec!["one".into()]).unwrap();
    let operation = OperationRef::native(owner.clone(), "creator", vec!["one".into()]).unwrap();
    let activation = ActivationRef::native(owner.clone(), "turn", vec!["one".into()]).unwrap();
    let mut values = vec![
        claim(
            &owner,
            "agent",
            agent_object(&owner, "Goal owner"),
            &[&capture],
        ),
        claim(
            &owner,
            "activation",
            TraceObject::Activation(Activation {
                activation_ref: activation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            }),
            &[&capture],
        ),
        claim(
            &owner,
            "operation",
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation: Some(activation.clone()),
                requester: None,
                category: OperationCategory::Tool {
                    namespace: Some("fixture".into()),
                    name: "create_goal".into(),
                },
                input_revision: None,
                argument_state: ArgumentState::Sealed,
            }),
            &[&capture],
        ),
        claim(
            &owner,
            "resource",
            TraceObject::Resource(Resource {
                resource_ref: resource.clone(),
                creator: Some(operation.clone()),
                host: Some(OwnerRef::Agent {
                    agent: owner.clone(),
                }),
                kind: ResourceKind::Goal,
            }),
            &[&capture],
        ),
    ];
    for ordinal in (0..25).rev() {
        values.push(state_claim(
            &owner,
            &resource,
            &capture,
            ordinal,
            ResourceState::GoalUpdated {
                status: format!("observed-{ordinal:02}"),
                detail: Some(capture.payload_ref.clone()),
            },
        ));
    }
    fixture.publish(values, &[&capture]).await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let reference = TraceAddress::of(&AnyObjectRef::Resource(resource.clone()));
    let stored = fixture
        .row(
            &owner,
            "objects",
            &AnyObjectRef::Resource(resource.clone()).encode(),
        )
        .await;
    assert!(
        stored.body["content"]["observations"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the stored detail summary must not hydrate the whole state history"
    );
    let path = format!("/api/v1/trace/objects/{}", reference.as_str());
    let (status, detail) = fixture.get(&format!("{path}?at_snapshot={token}")).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["execution"]["state"]["kind"], "unavailable",
        "goal states are not execution boundaries"
    );
    assert_eq!(
        detail["content"]["observations"].as_array().unwrap().len(),
        8
    );
    assert!(
        detail["content"].get("state").is_none(),
        "availability must not masquerade as resource state"
    );
    let targets: BTreeSet<_> = detail["relations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["target"].as_str().unwrap().to_owned())
        .collect();
    assert!(targets.contains(TraceAddress::of(&AnyObjectRef::Operation(operation)).as_str()));
    assert!(targets.contains(TraceAddress::of(&AnyObjectRef::Activation(activation)).as_str()));
    let first = &detail["content"]["observations"][0];
    assert_eq!(first["state"]["status"], "observed-00");
    assert_eq!(first["source"]["locator"]["ordinal"], 0);
    let response = fixture
        .get_response(&payload_url(&first["payloads"][0], token))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap()
            .as_ref(),
        br#"{"objective":"retained goal detail"}"#
    );
    let later = fixture.capture(&owner, "resource-later", b"later").await;
    fixture
        .publish(
            vec![state_claim(
                &owner,
                &resource,
                &capture,
                25,
                ResourceState::GoalUpdated {
                    status: "later".into(),
                    detail: None,
                },
            )],
            &[&later, &capture],
        )
        .await;
    let mut seen: Vec<_> = detail["content"]["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["state"]["status"].as_str().unwrap().to_owned())
        .collect();
    let mut cursor = detail["content"]["observationsCursor"]
        .as_str()
        .map(str::to_owned);
    while let Some(next) = cursor {
        let (status, page) = fixture
            .get(&format!(
                "{path}/relations?kind=observations&at_snapshot={token}&after={next}&limit=7"
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["view"], detail["view"]);
        assert!(page["items"].as_array().unwrap().len() <= 7);
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["state"]["status"].as_str().unwrap().to_owned()),
        );
        cursor = page["afterCursor"]
            .as_str()
            .map(str::to_owned)
            .filter(|_| page["hasAfter"] == true);
    }
    assert_eq!(
        seen,
        (0..25)
            .map(|ordinal| format!("observed-{ordinal:02}"))
            .collect::<Vec<_>>()
    );
    let (status, mismatch) = fixture
        .get(&format!(
            "{path}/relations?kind=payloads&at_snapshot={token}&after={}",
            detail["content"]["observationsCursor"].as_str().unwrap()
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{mismatch}");
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(owner))
        .await
        .unwrap();
    assert_ne!(
        fixture
            .get_response(&payload_url(&first["payloads"][0], token))
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn revision_details_page_exact_immutable_parts_instead_of_the_latest_message_value() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "revisions");
    let bytes = fixture
        .capture(&owner, "old-body", b"retained wrapper")
        .await;
    let message = MessageRef::native(owner.clone(), "message", vec!["one".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(owner.clone(), "revision", vec!["old".into()]).unwrap();
    let parts = (0..21)
        .map(|index| ContentPart {
            part_ref: MessagePartRef::native(owner.clone(), "part", vec![index.to_string()])
                .unwrap(),
            kind: ContentKind::Text,
            payload: bytes.payload_ref.clone(),
            media_type: Some("text/plain".into()),
            encoding: PayloadEncoding::Utf8,
        })
        .collect();
    fixture
        .publish(
            vec![
                claim(
                    &owner,
                    "agent",
                    agent_object(&owner, "Revision owner"),
                    &[&bytes],
                ),
                claim(
                    &owner,
                    "message",
                    TraceObject::Message(Message {
                        message_ref: message.clone(),
                        author: Author::Unknown,
                        protocol_role: None,
                        purpose: MessagePurpose::Unclassified,
                        production: None,
                    }),
                    &[&bytes],
                ),
                claim(
                    &owner,
                    "revision",
                    TraceObject::MessageRevision(MessageRevision {
                        revision_ref: revision.clone(),
                        message: message.clone(),
                        representation: Representation::Conversation,
                        form: RevisionForm::Complete,
                        parts,
                    }),
                    &[&bytes],
                ),
            ],
            &[&bytes],
        )
        .await;
    let boot = fixture.bootstrap(&owner).await;
    let token = boot["view"]["token"].as_str().unwrap();
    let stored = fixture
        .row(
            &owner,
            "objects",
            &AnyObjectRef::MessageRevision(revision.clone()).encode(),
        )
        .await;
    assert!(
        stored.body["content"]["payloads"]
            .as_array()
            .unwrap()
            .is_empty(),
        "immutable revision parts belong in a paged collection"
    );
    let reference = TraceAddress::of(&AnyObjectRef::MessageRevision(revision));
    let path = format!("/api/v1/trace/objects/{}", reference.as_str());
    let (status, detail) = fixture.get(&format!("{path}?at_snapshot={token}")).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["content"]["kind"], "revision");
    assert_eq!(detail["content"]["representation"], "conversation");
    assert_eq!(detail["content"]["payloads"].as_array().unwrap().len(), 8);
    let new = fixture.capture(&owner, "new-body", b"new body").await;
    fixture
        .publish(
            vec![claim(
                &owner,
                "new-revision",
                TraceObject::MessageRevision(MessageRevision {
                    revision_ref: MessageRevisionRef::native(
                        owner.clone(),
                        "revision",
                        vec!["new".into()],
                    )
                    .unwrap(),
                    message,
                    representation: Representation::Conversation,
                    form: RevisionForm::Complete,
                    parts: vec![ContentPart {
                        part_ref: MessagePartRef::native(owner.clone(), "part", vec!["new".into()])
                            .unwrap(),
                        kind: ContentKind::Text,
                        payload: new.payload_ref.clone(),
                        media_type: Some("text/plain".into()),
                        encoding: PayloadEncoding::Utf8,
                    }],
                }),
                &[&new],
            )],
            &[&new],
        )
        .await;
    let mut count = 8;
    let mut cursor = detail["content"]["payloadsCursor"]
        .as_str()
        .map(str::to_owned);
    while let Some(next) = cursor {
        let (status, page) = fixture
            .get(&format!(
                "{path}/relations?kind=payloads&at_snapshot={token}&after={next}&limit=5"
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(page["items"].as_array().unwrap().len() <= 5);
        for payload in page["items"].as_array().unwrap() {
            let response = fixture.get_response(&payload_url(payload, token)).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap()
                    .as_ref(),
                b"retained wrapper"
            );
        }
        count += page["items"].as_array().unwrap().len();
        cursor = page["afterCursor"]
            .as_str()
            .map(str::to_owned)
            .filter(|_| page["hasAfter"] == true);
    }
    assert_eq!(count, 21);
}
