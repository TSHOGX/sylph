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
    }
}

#[tokio::test]
async fn old_pages_and_bodies_do_not_change_when_a_late_row_moves() {
    let (_dir, store) = store().await;
    store
        .apply(
            1,
            0,
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
            1,
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
            0,
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
                cut - 1,
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
    assert_eq!(page.total, 1);
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
            let agent = AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env:codex:child-{index}")).unwrap());
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
            0,
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
                    state: "active".into()
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
