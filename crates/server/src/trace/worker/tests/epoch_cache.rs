use super::*;
use crate::trace::reading_store::read_scope;

async fn assign_old_entry_epoch(
    fixture: &Fixture,
    epoch: &IngestionEpoch,
    restart: bool,
) -> ReaderProjection {
    let name = if restart {
        "epoch-restarted"
    } else {
        "epoch-warm"
    };
    let (mut generation, _) = fixture.generation(name).await;
    let mut worker = fixture.worker();
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&generation, 1, cut).await.unwrap();
    let original = reader_projection(fixture, &generation).await;
    let history = read_scope(&agent(), None, "history");
    let (key, row) = original
        .rows
        .iter()
        .find(|((scope, _), _)| scope == &history)
        .unwrap();
    let reference = TraceAddress::parse(row.anchor.clone().unwrap())
        .unwrap()
        .object();
    assert!(matches!(reference, AnyObjectRef::HistoryEntry(_)));
    assert_eq!(row.source_epoch, OracleEpoch::Exact(fixture.epoch.clone()));
    let checkpoint_key = format!("read_epoch:{}", reference.encode());
    let held = worker.states.lock().await.get(name).unwrap().clone();
    let mut committed = held.lock().await.clone();
    assert!(committed.reading.is_some());

    // The stored row already uses the governing agent epoch. A missing per-entry
    // checkpoint binding has the same fallback, so clearing it changes no served row.
    let checkpoint = generation.checkpoint().await.unwrap();
    let unbound = generation
        .apply(
            checkpoint.fence_token,
            &checkpoint.snapshot,
            &ProjectionBatch {
                through_cut: cut,
                continuation: true,
                mapper_states: BTreeMap::from([(checkpoint_key.clone(), serde_json::Value::Null)]),
                ..ProjectionBatch::default()
            },
        )
        .await
        .unwrap();
    committed.snapshot = Some(unbound.snapshot.clone());
    assert_eq!(
        committed.object_epochs.remove(&reference),
        Some(fixture.epoch.clone())
    );
    assert_eq!(reader_projection(fixture, &generation).await, original);

    if restart {
        let path = generation.path().to_path_buf();
        let manifest = generation.manifest().clone();
        let limits = generation.limits().clone();
        generation.close().await;
        generation = GenerationStore::open(path, manifest, limits).await.unwrap();
        worker = fixture.worker();
        worker.full_reader_rebuilds = true;
        committed = worker
            .load_state(&generation, &unbound.snapshot)
            .await
            .unwrap()
            .0;
        assert!(committed.reading.is_none());
        assert!(!committed.object_epochs.contains_key(&reference));
    }

    let mut unit = ProjectionUnit::new(&committed, cut);
    unit.batch.continuation = true;
    unit.next
        .object_epochs
        .insert(reference.clone(), epoch.clone());
    unit.batch
        .mapper_states
        .insert(checkpoint_key, serde_json::to_value(epoch).unwrap());
    unit.scopes.push((epoch.clone(), cut));
    assert!(Arc::ptr_eq(&committed.reduction, &unit.next.reduction));
    assert!(unit.reduction_changes.is_empty());
    worker
        .materialize(&generation, &committed, &mut unit)
        .await
        .unwrap();
    assert_eq!(unit.membership_work, MembershipWork::default(),
        "An epoch-only row update cannot change any membership collection");
    worker.commit_effects(&generation, &unit).await.unwrap();
    let published = generation
        .apply(unbound.fence_token, &unbound.snapshot, &unit.batch)
        .await
        .unwrap();
    assert_eq!(
        published.snapshot.evidence_cut,
        unbound.snapshot.evidence_cut
    );
    assert!(published.snapshot.view_revision > unbound.snapshot.view_revision);

    let actual = reader_projection(fixture, &generation).await;
    let changed = &actual.rows[key];
    assert_eq!(changed.body, row.body);
    assert_eq!(changed.order_key, row.order_key);
    assert_eq!(changed.source_epoch, OracleEpoch::Exact(epoch.clone()),
        "{name}: a reconstructed baseline must not absorb the candidate epoch before row comparison");
    assert_eq!(
        changed.target_epochs,
        BTreeSet::from([OracleEpoch::Exact(epoch.clone())])
    );
    let old_row = generation
        .read_row(&unbound.snapshot, &key.0, &key.1)
        .await
        .unwrap();
    assert_eq!(old_row.epoch, fixture.epoch);
    assert_eq!(old_row.body, row.body);
    assert_eq!(
        generation.mapper_states(&published.snapshot).await.unwrap()
            [&format!("read_epoch:{}", reference.encode())],
        serde_json::to_value(epoch).unwrap(),
    );
    generation.close().await;
    actual
}

#[tokio::test]
async fn restored_reader_baseline_preserves_old_epoch_until_new_binding_is_committed() {
    let fixture = Fixture::new().await;
    fixture.native_fixture().await;
    let epoch = fixture
        .evidence
        .authorize_scope(
            &IngestionScope::Inventory(InventoryRef {
                environment: agent().environment().clone(),
                vendor: AgentName::parse("codex").unwrap(),
                instance: "late-entry-binding".into(),
            }),
            &[agent()],
        )
        .await
        .unwrap();
    assert_ne!(epoch, fixture.epoch);
    let warm = assign_old_entry_epoch(&fixture, &epoch, false).await;
    let restored = assign_old_entry_epoch(&fixture, &epoch, true).await;
    assert_eq!(restored, warm,
        "Restarted full materialization must match warm rows, order, memberships, payloads and exact authorization epochs");
}
