use super::*;
fn agent() -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:search").unwrap())
}
fn snapshot(revision: u64) -> Snapshot {
    Snapshot {
        generation: "search-generation".into(),
        evidence_cut: revision,
        view_revision: revision,
    }
}
fn document(key: &str, text: &str) -> SearchDocument {
    SearchDocument {
        key: key.into(),
        agent: agent(),
        original_owner: agent(),
        anchor: AnyObjectRef::HistoryEntry(
            EntryRef::native(agent(), "entry", vec![key.into()]).unwrap(),
        ),
        object: AnyObjectRef::Message(
            MessageRef::native(agent(), "message", vec![key.into()]).unwrap(),
        ),
        selection: None,
        mode: SearchHistoryMode::Authored,
        payload: PayloadRef::bytes("a".repeat(64)).unwrap(),
        representation: Representation::Conversation,
        body_revision: key.into(),
        byte_start: 0,
        text: text.into(),
        ingestion_epoch: IngestionEpoch::new("epoch").unwrap(),
    }
}
fn scope() -> SearchScope {
    SearchScope {
        agent: Some(agent()),
        selection: None,
        mode: SearchHistoryMode::Authored,
        representation: None,
    }
}

#[tokio::test]
async fn result_pages_keep_their_original_snippets_when_the_live_index_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SearchStore::open(
        &dir.path().join("search.db"),
        "search-generation".into(),
        SearchLimits::default(),
    )
    .await
    .unwrap();
    store
        .apply(
            0,
            &snapshot(1),
            &[
                document("a", "original durable capture"),
                document("b", "original retained result"),
            ],
            &[],
            true,
        )
        .await
        .unwrap();
    let page = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "original",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            &snapshot(1),
            "old-view",
            100,
            1,
        )
        .await
        .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert!(page.has_more);
    store
        .apply(
            1,
            &snapshot(2),
            &[document("b", "replaced newer bytes")],
            &[],
            true,
        )
        .await
        .unwrap();
    let second = store
        .resume_for(
            SearchQueryContext {
                subject: "reader",
                query: "original",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            page.next_cursor.as_ref().unwrap(),
            101,
            1,
        )
        .await
        .unwrap();
    assert_eq!(second.view, snapshot(1));
    assert_eq!(second.trace_lease, "old-view");
    assert!(second.hits[0].snippet.contains("original"));
    assert!(matches!(
        store
            .start_query(
                SearchQueryContext {
                    subject: "reader",
                    query: "original",
                    scope: &scope(),
                    allowed_environments: &["env7".into()].into()
                },
                &snapshot(1),
                "old-view",
                102,
                1
            )
            .await,
        Err(SearchError::Indexing)
    ));
}

#[tokio::test]
async fn chinese_search_respects_environment_and_result_job_subject() {
    let dir = tempfile::tempdir().unwrap();
    let store = SearchStore::open(
        &dir.path().join("search.db"),
        "search-generation".into(),
        SearchLimits::default(),
    )
    .await
    .unwrap();
    store
        .apply(
            0,
            &snapshot(1),
            &[
                document("a", "统一轨迹保留完整记录"),
                document("b", "统一轨迹第二条记录"),
            ],
            &[],
            true,
        )
        .await
        .unwrap();
    let hidden = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "轨迹",
                scope: &scope(),
                allowed_environments: &BTreeSet::new(),
            },
            &snapshot(1),
            "view",
            100,
            1,
        )
        .await
        .unwrap();
    assert!(hidden.hits.is_empty());
    let page = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "轨迹",
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            &snapshot(1),
            "view",
            101,
            1,
        )
        .await
        .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert!(page.has_more);
    assert!(matches!(
        store
            .resume_for(
                SearchQueryContext {
                    subject: "other",
                    query: "轨迹",
                    scope: &scope(),
                    allowed_environments: &["env7".into()].into()
                },
                page.next_cursor.as_ref().unwrap(),
                102,
                1
            )
            .await,
        Err(SearchError::SubjectMismatch)
    ));
    store.invalidate().await.unwrap();
    assert!(matches!(
        store
            .resume_for(
                SearchQueryContext {
                    subject: "reader",
                    query: "轨迹",
                    scope: &scope(),
                    allowed_environments: &["env7".into()].into()
                },
                page.next_cursor.as_ref().unwrap(),
                103,
                1
            )
            .await,
        Err(SearchError::Expired)
    ));
}

async fn open(limits: SearchLimits) -> (tempfile::TempDir, SearchStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SearchStore::open(
        &dir.path().join("search.db"),
        "search-generation".into(),
        limits,
    )
    .await
    .unwrap();
    (dir, store)
}

#[tokio::test]
async fn search_batch_bounds_include_text_keys_and_per_document_metadata() {
    let (_dir, store) = open(SearchLimits {
        max_batch_bytes: 4096,
        ..Default::default()
    })
    .await;
    assert!(matches!(
        store
            .apply(
                0,
                &snapshot(1),
                &[document("large-text", &"z".repeat(5000))],
                &[],
                true
            )
            .await,
        Err(SearchError::Limit("index batch bytes"))
    ));
    assert!(matches!(
        store
            .apply(0, &snapshot(1), &[], &["z".repeat(5000)], true)
            .await,
        Err(SearchError::Limit("index batch bytes"))
    ));
    let (_dir, store) = open(SearchLimits {
        max_document_metadata_bytes: 256,
        ..Default::default()
    })
    .await;
    assert!(matches!(
        store
            .apply(0, &snapshot(1), &[document("metadata", "short")], &[], true)
            .await,
        Err(SearchError::Limit("document metadata bytes"))
    ));
    assert!(store.checkpoint().await.unwrap().is_none());
}

#[tokio::test]
async fn invalid_fts_syntax_is_a_query_error_and_the_next_query_is_usable() {
    let (_dir, store) = open(SearchLimits::default()).await;
    store
        .apply(
            0,
            &snapshot(1),
            &[document("a", "searchable text")],
            &[],
            true,
        )
        .await
        .unwrap();
    let scope = scope();
    let allowed = ["env7".into()].into();
    let invalid = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "\"",
                scope: &scope,
                allowed_environments: &allowed,
            },
            &snapshot(1),
            "view",
            100,
            1,
        )
        .await;
    assert!(
        matches!(invalid, Err(SearchError::InvalidQuery(_))),
        "{invalid:?}"
    );
    let page = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "searchable",
                scope: &scope,
                allowed_environments: &allowed,
            },
            &snapshot(1),
            "view",
            100,
            1,
        )
        .await
        .unwrap();
    assert_eq!(page.hits.len(), 1);
}

#[tokio::test]
async fn reindex_between_the_preflight_and_query_snapshot_reports_indexing() {
    let (_dir, store) = open(SearchLimits::default()).await;
    store
        .apply(
            0,
            &snapshot(1),
            &[document("a", "searchable text")],
            &[],
            true,
        )
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *store.before_query.lock().await = Some((entered.clone(), resume.clone()));
    let querying = store.clone();
    let task = tokio::spawn(async move {
        querying
            .start_query(
                SearchQueryContext {
                    subject: "reader",
                    query: "searchable",
                    scope: &scope(),
                    allowed_environments: &["env7".into()].into(),
                },
                &snapshot(1),
                "view",
                100,
                1,
            )
            .await
    });
    entered.notified().await;
    store.begin_reindex().await.unwrap();
    resume.notify_one();
    assert!(matches!(task.await.unwrap(), Err(SearchError::Indexing)));
}

#[tokio::test]
async fn search_cursor_refuses_changed_text_scope_and_authorization_partition() {
    let (_dir, store) = open(SearchLimits::default()).await;
    store
        .apply(
            0,
            &snapshot(1),
            &[document("a", "shared text"), document("b", "shared text")],
            &[],
            true,
        )
        .await
        .unwrap();
    let scope = scope();
    let allowed = ["env7".into()].into();
    let context = SearchQueryContext {
        subject: "reader",
        query: "shared",
        scope: &scope,
        allowed_environments: &allowed,
    };
    let first = store
        .start_query(context, &snapshot(1), "view", 100, 1)
        .await
        .unwrap();
    let cursor = first.next_cursor.as_ref().unwrap();
    assert!(matches!(
        store
            .resume_for(
                SearchQueryContext {
                    query: "changed",
                    ..context
                },
                cursor,
                101,
                1
            )
            .await,
        Err(SearchError::InvalidCursor)
    ));
    let changed = SearchScope {
        mode: SearchHistoryMode::RetainedArchive,
        ..scope.clone()
    };
    assert!(matches!(
        store
            .resume_for(
                SearchQueryContext {
                    scope: &changed,
                    ..context
                },
                cursor,
                101,
                1
            )
            .await,
        Err(SearchError::InvalidCursor)
    ));
    assert!(matches!(
        store
            .resume_for(
                SearchQueryContext {
                    allowed_environments: &BTreeSet::new(),
                    ..context
                },
                cursor,
                101,
                1
            )
            .await,
        Err(SearchError::InvalidCursor)
    ));
    assert_eq!(
        store
            .resume_for(context, cursor, 101, 1)
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
}

#[tokio::test]
async fn result_byte_budget_is_checked_before_deserializing_full_candidate_metadata() {
    let (_dir, store) = open(SearchLimits {
        max_result_bytes: 256,
        ..Default::default()
    })
    .await;
    store
        .apply(
            0,
            &snapshot(1),
            &[document("a", "searchable text")],
            &[],
            true,
        )
        .await
        .unwrap();
    // A huge malformed metadata cell proves it was never hydrated: attempting to
    // deserialize first would fail instead of yielding explicit bounded coverage.
    sqlx::query("UPDATE search_documents SET document_json=?")
        .bind("{".repeat(1000))
        .execute(&store.writer)
        .await
        .unwrap();
    let page = store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: "searchable",
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
    assert!(page.hits.is_empty());
    assert!(page.result_limit_reached);
}
