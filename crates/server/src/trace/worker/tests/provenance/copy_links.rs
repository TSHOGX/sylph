use super::*;

#[tokio::test]
async fn copy_lineage_is_optional_authorized_metadata_and_never_owns_the_copied_body() {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "copy-reader");
    let original = agent("origin", "original-reader");
    let local = fixture
        .capture(&recipient, "copied-body", b"Independent retained copy")
        .await;
    let remote = fixture
        .capture(&original, "original-record", b"Original record")
        .await;
    let copied = EntryRef::native(recipient.clone(), "entry", vec!["copy".into()]).unwrap();
    let source = EntryRef::native(original.clone(), "entry", vec!["source".into()]).unwrap();
    let message = MessageRef::native(recipient.clone(), "message", vec!["copy".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(recipient.clone(), "revision", vec!["copy".into()]).unwrap();
    let mut values = vec![
        claim(
            &recipient,
            "agent",
            agent_object(&recipient, "Copy reader"),
            &[&local],
        ),
        claim(
            &original,
            "agent",
            agent_object(&original, "Original reader"),
            &[&remote],
        ),
        claim(
            &recipient,
            "entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: copied.clone(),
                containing_agent: recipient.clone(),
                predecessor: None,
                original_owner: None,
            }),
            &[&local],
        ),
        claim(
            &original,
            "entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: source.clone(),
                containing_agent: original.clone(),
                predecessor: None,
                original_owner: None,
            }),
            &[&remote],
        ),
        claim(
            &recipient,
            "message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Unknown,
                protocol_role: Some(ProtocolRole::User),
                purpose: MessagePurpose::Context,
                production: None,
            }),
            &[&local],
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
                        "part",
                        vec!["copy".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Text,
                    payload: local.payload_ref.clone(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
            &[&local],
        ),
    ];
    let mut content = claim(
        &recipient,
        "content",
        agent_object(&recipient, "unused"),
        &[&local],
    );
    content.fact = Fact::HistoryContentObserved {
        observation: HistoryContentObservation::complete(
            copied.clone(),
            local.source_position.clone(),
            "copied-record",
            vec![HistoryContent::Message(revision)],
        ),
    };
    values.push(content);
    let mut linked = claim(
        &recipient,
        "copy-proof",
        agent_object(&recipient, "unused"),
        &[&local, &remote],
    );
    linked.fact = Fact::Relation {
        relation: Relation::ContextCopy {
            original: source.clone(),
            copied: copied.clone(),
            original_position: remote.source_position.clone(),
            copied_position: local.source_position.clone(),
        },
    };
    values.push(linked);
    let body = "Independent retained copy";
    fixture.committed.previews.insert(
        local.payload_ref.clone(),
        Arc::new(sylph_trace::reading::PayloadPreview {
            preview: body.into(),
            preview_limit: fixture.generation.limits().max_preview_bytes,
            total_bytes: Some(body.len() as u64),
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: body.len() as u64,
            }],
            availability: Some(ReadAvailability::Available),
            ..Default::default()
        }),
    );
    fixture.publish(values, &[&local, &remote]).await;
    let before = fixture.bootstrap(&recipient).await;
    assert!(before.to_string().contains("Independent retained copy"));
    let token = before["view"]["token"].as_str().unwrap();
    let access = &before["window"]["items"][0]["content"]["parts"][0]["payload"];

    // Inspection owns a separate lease; deleting its origin cannot expire the
    // independently held main reader, whose content never borrowed that scope.
    let (status, detail) = fixture
        .get(&format!("/api/v1/trace/objects/{}", copied.encode()))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(
        detail["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["target"] == source.encode() && row["label"] == "Copy lineage")
    );
    fixture
        .state
        .trace
        .evidence()
        .delete_scope(&IngestionScope::Agent(original))
        .await
        .unwrap();
    let (status, retained) = fixture
        .get(&format!(
            "/api/v1/trace/agents/{}?at_snapshot={token}",
            recipient.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert!(retained.to_string().contains("Independent retained copy"));
    let response=fixture.get_response(&format!("/api/v1/trace/payloads/{}?via_ref={}&at_snapshot={token}&revision={}&representation=conversation",local.payload_ref,access["viaRef"].as_str().unwrap(),access["bodyRevision"].as_str().unwrap())).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), body.len() + 1)
            .await
            .unwrap()
            .as_ref(),
        body.as_bytes()
    );

    let (status, hidden) = fixture
        .get(&format!(
            "/api/v1/trace/objects/{}?at_snapshot={token}",
            copied.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{hidden}");
    assert!(
        !hidden["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["label"] == "Copy lineage")
    );
}
