use super::*;

#[tokio::test]
async fn search_scope_keeps_unknown_copies_discoverable_without_claiming_local_authorship() {
    let mut f = ProvenanceFixture::new().await;
    let owner = agent("recipient", "search-scope");
    let source = f.capture(&owner, "scope-fixture", b"scope fixture").await;
    let mut values = vec![claim(
        &owner,
        "agent",
        agent_object(&owner, "Scoped reader"),
        &[&source],
    )];
    let mut captures = vec![source.clone()];
    let mut entries = vec![];
    for (index, name) in ["local", "human", "unresolved", "copied", "foreign"]
        .into_iter()
        .enumerate()
    {
        let bytes = format!("scopeword {name}");
        let body = f.capture(&owner, name, bytes.as_bytes()).await;
        let entry = EntryRef::native(owner.clone(), "scope", vec![name.into()]).unwrap();
        let message = MessageRef::native(owner.clone(), "scope", vec![name.into()]).unwrap();
        let revision =
            MessageRevisionRef::native(owner.clone(), "scope", vec![name.into()]).unwrap();
        let origin = agent("origin", "source");
        let author = match name {
            "local" => Author::Agent {
                agent: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            },
            "human" => Author::Human { subject: None },
            "foreign" => Author::Agent {
                agent: OwnerRef::Agent {
                    agent: origin.clone(),
                },
            },
            _ => Author::Unknown,
        };
        let original = match name {
            "unresolved" => Some(OwnerRef::Unresolved {
                scope: owner.clone(),
                namespace: "fork".into(),
                native_key: vec![name.into()],
                reason: "Unproved membership".into(),
            }),
            "copied" => Some(OwnerRef::Agent { agent: origin }),
            _ => None,
        };
        for (key, object) in [
            (
                "entry",
                TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry.clone(),
                    containing_agent: owner.clone(),
                    predecessor: None,
                    original_owner: original,
                }),
            ),
            (
                "message",
                TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author,
                    protocol_role: None,
                    purpose: MessagePurpose::Unclassified,
                    production: if name == "local" {
                        Some(ProductionScope {
                            owner: OwnerRef::Agent {
                                agent: owner.clone(),
                            },
                            activation: None,
                        })
                    } else {
                        None
                    },
                }),
            ),
            (
                "revision",
                TraceObject::MessageRevision(MessageRevision {
                    revision_ref: revision.clone(),
                    message,
                    representation: Representation::Conversation,
                    form: RevisionForm::Complete,
                    parts: vec![ContentPart {
                        part_ref: MessagePartRef::native(owner.clone(), "scope", vec![name.into()])
                            .unwrap(),
                        kind: ContentKind::Text,
                        payload: body.payload_ref.clone(),
                        media_type: Some("text/plain".into()),
                        encoding: PayloadEncoding::Utf8,
                    }],
                }),
            ),
        ] {
            values.push(claim(&owner, &format!("{key}-{index}"), object, &[&body]));
        }
        let mut content = claim(
            &owner,
            &format!("content-{index}"),
            agent_object(&owner, "unused"),
            &[&body],
        );
        content.fact = Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                body.source_position.clone(),
                "scope-fixture",
                vec![HistoryContent::Message(revision)],
            ),
        };
        values.push(content);
        f.committed.previews.insert(
            body.payload_ref.clone(),
            Arc::new(sylph_trace::reading::PayloadPreview {
                preview: bytes.clone(),
                preview_limit: 2048,
                total_bytes: Some(bytes.len() as u64),
                ranges: vec![PayloadRange {
                    start: 0,
                    end_exclusive: bytes.len() as u64,
                }],
                availability: Some(ReadAvailability::Available),
                ..Default::default()
            }),
        );
        entries.push(entry);
        captures.push(body);
    }
    let selection = SelectionRef::native(owner.clone(), "scope", vec!["selected".into()]).unwrap();
    values.push(claim(
        &owner,
        "selection",
        TraceObject::HistorySelection(HistorySelection {
            selection_ref: selection.clone(),
            agent: owner.clone(),
            membership: SelectionMembership::EntrySet {
                entries: entries[..2].to_vec(),
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }),
        &[&source],
    ));
    let mut selected = claim(
        &owner,
        "selected",
        agent_object(&owner, "unused"),
        &[&source],
    );
    selected.fact = Fact::SelectHistory {
        agent: owner.clone(),
        selection: selection.clone(),
        selected_at: source.source_position.clone(),
    };
    values.push(selected);
    let older = SelectionRef::native(owner.clone(), "scope", vec!["older".into()]).unwrap();
    values.push(claim(
        &owner,
        "older-selection",
        TraceObject::HistorySelection(HistorySelection {
            selection_ref: older.clone(),
            agent: owner.clone(),
            membership: SelectionMembership::EntrySet {
                entries: vec![entries[2].clone()],
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }),
        &[&source],
    ));
    let unresolved_selection =
        SelectionRef::native(owner.clone(), "scope", vec!["unresolved-membership".into()]).unwrap();
    values.push(claim(
        &owner,
        "unresolved-selection",
        TraceObject::HistorySelection(HistorySelection {
            selection_ref: unresolved_selection.clone(),
            agent: owner.clone(),
            membership: SelectionMembership::EntryPath {
                head: EntryRef::native(owner.clone(), "scope", vec!["missing-head".into()])
                    .unwrap(),
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }),
        &[&source],
    ));
    f.publish(values, &captures.iter().collect::<Vec<_>>())
        .await;
    f.state.trace.index_pending().await.unwrap();
    let base = format!("/api/v1/trace/search?agent={}&q=scopeword", owner.encode());
    let (status, all) = f.get(&base).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["items"].as_array().unwrap().len(), 5);
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["history"]["archive"] == true)
    );
    let (status, authored) = f.get(&format!("{base}&mode=authored")).await;
    assert_eq!(status, StatusCode::OK, "{authored}");
    assert_eq!(authored["items"].as_array().unwrap().len(), 1);
    assert!(
        authored["items"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("local")
    );
    let (status, context) = f
        .get(&format!(
            "{base}&mode=selected_context&selection={}",
            selection.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{context}");
    assert_eq!(context["items"].as_array().unwrap().len(), 2);
    assert!(
        context["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|hit| hit["history"]["archive"] == false)
    );
    for suffix in [
        "&mode=selected_context".to_owned(),
        format!("&mode=authored&selection={}", selection.encode()),
        format!("&mode=retained_archive&selection={}", selection.encode()),
        "&mode=imagined".into(),
    ] {
        assert_eq!(
            f.get_response(&format!("{base}{suffix}")).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let (status, older_hits) = f
        .get(&format!(
            "{base}&mode=selected_context&selection={}",
            older.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{older_hits}");
    assert_eq!(
        older_hits["items"].as_array().unwrap().len(),
        1,
        "a retained older selection was reported empty"
    );
    assert!(
        older_hits["items"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("unresolved")
    );
    let missing =
        SelectionRef::native(owner.clone(), "scope", vec!["absent-selection".into()]).unwrap();
    assert_eq!(
        f.get_response(&format!(
            "{base}&mode=selected_context&selection={}",
            missing.encode()
        ))
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let (status, unavailable) = f
        .get(&format!(
            "{base}&mode=selected_context&selection={}",
            unresolved_selection.encode()
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{unavailable}");
    assert_eq!(unavailable["error"]["code"], "history_unavailable");
    let checkpoint = all["view"]["token"].as_str().unwrap();
    let (status, archive) = f
        .get(&format!(
            "/api/v1/trace/agents/{}?archive=true&around={}&at_snapshot={checkpoint}",
            owner.encode(),
            entries[2].encode()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{archive}");
    assert_eq!(archive["view"], all["view"]);
    assert_eq!(archive["header"]["history"]["archive"], true);
    assert_eq!(archive["header"]["history"]["isCurrent"], false);
    assert!(archive.to_string().contains("scopeword unresolved"));
    assert_eq!(
        f.get(&format!(
            "/api/v1/trace/agents/{}?archive=true&selection={}",
            owner.encode(),
            selection.encode()
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (_, first) = f
        .get(&format!("{base}&mode=retained_archive&limit=1"))
        .await;
    let cursor = first["afterCursor"].as_str().unwrap();
    let token = first["view"]["token"].as_str().unwrap();
    assert_eq!(
        f.get(&format!(
            "{base}&mode=authored&cursor={cursor}&at_snapshot={token}"
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
