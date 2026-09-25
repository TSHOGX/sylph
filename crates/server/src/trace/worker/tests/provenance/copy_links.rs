use super::*;

#[tokio::test]
async fn copy_lineage_is_optional_authorized_metadata_and_never_owns_the_copied_body() {
    copy_proof_deletion(false, false, false, false).await;
}

#[tokio::test]
async fn nested_copy_lineage_requires_every_version_proof_and_accepts_independent_alternatives() {
    copy_proof_deletion(true, false, false, false).await;
    copy_proof_deletion(true, true, false, false).await;
}

#[tokio::test]
async fn revoking_only_the_copy_witness_grant_keeps_both_endpoint_headers_and_copied_bytes() {
    copy_proof_deletion(true, false, true, false).await;
}

#[tokio::test]
async fn copy_proof_visibility_is_rechecked_after_page_assembly_without_expiring_local_content() {
    copy_proof_deletion(true, false, true, true).await;
    copy_proof_deletion(true, false, false, true).await;
}

async fn copy_proof_deletion(nested: bool, alternate: bool, revoke: bool, during_read: bool) {
    let mut fixture = ProvenanceFixture::new().await;
    let recipient = agent("recipient", "copy-reader");
    let original = agent(
        if revoke { "recipient" } else { "origin" },
        "original-reader",
    );
    let local = fixture
        .capture(&recipient, "copied-body", b"Independent retained copy")
        .await;
    let remote = fixture
        .capture(&original, "original-record", b"Original record")
        .await;
    let witness_owner = agent("origin", "copy-witness");
    let witness = fixture
        .capture(&witness_owner, "copy-witness", b"Independent copy proof")
        .await;
    let copied = EntryRef::native(recipient.clone(), "entry", vec!["copy".into()]).unwrap();
    let alternate_owner = agent("origin", "independent-copy-witness");
    let alternate_witness = fixture
        .capture(
            &alternate_owner,
            "alternate-copy-witness",
            b"Other retained proof",
        )
        .await;
    let middle_owner = agent(if revoke { "recipient" } else { "origin" }, "middle-copy");
    let middle_capture = fixture
        .capture(&middle_owner, "middle-copy", b"Retained intermediate copy")
        .await;
    let middle_entry =
        EntryRef::native(middle_owner.clone(), "entry", vec!["middle".into()]).unwrap();
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
    let mut source_content = claim(
        &original,
        "source-content",
        agent_object(&original, "unused"),
        &[&remote],
    );
    source_content.fact = Fact::HistoryContentObserved {
        observation: HistoryContentObservation::complete(
            source.clone(),
            remote.source_position.clone(),
            "source-record",
            vec![HistoryContent::Opaque(remote.payload_ref.clone())],
        ),
    };
    values.push(source_content);
    let mut linked = claim(
        &recipient,
        "copy-proof",
        agent_object(&recipient, "unused"),
        &[&local, &remote, &witness],
    );
    linked.fact = Fact::Relation {
        relation: Relation::ContextCopy {
            original: source.clone(),
            copied: if nested {
                middle_entry.clone()
            } else {
                copied.clone()
            },
            original_position: remote.source_position.clone(),
            copied_position: if nested {
                middle_capture.source_position.clone()
            } else {
                local.source_position.clone()
            },
        },
    };
    if alternate {
        let mut other = linked.clone();
        other.claim_ref = ClaimRef::native(
            recipient.clone(),
            "provenance-test",
            vec!["alternate-copy-proof".into()],
        )
        .unwrap();
        other
            .evidence
            .retain(|observation| observation != &witness.observation_ref);
        other
            .evidence
            .push(alternate_witness.observation_ref.clone());
        values.push(other);
    }
    values.push(linked);
    if nested {
        values.push(claim(
            &middle_owner,
            "agent",
            agent_object(&middle_owner, "Middle copy"),
            &[&middle_capture],
        ));
        values.push(claim(
            &middle_owner,
            "entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: middle_entry.clone(),
                containing_agent: middle_owner.clone(),
                predecessor: None,
                original_owner: Some(OwnerRef::Unknown),
            }),
            &[&middle_capture],
        ));
        let mut content = claim(
            &middle_owner,
            "content",
            agent_object(&middle_owner, "unused"),
            &[&middle_capture],
        );
        content.fact = Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                middle_entry.clone(),
                middle_capture.source_position.clone(),
                "middle-content",
                vec![HistoryContent::Opaque(middle_capture.payload_ref.clone())],
            ),
        };
        values.push(content);
        let mut outer = claim(
            &recipient,
            "outer-copy-proof",
            agent_object(&recipient, "unused"),
            &[&local, &middle_capture],
        );
        outer.fact = Fact::Relation {
            relation: Relation::ContextCopy {
                original: middle_entry.clone(),
                copied: copied.clone(),
                original_position: middle_capture.source_position.clone(),
                copied_position: local.source_position.clone(),
            },
        };
        values.push(outer);
    }
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
    fixture
        .publish(
            values,
            &[
                &local,
                &remote,
                &witness,
                &alternate_witness,
                &middle_capture,
            ],
        )
        .await;
    let before = fixture.bootstrap(&recipient).await;
    assert!(before.to_string().contains("Independent retained copy"));
    assert_eq!(
        before["window"]["items"][0]["entryContext"]["kind"],
        "inherited"
    );
    assert_eq!(
        before["window"]["items"][0]["entryContext"]["originalOwner"]["address"],
        original.encode()
    );
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
            .any(|row| row["target"]
                == if nested {
                    middle_entry.encode()
                } else {
                    source.encode()
                }
                && row["label"] == "Copy lineage")
    );
    let pending = if during_read {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *fixture.state.trace.copy_context_read_pause.lock().await =
            Some((entered.clone(), release.clone()));
        let app = fixture.app.clone();
        let cookie = fixture.cookie.clone();
        let url = format!(
            "/api/v1/trace/agents/{}?at_snapshot={token}",
            recipient.encode()
        );
        let request = tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .uri(url)
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        Some((request, release))
    } else {
        None
    };
    if revoke {
        fixture
            .state
            .store
            .revoke_grant(&fixture.subject, "origin", "observe")
            .await
            .unwrap();
    } else {
        fixture
            .state
            .trace
            .evidence()
            .delete_scope(&IngestionScope::Agent(witness_owner))
            .await
            .unwrap();
    }
    if alternate {
        let (status, surviving) = fixture
            .get(&format!(
                "/api/v1/trace/agents/{}?at_snapshot={token}",
                recipient.encode()
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{surviving}");
        assert_eq!(
            surviving["window"]["items"][0]["entryContext"]["originalOwner"]["address"],
            original.encode(),
            "one retained independent copy certificate still proves the chain"
        );
        fixture
            .state
            .trace
            .evidence()
            .delete_scope(&IngestionScope::Agent(alternate_owner))
            .await
            .unwrap();
    }
    let (status, without_proof) = if let Some((request, release)) = pending {
        release.notify_one();
        response(request.await.unwrap()).await
    } else {
        fixture
            .get(&format!(
                "/api/v1/trace/agents/{}?at_snapshot={token}",
                recipient.encode()
            ))
            .await
    };
    assert_eq!(status, StatusCode::OK, "{without_proof}");
    assert!(
        without_proof
            .to_string()
            .contains("Independent retained copy")
    );
    assert_eq!(
        without_proof["window"]["items"][0]["entryContext"]["kind"], "unresolved",
        "a visible origin header cannot authorize a deleted copy witness"
    );
    assert!(without_proof["window"]["items"][0]["entryContext"]["originalOwner"].is_null());
    let (status, origin_header) = fixture
        .get(&format!("/api/v1/trace/agents/{}", original.encode()))
        .await;
    assert_eq!(status, StatusCode::OK, "{origin_header}");
    assert!(origin_header.to_string().contains("Original reader"));
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
    assert_eq!(
        hidden["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["label"] == "Copy lineage"),
        nested,
        "an independently retained immediate edge survives loss of its remote ancestor"
    );
}
