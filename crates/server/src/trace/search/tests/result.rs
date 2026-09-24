use super::*;

#[tokio::test]
async fn result_references_bind_one_subject_and_position_until_expiry_or_invalidation() {
    let (_directory, store) = open(SearchLimits::default()).await;
    publish(
        &store,
        &snapshot(1),
        &[
            document("one", "shared first"),
            document("two", "shared second"),
        ],
    )
    .await
    .unwrap();
    let page = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "shared",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            &snapshot(1),
            "view",
            100,
            1,
        )
        .await
        .unwrap();
    let next = store
        .page("reader", &page.job_id, page.next_cursor.as_deref(), 101, 1)
        .await
        .unwrap();
    assert_eq!(next.offset, 1);
    let reference = SearchStore::result_reference(&next.job_id, next.offset).unwrap();
    let retained = store.read_result("reader", &reference, 102).await.unwrap();
    assert_eq!(retained.hit, next.hits[0]);
    assert_eq!(retained.view, snapshot(1));
    assert!(matches!(
        store.read_result("other", &reference, 102).await,
        Err(SearchError::SubjectMismatch)
    ));
    assert!(matches!(
        store
            .read_result(
                "reader",
                &SearchStore::result_reference(&page.job_id, 2).unwrap(),
                102
            )
            .await,
        Err(SearchError::InvalidCursor)
    ));
    assert!(matches!(
        store
            .read_result(
                "reader",
                &"x".repeat(store.limits().max_query_bytes + 1),
                102
            )
            .await,
        Err(SearchError::InvalidCursor)
    ));
    publish(&store, &snapshot(2), &[document("new", "replacement")])
        .await
        .unwrap();
    assert_eq!(
        store
            .read_result("reader", &reference, 103)
            .await
            .unwrap()
            .hit,
        retained.hit
    );
    assert!(matches!(
        store
            .read_result("reader", &reference, retained.expires_at_ms)
            .await,
        Err(SearchError::Expired)
    ));
    store.invalidate().await.unwrap();
    assert!(matches!(
        store.read_result("reader", &reference, 104).await,
        Err(SearchError::Expired)
    ));
}

#[tokio::test]
async fn materialized_results_include_array_framing_in_their_byte_budget() {
    let (_directory, store) = open(SearchLimits::default()).await;
    let documents = [document("one", "shared text")];
    publish(&store, &snapshot(1), &documents).await.unwrap();
    let first = query(&store, &snapshot(1), "shared").await.unwrap();
    let actual = serde_json::to_vec(&first.hits).unwrap().len();
    for budget in [actual - 1, actual] {
        let (_directory, bounded) = open(SearchLimits {
            max_result_bytes: budget,
            ..Default::default()
        })
        .await;
        publish(&bounded, &snapshot(1), &documents).await.unwrap();
        let page = query(&bounded, &snapshot(1), "shared").await.unwrap();
        assert!(serde_json::to_vec(&page.hits).unwrap().len() <= bounded.limits().max_result_bytes);
        assert_eq!(page.hits.len(), usize::from(budget == actual));
        assert_eq!(page.result_limit_reached, budget < actual);
    }
}

#[tokio::test]
async fn oversized_result_jobs_are_refused_before_their_corrupt_json_is_loaded() {
    let (_directory, store) = open(SearchLimits::default()).await;
    publish(&store, &snapshot(1), &[document("one", "shared text")])
        .await
        .unwrap();
    let page = query(&store, &snapshot(1), "shared").await.unwrap();
    let reference = SearchStore::result_reference(&page.job_id, 0).unwrap();
    sqlx::query("UPDATE search_jobs SET hits_json=? WHERE id=?")
        .bind("{".repeat(store.limits().max_result_bytes + 1))
        .bind(&page.job_id)
        .execute(&store.writer)
        .await
        .unwrap();
    assert!(matches!(
        store.read_result("reader", &reference, 101).await,
        Err(SearchError::Limit("result job bytes"))
    ));
}
