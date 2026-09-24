use super::*;

fn selection(key: &str) -> SelectionRef {
    SelectionRef::native(agent(), "membership-test", vec![key.into()]).unwrap()
}

async fn write_memberships(
    store: &SearchStore,
    update: &SearchUpdate,
    memberships: &[SearchMembership],
) {
    for batch in memberships.chunks(store.limits().max_documents_per_batch) {
        store
            .write_membership_batch(update, "body", batch)
            .await
            .unwrap();
    }
    store.complete_memberships(update, "body").await.unwrap();
}

async fn selected_query(
    store: &SearchStore,
    snapshot: &Snapshot,
    selection: SelectionRef,
) -> SearchPage {
    store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "shared",
                scope: &SearchScope {
                    agent: Some(agent()),
                    selection: Some(selection),
                    mode: SearchHistoryMode::SelectedContext,
                    representation: None,
                },
                allowed_environments: &["env7".into()].into(),
            },
            snapshot,
            "retained-query-view",
            101,
            2,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn interrupted_membership_restarts_without_rewriting_bodies_or_changing_frozen_results() {
    let (directory, store) = open(SearchLimits {
        max_documents_per_batch: 2,
        ..Default::default()
    })
    .await;
    let first = store.begin_update(&snapshot(1)).await.unwrap();
    stage_source(
        &store,
        &first,
        "body",
        &[
            document("one", "shared first body"),
            document("two", "shared second body"),
        ],
    )
    .await;
    store.finish_update(&first).await.unwrap();
    let frozen = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "shared",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            &snapshot(1),
            "frozen-view",
            100,
            1,
        )
        .await
        .unwrap();
    let bodies = stored_documents(&store).await;
    let fts: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&store.readers)
            .await
            .unwrap();
    let partial = store.begin_update(&snapshot(2)).await.unwrap();
    assert!(store.prepare_source(&partial, "body").await.unwrap());
    store
        .write_membership_batch(
            &partial,
            "body",
            &[SearchMembership::SelectedContext(selection("discarded"))],
        )
        .await
        .unwrap();
    assert!(matches!(
        store.finish_update(&partial).await,
        Err(SearchError::Indexing)
    ));
    assert!(matches!(
        query(&store, &snapshot(2), "shared").await,
        Err(SearchError::Indexing)
    ));
    store.readers.close().await;
    store.writer.close().await;
    let restored = SearchStore::open(
        &directory.path().join("search.db"),
        snapshot(2).generation,
        store.limits().clone(),
    )
    .await
    .unwrap();
    assert!(restored.checkpoint().await.unwrap().is_none());
    let retry = restored.begin_update(&snapshot(2)).await.unwrap();
    assert!(restored.prepare_source(&retry, "body").await.unwrap());
    let count = restored.limits().max_documents_per_batch * 3 + 1;
    let memberships: Vec<_> = (0..count)
        .map(|index| SearchMembership::SelectedContext(selection(&format!("kept-{index}"))))
        .collect();
    write_memberships(&restored, &retry, &memberships).await;
    restored.finish_update(&retry).await.unwrap();
    assert_eq!(stored_documents(&restored).await, bodies);
    let retained_fts: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&restored.readers)
            .await
            .unwrap();
    assert_eq!(retained_fts, fts);
    assert!(query(&restored, &snapshot(2), "shared")
        .await
        .unwrap()
        .hits
        .is_empty());
    assert!(
        selected_query(&restored, &snapshot(2), selection("discarded"))
            .await
            .hits
            .is_empty()
    );
    for index in [0, count - 1] {
        let selected = selection(&format!("kept-{index}"));
        let page = selected_query(&restored, &snapshot(2), selected.clone()).await;
        assert_eq!(page.hits.len(), 2);
        assert!(page
            .hits
            .iter()
            .all(|hit| hit.context.selection.as_ref() == Some(&selected)));
    }
    assert!(matches!(
        restored
            .write_membership_batch(&partial, "body", &[SearchMembership::Authored])
            .await,
        Err(SearchError::Indexing)
    ));
    assert!(matches!(
        restored.complete_memberships(&partial, "body").await,
        Err(SearchError::Indexing)
    ));
    let removed = restored.begin_update(&snapshot(3)).await.unwrap();
    assert_eq!(
        restored.finish_update(&removed).await.unwrap(),
        bodies.len() as u64
    );
    assert!(stored_documents(&restored).await.is_empty());
    assert!(selected_query(&restored, &snapshot(3), selection("kept-0"))
        .await
        .hits
        .is_empty());
    let second = restored
        .resume_for(
            SearchQueryContext {
                subject: "reader",
                query: "shared",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            frozen.next_cursor.as_ref().unwrap(),
            102,
            1,
        )
        .await
        .unwrap();
    assert_eq!(second.view, snapshot(1));
    assert_eq!(second.trace_lease, "frozen-view");
    assert_eq!(second.hits.len(), 1);
    assert!(second.hits[0].context.snippet.contains("shared"));
}

#[tokio::test]
async fn membership_batches_refuse_excess_count_and_serialized_bytes_before_writing() {
    let (_directory, store) = open(SearchLimits {
        max_documents_per_batch: 2,
        max_batch_bytes: 4096,
        ..Default::default()
    })
    .await;
    let update = store.begin_update(&snapshot(1)).await.unwrap();
    assert!(!store.prepare_source(&update, "body").await.unwrap());
    let too_many = vec![SearchMembership::Authored; store.limits().max_documents_per_batch + 1];
    assert!(matches!(
        store
            .write_membership_batch(&update, "body", &too_many)
            .await,
        Err(SearchError::Limit("membership batch count"))
    ));
    let oversized =
        SearchMembership::SelectedContext(selection(&"x".repeat(store.limits().max_batch_bytes)));
    assert!(matches!(
        store
            .write_membership_batch(&update, "body", &[oversized])
            .await,
        Err(SearchError::Limit("membership batch bytes"))
    ));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM search_memberships")
        .fetch_one(&store.readers)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn selected_result_metadata_is_budgeted_before_deserializing_the_indexed_body() {
    let (_directory, store) = open(SearchLimits {
        max_result_bytes: 2048,
        ..Default::default()
    })
    .await;
    let update = store.begin_update(&snapshot(1)).await.unwrap();
    assert!(!store.prepare_source(&update, "body").await.unwrap());
    store
        .write_source_batch(&update, "body", &[document("one", "shared content")])
        .await
        .unwrap();
    store.complete_body(&update, "body").await.unwrap();
    let selected = selection(&"x".repeat(store.limits().max_result_bytes));
    write_memberships(
        &store,
        &update,
        &[SearchMembership::SelectedContext(selected.clone())],
    )
    .await;
    store.finish_update(&update).await.unwrap();
    // The body metadata fits by itself but the query-owned selection does not.
    // A malformed cell makes an attempted early hydration fail unmistakably.
    sqlx::query("UPDATE search_documents SET document_json=?")
        .bind("{".repeat(store.limits().max_result_bytes / 2))
        .execute(&store.writer)
        .await
        .unwrap();
    let result = selected_query(&store, &snapshot(1), selected).await;
    assert!(result.hits.is_empty());
    assert!(result.result_limit_reached);
}
