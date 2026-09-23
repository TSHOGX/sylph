use super::*;
use crate::trace::reading_store::DirectoryMetadata;
fn agent(index: usize) -> AgentRef {
    AgentRef::Session(
        sylph_protocol::SessionId::decode(&format!("public:codex:node-{index:05}")).unwrap(),
    )
}
fn node(index: usize, end: usize, name: &str) -> ReadRowChange {
    let reference = agent(index);
    let mut row = targeted_row(&reference.encode(), index as u64 + 1, "public");
    row.scope = read_scope(&agent(0), None, DIRECTORY_COLLECTION);
    row.anchor = Some(TraceAddress::of(&AnyObjectRef::Agent(reference.clone())));
    row.directory = Some(DirectoryMetadata {
        component: agent(0),
        parent: (index > 0).then(|| agent(0)),
        end_key: (end as u64 + 1).to_be_bytes().to_vec(),
        spawn_epochs: if index > 0 {
            [IngestionEpoch::new("edge-epoch").unwrap()].into()
        } else {
            BTreeSet::new()
        },
    });
    row.body = Some(
        serde_json::to_value(AgentDirectoryRow {
            agent: AgentLink {
                address: AgentAddress::of(&reference),
                name: Some(name.into()),
                vendor: "codex".into(),
                identity_suffix: format!("{index:05}"),
            },
            relation: "spawn".into(),
            own_execution: ExecutionSummary {
                reference: None,
                state: ReadExecutionState::Unavailable,
                durations: vec![],
            },
            activations: ReadCount {
                value: None,
                scope: "own".into(),
                complete: false,
            },
            required_input: ReadCount {
                value: None,
                scope: "own".into(),
                complete: false,
            },
            coverage: vec![],
        })
        .unwrap(),
    );
    row
}
async fn publish(store: &GenerationStore, nodes: Vec<ReadRowChange>) -> Snapshot {
    let before = store.checkpoint().await.unwrap();
    store
        .apply(
            1,
            &before.snapshot,
            &ProjectionBatch {
                through_cut: before.snapshot.evidence_cut + 1,
                read_rows: nodes,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .snapshot
}
#[tokio::test]
async fn filtered_directory_counts_become_exact_after_bounded_prefixes_and_skip_denied_subtrees() {
    let (_dir, store) = store().await;
    let count = store.limits().max_candidates * 3;
    let mut nodes: Vec<_> = (0..count)
        .map(|index| {
            node(
                index,
                if index == 0 { count } else { index + 1 },
                if index % 3 == 0 { "Needle" } else { "Other" },
            )
        })
        .collect();
    // Deny a whole middle branch containing otherwise matching, visible records.
    nodes[3].directory.as_mut().unwrap().end_key = 10u64.to_be_bytes().to_vec();
    nodes[3].directory.as_mut().unwrap().spawn_epochs =
        [IngestionEpoch::new("denied-edge").unwrap()].into();
    let snapshot = publish(&store, nodes).await;
    let filter = ReadFilter {
        text: "needle".into(),
        ..Default::default()
    };
    let mut cursor = None;
    let mut found = vec![];
    let final_count;
    loop {
        let page = store
            .directory_page(
                &snapshot,
                &agent(0),
                cursor.as_deref(),
                2,
                &filter,
                |candidate| async move { Ok(candidate.epoch.as_str() != "denied-edge") },
            )
            .await
            .unwrap();
        assert!(page.examined <= store.limits().max_candidates);
        assert!(page.rows.len() <= 2);
        found.extend(page.rows.into_iter().map(|row| row.address));
        if !page.has_more {
            final_count = page.total;
            break;
        }
        cursor = page.after;
        assert!(cursor.is_some());
    }
    let expected: Vec<_> = (0..count)
        .filter(|index| index % 3 == 0 && !matches!(index, 3..=8))
        .map(|index| agent(index).encode())
        .collect();
    assert_eq!(found, expected);
    assert_eq!(final_count, Some(expected.len() as u64));
    let head = store
        .directory_page(&snapshot, &agent(0), None, 0, &filter, |_| async {
            Ok(true)
        })
        .await
        .unwrap();
    assert_eq!(
        head.total, None,
        "a bounded initial count must not report its partial prefix as a total"
    );
}

#[tokio::test]
async fn a_late_directory_page_has_constant_scan_work_when_the_catalogue_grows_tenfold() {
    let mut measurements = vec![];
    for count in [1000, 10000] {
        let (_dir, store) = store().await;
        let mut snapshot = store.checkpoint().await.unwrap().snapshot;
        for begin in (0..count).step_by(128) {
            snapshot = publish(
                &store,
                (begin..(begin + 128).min(count))
                    .map(|index| {
                        node(
                            index,
                            if index == 0 { count } else { index + 1 },
                            "Recorded agent",
                        )
                    })
                    .collect(),
            )
            .await;
        }
        let middle = count / 2;
        let filter = ReadFilter::default();
        // This store probe supplies a trusted already-scanned prefix. The HTTP
        // layer separately proves its private cursor/authority/visibility binding.
        let cursor = serde_json::to_string(&serde_json::json!({"snapshot":snapshot,"root":agent(0),"filter":filter.identity().unwrap(),"key":(middle as u64 + 1).to_be_bytes().to_vec(),"address":agent(middle).encode(),"version":snapshot.view_revision,"matched":middle+1})).unwrap();
        let page = store
            .directory_page(&snapshot, &agent(0), Some(&cursor), 4, &filter, |_| async {
                Ok(true)
            })
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 4);
        assert_eq!(page.rows[0].address, agent(middle + 1).encode());
        assert!(page.examined <= store.limits().max_candidates);
        measurements.push(page.vm_steps);
    }
    assert!(
        measurements[1] <= measurements[0] + 64,
        "late directory page scanned a growing prefix: {measurements:?}"
    );
    println!("directory_page_vm_steps {measurements:?}");
}

#[tokio::test]
async fn removed_directory_focus_and_parent_lookups_do_not_scan_superseded_versions() {
    let (_dir, store) = store().await;
    let first = publish(&store, vec![node(0, 2, "Root"), node(1, 2, "Child")]).await;
    for version in 0..320 {
        publish(
            &store,
            vec![node(1, 2, &format!("Child revision {version}"))],
        )
        .await;
    }
    let mut removed = node(1, 2, "Removed");
    removed.body = None;
    let mut independent = node(1, 2, "Independent");
    independent.scope = read_scope(&agent(1), None, DIRECTORY_COLLECTION);
    let metadata = independent.directory.as_mut().unwrap();
    metadata.component = agent(1);
    metadata.parent = None;
    metadata.spawn_epochs.clear();
    let moved = publish(&store, vec![removed, independent.clone()]).await;
    independent.body = None;
    let deleted = publish(&store, vec![independent]).await;
    let bounded = GenerationStore::open(
        store.path(),
        store.manifest().clone(),
        QueryLimits {
            max_vm_steps: 1024,
            ..store.limits().clone()
        },
    )
    .await
    .unwrap();
    let result = bounded
        .directory_page(
            &deleted,
            &agent(1),
            None,
            0,
            &ReadFilter::default(),
            |_| async { Ok(true) },
        )
        .await;
    assert!(
        matches!(result, Err(GenerationError::NotFound)),
        "removed focus must be unavailable within its query budget: {result:?}"
    );
    let result = bounded
        .directory_parent_candidate(&deleted, &agent(1), &agent(0))
        .await;
    assert!(
        matches!(result, Err(GenerationError::NotFound)),
        "removed parent link must be unavailable within its query budget: {result:?}"
    );
    let relocated = bounded
        .directory_page(
            &moved,
            &agent(1),
            None,
            1,
            &ReadFilter::default(),
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert_eq!(
        relocated.rows.len(),
        1,
        "a component move must select its live same-revision row"
    );
    assert_eq!(relocated.total, Some(1));
    assert!(matches!(
        bounded
            .directory_parent_candidate(&moved, &agent(1), &agent(0))
            .await,
        Err(GenerationError::NotFound)
    ));
    bounded
        .directory_parent_candidate(&first, &agent(1), &agent(0))
        .await
        .unwrap();
    let retained = bounded
        .directory_page(
            &first,
            &agent(1),
            None,
            1,
            &ReadFilter::default(),
            |_| async { Ok(true) },
        )
        .await
        .unwrap();
    assert_eq!(retained.rows[0].address, agent(1).encode());
}
