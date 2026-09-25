use super::*;

#[tokio::test]
async fn inherited_only_header_survives_absent_origin_and_clears_on_the_first_local_append() {
    let mut f = ProvenanceFixture::new().await;
    let owner = agent("recipient", "context-only");
    let origin = agent("origin", "unavailable-original");
    let source = f
        .capture(&owner, "inherited-content", b"copied retained text")
        .await;
    let header_source = f
        .capture(
            &agent("recipient", "header-support"),
            "header",
            b"retained agent metadata",
        )
        .await;
    let entry = |index: usize| {
        EntryRef::native(owner.clone(), "context-only", vec![index.to_string()]).unwrap()
    };
    let mut previous = None;
    let mut old_view = None;
    for index in 0..3 {
        let capture = if index == 0 {
            source.clone()
        } else {
            f.capture(
                &owner,
                &format!("local-{index}"),
                format!("local text {index}").as_bytes(),
            )
            .await
        };
        let message =
            MessageRef::native(owner.clone(), "context-only", vec![index.to_string()]).unwrap();
        let revision =
            MessageRevisionRef::native(owner.clone(), "context-only", vec![index.to_string()])
                .unwrap();
        let mut values = vec![];
        if index == 0 {
            values.push(claim(
                &owner,
                "agent",
                agent_object(&owner, "Context reader"),
                &[&header_source],
            ));
        }
        for (label, object) in [
            (
                "entry",
                TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry(index),
                    containing_agent: owner.clone(),
                    predecessor: previous,
                    original_owner: (index == 0).then(|| OwnerRef::Agent {
                        agent: origin.clone(),
                    }),
                }),
            ),
            (
                "message",
                TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Unknown,
                    protocol_role: None,
                    purpose: MessagePurpose::Unclassified,
                    production: None,
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
                        part_ref: MessagePartRef::native(
                            owner.clone(),
                            "context-only",
                            vec![index.to_string()],
                        )
                        .unwrap(),
                        kind: ContentKind::Text,
                        payload: capture.payload_ref.clone(),
                        media_type: Some("text/plain".into()),
                        encoding: PayloadEncoding::Utf8,
                    }],
                }),
            ),
        ] {
            values.push(claim(
                &owner,
                &format!("{label}-{index}"),
                object,
                &[&capture],
            ));
        }
        let mut content = claim(
            &owner,
            &format!("content-{index}"),
            agent_object(&owner, "unused"),
            &[&capture],
        );
        content.fact = Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry(index),
                capture.source_position.clone(),
                "context-only",
                vec![HistoryContent::Message(revision)],
            ),
        };
        values.push(content);
        let bytes = f
            .state
            .trace
            .evidence()
            .payload_bytes(&capture.payload_ref, 4096)
            .await
            .unwrap();
        f.committed.previews.insert(
            capture.payload_ref.clone(),
            Arc::new(sylph_trace::reading::PayloadPreview {
                preview: String::from_utf8(bytes.clone()).unwrap(),
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
        let changes = f.publish(values, &[&capture, &header_source]).await;
        let current = f.bootstrap(&owner).await;
        assert_eq!(
            current["header"]["inheritedContextOnly"],
            index == 0,
            "{current}"
        );
        assert!(current.to_string().contains("copied retained text"));
        if index == 0 {
            let row = f.row(&owner, "header", &owner.encode()).await;
            assert!(
                row.dependency_epochs
                    .contains(&source.ingestion_authorization_epoch),
                "the inherited-only header lost its entry evidence"
            );
            assert_ne!(row.epoch, source.ingestion_authorization_epoch);
            assert!(current["window"]["items"][0]["entryContext"]["originalOwner"].is_null());
            old_view = Some(current["view"]["token"].as_str().unwrap().to_owned());
        } else {
            assert!(current.to_string().contains(&format!("local text {index}")));
            if index == 1 {
                assert!(
                    changes
                        .iter()
                        .any(|row| row.scope == read_scope(&owner, None, "header")),
                    "first local history did not update the retained header"
                );
            }
            if index == 2 {
                assert!(
                    !changes
                        .iter()
                        .any(|row| row.scope == read_scope(&owner, None, "header")),
                    "ordinary local tails should reuse the header"
                );
            }
            let (status, old) = f
                .get(&format!(
                    "/api/v1/trace/agents/{}?at_snapshot={}",
                    owner.encode(),
                    old_view.as_ref().unwrap()
                ))
                .await;
            assert_eq!(status, StatusCode::OK, "{old}");
            assert_eq!(old["header"]["inheritedContextOnly"], true);
            assert!(!old.to_string().contains("local text"));
        }
        previous = Some(entry(index));
    }
}
