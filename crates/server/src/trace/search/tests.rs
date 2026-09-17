use super::*;
fn agent() -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:search").unwrap())
}

#[tokio::test]
async fn search_writer_discards_an_acknowledged_begin_without_a_transaction_guard() {
    let (_dir, store) = open(SearchLimits::default()).await;
    crate::db::sqlite::assert_abandoned_begin_is_discarded(&store.writer).await;
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
        dependency_epochs: BTreeSet::new(),
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

async fn publish(
    store: &SearchStore,
    snapshot: &Snapshot,
    documents: &[SearchDocument],
) -> Result<()> {
    let update = store.begin_update(snapshot).await?;
    let key = blake3::hash(&serde_json::to_vec(documents)?)
        .to_hex()
        .to_string();
    if !store.prepare_source(&update, &key).await? {
        store.write_source_batch(&update, &key, documents).await?;
        store.complete_source(&update, &key).await?;
    }
    store.finish_update(&update).await.map(|_| ())
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
    publish(
        &store,
        &snapshot(1),
        &[
            document("a", "original durable capture"),
            document("b", "original retained result"),
        ],
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
    publish(
        &store,
        &snapshot(2),
        &[document("b", "replaced newer bytes")],
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
    publish(
        &store,
        &snapshot(1),
        &[
            document("a", "统一轨迹保留完整记录"),
            document("b", "统一轨迹第二条记录"),
        ],
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
        publish(
            &store,
            &snapshot(1),
            &[document("large-text", &"z".repeat(5000))],
        )
        .await,
        Err(SearchError::Limit("index batch bytes"))
    ));
    let update = store.begin_update(&snapshot(1)).await.unwrap();
    assert!(matches!(
        store.prepare_source(&update, &"z".repeat(5000)).await,
        Err(SearchError::Limit("source key bytes"))
    ));
    let (_dir, store) = open(SearchLimits {
        max_document_metadata_bytes: 256,
        ..Default::default()
    })
    .await;
    assert!(matches!(
        publish(&store, &snapshot(1), &[document("metadata", "short")]).await,
        Err(SearchError::Limit("document metadata bytes"))
    ));
    assert!(store.checkpoint().await.unwrap().is_none());
}

#[tokio::test]
async fn invalid_fts_syntax_is_a_query_error_and_the_next_query_is_usable() {
    let (_dir, store) = open(SearchLimits::default()).await;
    publish(&store, &snapshot(1), &[document("a", "searchable text")])
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
    publish(&store, &snapshot(1), &[document("a", "searchable text")])
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
    store.begin_update(&snapshot(2)).await.unwrap();
    resume.notify_one();
    assert!(matches!(task.await.unwrap(), Err(SearchError::Indexing)));
}

#[tokio::test]
async fn search_cursor_refuses_changed_text_scope_and_authorization_partition() {
    let (_dir, store) = open(SearchLimits::default()).await;
    publish(
        &store,
        &snapshot(1),
        &[document("a", "shared text"), document("b", "shared text")],
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
    publish(&store, &snapshot(1), &[document("a", "searchable text")])
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

async fn stage_source(
    store: &SearchStore,
    update: &SearchUpdate,
    key: &str,
    documents: &[SearchDocument],
) {
    assert!(!store.prepare_source(update, key).await.unwrap());
    for batch in documents.chunks(store.limits().max_documents_per_batch) {
        store.write_source_batch(update, key, batch).await.unwrap();
    }
    store.complete_source(update, key).await.unwrap();
}

async fn query(store: &SearchStore, snapshot: &Snapshot, text: &str) -> Result<SearchPage> {
    store
        .start_query(
            SearchQueryContext {
                subject: "reader",
                query: text,
                scope: &scope(),
                allowed_environments: &["env7".into()].into(),
            },
            snapshot,
            "retained-query-view",
            100,
            store.limits().max_results,
        )
        .await
}

async fn stored_documents(store: &SearchStore) -> Vec<(i64, String, String, String, String)> {
    sqlx::query_as("SELECT d.id,d.source_key,d.document_key,d.document_json,f.text FROM search_documents d JOIN search_fts f ON f.rowid=d.id ORDER BY d.id")
        .fetch_all(&store.readers).await.unwrap()
}

#[tokio::test]
async fn interrupted_source_restarts_without_retaining_partial_documents_or_terms() {
    let (directory, store) = open(SearchLimits {
        max_documents_per_batch: 2,
        ..Default::default()
    })
    .await;
    let target = snapshot(1);
    let update = store.begin_update(&target).await.unwrap();
    assert!(!store
        .prepare_source(&update, "interrupted-source")
        .await
        .unwrap());
    let count = store.limits().max_documents_per_batch * 3 + 1;
    let interrupted: Vec<_> = (0..count)
        .map(|index| document(&format!("part-{index}"), "unfinished token"))
        .collect();
    for batch in interrupted.chunks(store.limits().max_documents_per_batch) {
        store
            .write_source_batch(&update, "interrupted-source", batch)
            .await
            .unwrap();
    }
    assert_eq!(stored_documents(&store).await.len(), count);
    assert!(store.checkpoint().await.unwrap().is_none());
    store.readers.close().await;
    store.writer.close().await;
    let restored = SearchStore::open(
        &directory.path().join("search.db"),
        target.generation.clone(),
        store.limits().clone(),
    )
    .await
    .unwrap();
    assert!(restored.checkpoint().await.unwrap().is_none());
    assert!(matches!(
        query(&restored, &target, "unfinished").await,
        Err(SearchError::Indexing)
    ));
    let retry = restored.begin_update(&target).await.unwrap();
    assert!(!restored
        .prepare_source(&retry, "interrupted-source")
        .await
        .unwrap());
    assert!(stored_documents(&restored).await.is_empty());
    let remaining_terms: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM search_fts WHERE search_fts MATCH 'unfinished'")
            .fetch_one(&restored.readers)
            .await
            .unwrap();
    assert_eq!(remaining_terms, 0);
    // Reusing a document key also detects a partial row that escaped cleanup.
    restored
        .write_source_batch(
            &retry,
            "interrupted-source",
            &[document("part-0", "finished token")],
        )
        .await
        .unwrap();
    restored
        .complete_source(&retry, "interrupted-source")
        .await
        .unwrap();
    restored.finish_update(&retry).await.unwrap();
    assert_eq!(restored.checkpoint().await.unwrap(), Some(target.clone()));
    assert_eq!(
        query(&restored, &target, "finished")
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(query(&restored, &target, "unfinished")
        .await
        .unwrap()
        .hits
        .is_empty());
    assert!(matches!(
        restored.finish_update(&update).await,
        Err(SearchError::Indexing)
    ));
}

#[tokio::test]
async fn complete_source_reuse_preserves_document_ids_text_and_fts_storage() {
    let (_directory, store) = open(SearchLimits::default()).await;
    let first = store.begin_update(&snapshot(1)).await.unwrap();
    stage_source(
        &store,
        &first,
        "immutable-source",
        &[
            document("a", "original searchable text"),
            document("b", "original companion text"),
        ],
    )
    .await;
    store.finish_update(&first).await.unwrap();
    let before = stored_documents(&store).await;
    let fts_before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&store.readers)
            .await
            .unwrap();
    sqlx::raw_sql("CREATE TRIGGER immutable_document_insert BEFORE INSERT ON search_documents BEGIN SELECT RAISE(ABORT,'rewrote retained document'); END;
        CREATE TRIGGER immutable_document_update BEFORE UPDATE ON search_documents BEGIN SELECT RAISE(ABORT,'rewrote retained document'); END;
        CREATE TRIGGER immutable_document_delete BEFORE DELETE ON search_documents BEGIN SELECT RAISE(ABORT,'rewrote retained document'); END;").execute(&store.writer).await.unwrap();
    let second = store.begin_update(&snapshot(2)).await.unwrap();
    assert!(store
        .prepare_source(&second, "immutable-source")
        .await
        .unwrap());
    store.finish_update(&second).await.unwrap();
    assert_eq!(stored_documents(&store).await, before);
    let fts_after: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,block FROM search_fts_data ORDER BY id")
            .fetch_all(&store.readers)
            .await
            .unwrap();
    assert_eq!(fts_after, fts_before);
    assert_eq!(
        query(&store, &snapshot(2), "original")
            .await
            .unwrap()
            .hits
            .len(),
        2
    );
    assert_eq!(store.checkpoint().await.unwrap(), Some(snapshot(2)));
}

#[tokio::test]
async fn changed_and_vanished_sources_remove_every_document_across_removal_batches() {
    let (_directory, store) = open(SearchLimits {
        max_documents_per_batch: 2,
        ..Default::default()
    })
    .await;
    let count = store.limits().max_documents_per_batch * 3 + 1;
    let first = store.begin_update(&snapshot(1)).await.unwrap();
    for source in ["changed-v1", "vanished"] {
        let documents: Vec<_> = (0..count)
            .map(|index| document(&format!("{source}-{index}"), "obsolete text"))
            .collect();
        stage_source(&store, &first, source, &documents).await;
    }
    stage_source(
        &store,
        &first,
        "unchanged",
        &[document("keep", "kept text")],
    )
    .await;
    store.finish_update(&first).await.unwrap();
    assert_eq!(stored_documents(&store).await.len(), count * 2 + 1);
    let second = store.begin_update(&snapshot(2)).await.unwrap();
    assert!(store.prepare_source(&second, "unchanged").await.unwrap());
    stage_source(
        &store,
        &second,
        "changed-v2",
        &[document("replacement", "replacement text")],
    )
    .await;
    assert!(store.checkpoint().await.unwrap().is_none());
    let removed = store.finish_update(&second).await.unwrap();
    assert_eq!(removed, (count * 2) as u64);
    assert_eq!(stored_documents(&store).await.len(), 2);
    assert!(query(&store, &snapshot(2), "obsolete")
        .await
        .unwrap()
        .hits
        .is_empty());
    assert_eq!(
        query(&store, &snapshot(2), "replacement")
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(
        query(&store, &snapshot(2), "kept")
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    let sources: Vec<String> =
        sqlx::query_scalar("SELECT source_key FROM search_sources ORDER BY source_key")
            .fetch_all(&store.readers)
            .await
            .unwrap();
    assert_eq!(sources, vec!["changed-v2", "unchanged"]);
}

#[tokio::test]
async fn a_newer_same_cut_index_revision_is_valid_but_one_revision_cannot_change_its_cut() {
    let (_directory, store) = open(SearchLimits::default()).await;
    let first = snapshot(1);
    publish(&store, &first, &[document("one", "first text")])
        .await
        .unwrap();
    let same_cut = Snapshot {
        view_revision: 2,
        ..first.clone()
    };
    publish(&store, &same_cut, &[document("two", "second text")])
        .await
        .unwrap();
    assert_eq!(store.checkpoint().await.unwrap(), Some(same_cut.clone()));
    for wrong in [
        Snapshot {
            evidence_cut: 2,
            ..same_cut.clone()
        },
        Snapshot {
            evidence_cut: 0,
            ..same_cut.clone()
        },
        first,
    ] {
        assert!(matches!(
            store.begin_update(&wrong).await,
            Err(SearchError::Indexing)
        ));
        assert_eq!(store.checkpoint().await.unwrap(), Some(same_cut.clone()));
    }
    assert_eq!(
        query(&store, &same_cut, "second").await.unwrap().hits.len(),
        1
    );
    assert!(query(&store, &same_cut, "first")
        .await
        .unwrap()
        .hits
        .is_empty());
}

#[tokio::test]
async fn replacement_and_invalidation_fence_every_operation_of_an_old_index_writer() {
    for invalidate in [false, true] {
        let (_directory, store) = open(SearchLimits::default()).await;
        let target = snapshot(1);
        let stale = store.begin_update(&target).await.unwrap();
        assert!(!store.prepare_source(&stale, "old-source").await.unwrap());
        store
            .write_source_batch(&stale, "old-source", &[document("old", "old text")])
            .await
            .unwrap();
        if invalidate {
            store.invalidate().await.unwrap();
        }
        let current = if invalidate {
            None
        } else {
            Some(store.begin_update(&target).await.unwrap())
        };
        let before = stored_documents(&store).await;
        assert!(matches!(
            store.prepare_source(&stale, "stale-source").await,
            Err(SearchError::Indexing)
        ));
        assert!(matches!(
            store
                .write_source_batch(&stale, "old-source", &[document("extra", "stale text")])
                .await,
            Err(SearchError::Indexing)
        ));
        assert!(matches!(
            store.complete_source(&stale, "old-source").await,
            Err(SearchError::Indexing)
        ));
        assert!(matches!(
            store.finish_update(&stale).await,
            Err(SearchError::Indexing)
        ));
        assert_eq!(stored_documents(&store).await, before);
        assert!(store.checkpoint().await.unwrap().is_none());
        let current = match current {
            Some(update) => update,
            None => store.begin_update(&target).await.unwrap(),
        };
        stage_source(
            &store,
            &current,
            "new-source",
            &[document("new", "new text")],
        )
        .await;
        store.finish_update(&current).await.unwrap();
        assert!(matches!(
            store.finish_update(&stale).await,
            Err(SearchError::Indexing)
        ));
        assert_eq!(store.checkpoint().await.unwrap(), Some(target.clone()));
        assert_eq!(query(&store, &target, "new").await.unwrap().hits.len(), 1);
        assert!(query(&store, &target, "old").await.unwrap().hits.is_empty());
    }
}

#[tokio::test]
async fn an_incomplete_source_cannot_publish_even_after_its_document_batch_commits() {
    let (_directory, store) = open(SearchLimits::default()).await;
    let target = snapshot(1);
    let update = store.begin_update(&target).await.unwrap();
    assert!(!store.prepare_source(&update, "unfinished").await.unwrap());
    stage_source(
        &store,
        &update,
        "finished",
        &[document("finished", "finished text")],
    )
    .await;
    for write in [false, true] {
        if write {
            store
                .write_source_batch(
                    &update,
                    "unfinished",
                    &[document("unfinished", "pending text")],
                )
                .await
                .unwrap();
        }
        assert!(matches!(
            store.finish_update(&update).await,
            Err(SearchError::Indexing)
        ));
        assert!(store.checkpoint().await.unwrap().is_none());
        assert!(matches!(
            query(&store, &target, "finished").await,
            Err(SearchError::Indexing)
        ));
    }
    store.complete_source(&update, "unfinished").await.unwrap();
    store.finish_update(&update).await.unwrap();
    assert_eq!(store.checkpoint().await.unwrap(), Some(target.clone()));
    assert_eq!(
        query(&store, &target, "finished").await.unwrap().hits.len(),
        1
    );
    assert_eq!(
        query(&store, &target, "pending").await.unwrap().hits.len(),
        1
    );
}
