use super::*;
use crate::trace::generation::{GenerationManifest, ProjectionBatch, QueryLimits};

async fn store() -> (tempfile::TempDir, GenerationStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = GenerationStore::create(
        dir.path().join("trace.db"),
        GenerationManifest {
            generation: "reading".into(),
            semantic_schema: "1".into(),
            mapper_builds: [("codex".into(), "1".into())].into(),
            reducer_build: "1".into(),
            capture_inventory_build: "1".into(),
            encoding_build: "1".into(),
        },
        QueryLimits {
            max_candidates: 8,
            ..QueryLimits::default()
        },
    )
    .await
    .unwrap();
    (dir, store)
}
fn row(address: &str, position: u64, text: &str) -> ReadRowChange {
    ReadRowChange {
        scope: "history".into(),
        address: address.into(),
        anchor: None,
        order_key: position.to_be_bytes().to_vec(),
        body: Some(serde_json::json!({"text":text})),
        scope_epoch: IngestionEpoch::new("epoch").unwrap(),
        required_environments: BTreeSet::new(),
        target_epochs: BTreeSet::new(),
    }
}

#[tokio::test]
async fn old_pages_and_bodies_do_not_change_when_a_late_row_moves() {
    let (_dir, store) = store().await;
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: vec![
                    row("a", 1, "first"),
                    row("b", 2, "original"),
                    row("c", 3, "last"),
                ],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let old = store.checkpoint().await.unwrap().snapshot;
    let first = store
        .read_page(&old, "history", None, None, None, 1, false)
        .await
        .unwrap();
    assert_eq!(first.rows[0].address, "a");
    store
        .apply(
            1,
            &old,
            &ProjectionBatch {
                through_cut: 2,
                read_rows: vec![row("b", 4, "corrected")],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let second = store
        .read_page(
            &old,
            "history",
            None,
            first.after.as_deref(),
            None,
            2,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| row.address.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "c"]
    );
    assert_eq!(second.rows[0].body["text"], "original");
    let now = store.checkpoint().await.unwrap().snapshot;
    let current = store
        .read_page(&now, "history", None, None, None, 3, false)
        .await
        .unwrap();
    assert_eq!(
        current
            .rows
            .iter()
            .map(|row| row.address.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "c", "b"]
    );
    assert_eq!(current.rows[2].body["text"], "corrected");
}

#[tokio::test]
async fn newest_window_can_page_back_without_reusing_a_cursor_for_another_scope() {
    let (_dir, store) = store().await;
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: (1..=6)
                    .map(|index| row(&index.to_string(), index, "body"))
                    .collect(),
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let newest = store
        .read_page(&snapshot, "history", None, None, None, 2, true)
        .await
        .unwrap();
    assert_eq!(
        newest
            .rows
            .iter()
            .map(|row| row.address.as_str())
            .collect::<Vec<_>>(),
        vec!["5", "6"]
    );
    assert!(newest.has_before);
    assert!(!newest.has_after);
    let previous = store
        .read_page(
            &snapshot,
            "history",
            newest.before.as_deref(),
            None,
            None,
            2,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        previous
            .rows
            .iter()
            .map(|row| row.address.as_str())
            .collect::<Vec<_>>(),
        vec!["3", "4"]
    );
    assert!(matches!(
        store
            .read_page(
                &snapshot,
                "other",
                newest.before.as_deref(),
                None,
                None,
                2,
                false
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
}

#[tokio::test]
async fn superseded_versions_exhaust_a_bounded_scan_with_a_resumable_nonempty_promise() {
    let (_dir, store) = store().await;
    for cut in 1..=20 {
        store
            .apply(
                1,
                &store.checkpoint().await.unwrap().snapshot,
                &ProjectionBatch {
                    through_cut: cut,
                    read_rows: vec![row("hot", 1, &format!("revision-{cut}"))],
                    ..ProjectionBatch::default()
                },
            )
            .await
            .unwrap();
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(&snapshot, "history", None, None, None, 2, false)
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    assert!(page.work_exhausted);
    assert!(page.has_after);
    assert!(page.after.is_some());
    assert_eq!(page.total, Some(1));
    let mut cursor = page.after;
    let mut found = Vec::new();
    for _ in 0..4 {
        let page = store
            .read_page(
                &snapshot,
                "history",
                None,
                cursor.as_deref(),
                None,
                2,
                false,
            )
            .await
            .unwrap();
        found.extend(page.rows);
        cursor = page.after;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].body["text"], "revision-20");
}

#[tokio::test]
async fn directory_filters_scan_beyond_the_first_window_and_bind_the_resume_scope() {
    let (_dir, store) = store().await;
    let rows = (1..=20)
        .map(|index| {
            let agent = AgentRef::Session(
                sylph_protocol::SessionId::decode(&format!("env:codex:child-{index}")).unwrap(),
            );
            let mut change = row(&index.to_string(), index, "");
            change.body = Some(
                serde_json::to_value(AgentDirectoryRow {
                    agent: AgentLink {
                        address: AgentAddress::of(&agent),
                        name: Some(if index == 20 {
                            "Needle 中文".into()
                        } else {
                            "Other".into()
                        }),
                        identity_suffix: index.to_string(),
                        vendor: "codex".into(),
                    },
                    relation: "spawn".into(),
                    own_execution: ExecutionSummary {
                        reference: None,
                        state: ReadExecutionState::ObservedActive {
                            last_observed_at_ms: None,
                        },
                        durations: vec![],
                    },
                    activations: ReadCount {
                        value: Some(1),
                        scope: "own activations".into(),
                        complete: true,
                    },
                    required_input: ReadCount {
                        value: None,
                        scope: "required input".into(),
                        complete: false,
                    },
                    coverage: vec![],
                })
                .unwrap(),
            );
            change
        })
        .collect();
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: rows,
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let filter = ReadFilter {
        text: "needle 中文".into(),
        state: "active".into(),
        ..ReadFilter::default()
    };
    let first = store
        .read_filtered_page(&snapshot, "history", None, None, None, 2, false, &filter)
        .await
        .unwrap();
    assert!(first.rows.is_empty());
    assert!(first.work_exhausted);
    assert!(first.has_after);
    assert!(matches!(
        store
            .read_filtered_page(
                &snapshot,
                "history",
                None,
                first.after.as_deref(),
                None,
                2,
                false,
                &ReadFilter {
                    text: "other".into(),
                    state: "active".into(),
                    ..ReadFilter::default()
                }
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
    let mut cursor = first.after;
    let mut found = Vec::new();
    for _ in 0..4 {
        let page = store
            .read_filtered_page(
                &snapshot,
                "history",
                None,
                cursor.as_deref(),
                None,
                2,
                false,
                &filter,
            )
            .await
            .unwrap();
        found.extend(page.rows);
        cursor = page.after;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].body["agent"]["name"], "Needle 中文");
    assert_eq!(found[0].address, "20");
}

fn visibility_filter(environments: &[&str]) -> ReadFilter {
    ReadFilter {
        allowed_environments: Some(environments.iter().map(|value| (*value).into()).collect()),
        ..ReadFilter::default()
    }
}

fn targeted_row(address: &str, position: u64, environment: &str) -> ReadRowChange {
    let mut change = row(address, position, address);
    change.required_environments.insert(environment.into());
    change
        .target_epochs
        .insert(IngestionEpoch::new(format!("epoch-{environment}")).unwrap());
    change
}

#[tokio::test]
async fn continuation_size_is_independent_of_grant_count_and_still_binds_every_grant() {
    let (_dir, store) = store().await;
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: vec![
                    targeted_row("first", 1, "public"),
                    targeted_row("last", 2, "public"),
                ],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let small = visibility_filter(&["public"]);
    let mut large = small.clone();
    large
        .allowed_environments
        .as_mut()
        .unwrap()
        .extend((0..5000).map(|index| format!("environment-{index:08}")));
    let small_page = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: None,
                around: None,
                limit: 1,
                latest: false,
            },
            &small,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    let large_page = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: None,
                around: None,
                limit: 1,
                latest: false,
            },
            &large,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    let small_cursor = small_page.after.unwrap();
    let large_cursor = large_page.after.unwrap();
    assert_eq!(small_cursor.len(), large_cursor.len());
    assert!(large_cursor.len() < store.limits().max_key_bytes);
    let next = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: Some(&large_cursor),
                around: None,
                limit: 1,
                latest: false,
            },
            &large,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert_eq!(next.rows[0].address, "last");
    assert!(matches!(
        store
            .read_authorized_page(
                &snapshot,
                "history",
                ReadWindow {
                    before: None,
                    after: Some(&large_cursor),
                    around: None,
                    limit: 1,
                    latest: false
                },
                &small,
                |_| async { Ok(true) }
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
    let partial = cursor_after(&snapshot, "history", "first", 0, &large).unwrap();
    let next = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: Some(&partial),
                around: None,
                limit: 1,
                latest: false,
            },
            &large,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert_eq!(next.rows[0].address, "last");
}

#[tokio::test]
async fn authorization_scans_use_covering_metadata_indexes_without_body_page_reads() {
    let (_dir, store) = store().await;
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let (mut conn, _) = store.read_connection(&snapshot).await.unwrap();
    for statement in [READ_FORWARD_SQL, READ_BACKWARD_SQL] {
        let plan = sqlx::query(&format!("EXPLAIN QUERY PLAN {statement}"))
            .bind("history")
            .bind(Vec::<u8>::new())
            .bind("")
            .bind(0i64)
            .bind(9i64)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert!(plan.iter().any(|row| {
            row.get::<String, _>("detail")
                .contains("USING COVERING INDEX read_window")
        }));
    }
    let plan = sqlx::query(&format!("EXPLAIN QUERY PLAN {READ_COUNT_SQL}"))
        .bind("history")
        .bind(9i64)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert!(plan.iter().any(|row| {
        row.get::<String, _>("detail")
            .contains("USING COVERING INDEX read_window")
    }));
    let plan = sqlx::query(&format!("EXPLAIN QUERY PLAN {READ_ANCHOR_SQL}"))
        .bind("history")
        .bind("anchor")
        .bind(1i64)
        .bind(1i64)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert!(plan.iter().any(|row| {
        row.get::<String, _>("detail")
            .contains("USING COVERING INDEX read_anchor")
    }));
}

#[tokio::test]
async fn projection_rejects_invalid_or_unbounded_authorization_metadata() {
    let (_dir, store) = store().await;
    let batch = |change| ProjectionBatch {
        through_cut: 1,
        read_rows: vec![change],
        ..ProjectionBatch::default()
    };
    assert!(matches!(
        store
            .apply(
                1,
                &store.checkpoint().await.unwrap().snapshot,
                &batch(targeted_row("invalid", 1, "NOT-an-environment"))
            )
            .await,
        Err(GenerationError::Invalid(_))
    ));
    let mut large = row("large", 1, "body");
    large
        .target_epochs
        .insert(IngestionEpoch::new("a".repeat(store.limits().max_key_bytes)).unwrap());
    assert!(matches!(
        store
            .apply(1, &store.checkpoint().await.unwrap().snapshot, &batch(large))
            .await,
        Err(GenerationError::Limit(
            "reading authorization metadata bytes"
        ))
    ));
    assert_eq!(store.checkpoint().await.unwrap().snapshot.view_revision, 0);
}

#[tokio::test]
async fn hidden_candidates_resume_within_the_scan_budget_without_exposing_a_total() {
    let (_dir, store) = store().await;
    let mut rows = (1..=16)
        .map(|index| targeted_row(&format!("hidden-{index}"), index, "private"))
        .collect::<Vec<_>>();
    rows.push(targeted_row("visible", 17, "public"));
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: rows,
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let filter = visibility_filter(&["public"]);
    let page = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: None,
                around: None,
                limit: 2,
                latest: false,
            },
            &filter,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    assert!(page.work_exhausted);
    assert!(page.has_after);
    assert_eq!(page.total, None);
    let first_cursor = page.after.unwrap();
    assert!(matches!(
        store
            .read_authorized_page(
                &snapshot,
                "history",
                ReadWindow {
                    before: None,
                    after: Some(&first_cursor),
                    around: None,
                    limit: 2,
                    latest: false
                },
                &visibility_filter(&["private", "public"]),
                |_| async { Ok(true) }
            )
            .await,
        Err(GenerationError::InvalidCursor)
    ));
    let mut cursor = Some(first_cursor);
    let mut found = Vec::new();
    for _ in 0..3 {
        let page = store
            .read_authorized_page(
                &snapshot,
                "history",
                ReadWindow {
                    before: None,
                    after: cursor.as_deref(),
                    around: None,
                    limit: 2,
                    latest: false,
                },
                &filter,
                |_| async { Ok(true) },
            )
            .await
            .unwrap();
        assert_eq!(page.total, None);
        assert!(!page.has_before || page.before.is_some());
        found.extend(page.rows);
        cursor = page.after;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address, "visible");
    assert!(cursor.is_none());
    assert_eq!(
        store
            .read_authorized_count(&snapshot, "history", &filter, |_| async { Ok(true) })
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn denied_environments_and_deleted_epochs_are_checked_before_body_hydration() {
    use sqlx::Connection;
    let (dir, store) = store().await;
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: vec![
                    targeted_row("private", 1, "private"),
                    targeted_row("deleted", 2, "deleted"),
                    targeted_row("visible", 3, "public"),
                ],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let mut conn = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("trace.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE read_rows SET body_json='invalid json', body_bytes=99999999 WHERE address IN ('private','deleted')").execute(&mut conn).await.unwrap();
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let filter = visibility_filter(&["public", "deleted"]);
    let authorize = |candidate: ReadCandidate| async move {
        assert!(!candidate.required_environments.contains("private"));
        assert_eq!(candidate.epoch.as_str(), "epoch");
        Ok(!candidate
            .target_epochs
            .contains(&IngestionEpoch::new("epoch-deleted").unwrap()))
    };
    let page = store
        .read_authorized_page(
            &snapshot,
            "history",
            ReadWindow {
                before: None,
                after: None,
                around: None,
                limit: 2,
                latest: false,
            },
            &filter,
            authorize,
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].address, "visible");
    assert_eq!(page.total, Some(1));
    assert_eq!(
        store
            .read_authorized_count(&snapshot, "history", &filter, authorize)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        store
            .read_authorized_count(&snapshot, "history", &visibility_filter(&[]), |_| async {
                panic!("denied environments must skip the epoch guard")
            })
            .await
            .unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn exact_visible_counts_require_every_target_grant_and_follow_snapshot_versions() {
    let (_dir, store) = store().await;
    let mut joint = targeted_row("joint", 1, "first");
    joint.required_environments.insert("second".into());
    joint
        .target_epochs
        .insert(IngestionEpoch::new("epoch-second").unwrap());
    store
        .apply(
            1,
            &store.checkpoint().await.unwrap().snapshot,
            &ProjectionBatch {
                through_cut: 1,
                read_rows: vec![joint.clone(), targeted_row("visible", 2, "first")],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let old = store.checkpoint().await.unwrap().snapshot;
    let first_only = visibility_filter(&["first"]);
    let both = visibility_filter(&["first", "second"]);
    assert_eq!(
        store
            .read_authorized_count(&old, "history", &first_only, |_| async { Ok(true) })
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        store
            .read_authorized_count(&old, "history", &both, |_| async { Ok(true) })
            .await
            .unwrap(),
        Some(2)
    );
    joint.body = None;
    store
        .apply(
            1,
            &old,
            &ProjectionBatch {
                through_cut: 2,
                read_rows: vec![joint],
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    let now = store.checkpoint().await.unwrap().snapshot;
    assert_eq!(
        store
            .read_authorized_count(&old, "history", &both, |_| async { Ok(true) })
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        store
            .read_authorized_count(&now, "history", &both, |_| async { Ok(true) })
            .await
            .unwrap(),
        Some(1)
    );
    let latest = store
        .read_authorized_page(
            &old,
            "history",
            ReadWindow {
                before: None,
                after: None,
                around: None,
                limit: 1,
                latest: true,
            },
            &both,
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert_eq!(latest.total, Some(2));
    let previous = store
        .read_authorized_page(
            &old,
            "history",
            ReadWindow {
                before: latest.before.as_deref(),
                after: None,
                around: None,
                limit: 1,
                latest: false,
            },
            &first_only,
            |_| async { Ok(true) },
        )
        .await;
    assert!(matches!(previous, Err(GenerationError::InvalidCursor)));
}
