use super::*;

fn selection_claim(owner: &AgentRef, entry: &EntryRef, source: &Capture, index: usize) -> Claim {
    let key = format!("selection-{index}");
    claim(
        owner,
        &key,
        TraceObject::HistorySelection(HistorySelection {
            selection_ref: SelectionRef::native(owner.clone(), "membership", vec![key.clone()])
                .unwrap(),
            agent: owner.clone(),
            membership: SelectionMembership::EntrySet {
                entries: vec![entry.clone()],
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }),
        &[source],
    )
}

#[tokio::test]
async fn retained_selections_share_indexed_text_and_later_membership_does_not_reread_the_body() {
    let mut f = ProvenanceFixture::new().await;
    let indexer = f.state.trace.fixture_indexer().await;
    let owner = agent("recipient", "selection-reuse");
    let index = f
        .state
        .trace
        .search_store(&f.generation.manifest().generation)
        .await
        .unwrap();
    let count = index.limits().max_documents_per_batch * 2 + 1;
    let text = format!(
        "membershipneedle {}",
        "x".repeat(index.limits().max_document_bytes / 2)
    );
    let source = f.capture(&owner, "retained-body", text.as_bytes()).await;
    let entry = EntryRef::native(owner.clone(), "membership", vec!["one".into()]).unwrap();
    let message = MessageRef::native(owner.clone(), "membership", vec!["one".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(owner.clone(), "membership", vec!["one".into()]).unwrap();
    let objects = [
        agent_object(&owner, "Selection reuse"),
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: entry.clone(),
            containing_agent: owner.clone(),
            predecessor: None,
            original_owner: None,
        }),
        TraceObject::Message(Message {
            message_ref: message.clone(),
            author: Author::Unknown,
            protocol_role: None,
            purpose: MessagePurpose::Unclassified,
            production: None,
        }),
        TraceObject::MessageRevision(MessageRevision {
            revision_ref: revision.clone(),
            message,
            representation: Representation::Conversation,
            form: RevisionForm::Complete,
            parts: vec![ContentPart {
                part_ref: MessagePartRef::native(owner.clone(), "membership", vec!["one".into()])
                    .unwrap(),
                kind: ContentKind::Text,
                payload: source.payload_ref.clone(),
                media_type: Some("text/plain".into()),
                encoding: PayloadEncoding::Utf8,
            }],
        }),
    ];
    let mut claims: Vec<_> = objects
        .into_iter()
        .enumerate()
        .map(|(index, object)| claim(&owner, &format!("object-{index}"), object, &[&source]))
        .collect();
    let mut content = claim(
        &owner,
        "content",
        agent_object(&owner, "unused"),
        &[&source],
    );
    content.fact = Fact::HistoryContentObserved {
        observation: HistoryContentObservation::complete(
            entry.clone(),
            source.source_position.clone(),
            "membership",
            vec![HistoryContent::Message(revision)],
        ),
    };
    claims.push(content);
    f.committed.previews.insert(
        source.payload_ref.clone(),
        Arc::new(sylph_trace::reading::PayloadPreview {
            preview: text[..2048.min(text.len())].into(),
            preview_limit: 2048,
            total_bytes: Some(text.len() as u64),
            ranges: vec![PayloadRange {
                start: 0,
                end_exclusive: text.len() as u64,
            }],
            availability: Some(ReadAvailability::Available),
            ..Default::default()
        }),
    );
    f.publish(claims, &[&source]).await;
    // The index must span multiple membership batches without making fixture
    // publication exceed the independent projection transaction bound.
    let selections: Vec<_> = (0..count)
        .map(|index| selection_claim(&owner, &entry, &source, index))
        .collect();
    for batch in selections.chunks(8) {
        f.publish(batch.to_vec(), &[&source]).await;
    }
    let cold = indexer.index_pending().await.unwrap();
    let db = crate::db::sqlite::pool(
        sqlx::sqlite::SqliteConnectOptions::new().filename(
            f.state
                .config
                .data_dir
                .join(format!("search-{}.db", f.generation.manifest().generation)),
        ),
        1,
        true,
        true,
    )
    .await
    .unwrap();
    let before: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT d.id,d.document_json,f.text FROM search_documents d JOIN search_fts f ON f.rowid=d.id ORDER BY d.id"
    ).fetch_all(&db).await.unwrap();
    assert_eq!(
        before.len(),
        1,
        "history memberships duplicated the indexed body"
    );
    assert_eq!(cold.documents_written, before.len() as u64);
    assert_eq!(cold.payload_bytes_read, text.len() as u64);
    let fts_before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();

    let later = f
        .capture(&owner, "later-selection", b"another retained selection")
        .await;
    f.publish(
        vec![selection_claim(&owner, &entry, &later, count)],
        &[&later],
    )
    .await;
    let warm = indexer.index_pending().await.unwrap();
    assert_eq!(warm.sources_examined, 1);
    assert_eq!(warm.sources_reused, warm.sources_examined);
    assert_eq!(
        warm.payload_bytes_read, 0,
        "membership alone reread retained text"
    );
    assert_eq!(warm.documents_written, 0);
    assert_eq!(warm.documents_removed, 0);
    let after: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT d.id,d.document_json,f.text FROM search_documents d JOIN search_fts f ON f.rowid=d.id ORDER BY d.id"
    ).fetch_all(&db).await.unwrap();
    assert_eq!(after, before);
    let fts_after: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        fts_after, fts_before,
        "membership alone rewrote FTS storage"
    );
    for index in [0, count / 2, count - 1, count] {
        let selection = SelectionRef::native(
            owner.clone(),
            "membership",
            vec![format!("selection-{index}")],
        )
        .unwrap();
        let (status, result) = f
            .get(&format!(
            "/api/v1/trace/search?agent={}&mode=selected_context&selection={}&q=membershipneedle",
            owner.encode(), selection.encode()
        ))
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["items"][0]["history"]["selection"],
            selection.encode()
        );
        assert!(result["items"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("membershipneedle"));
    }
    db.close().await;
}
