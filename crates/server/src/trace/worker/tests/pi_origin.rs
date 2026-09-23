use super::*;
use crate::trace::reading_store::read_scope;

const PARENT: &[u8] = include_bytes!("../../../../../../fixtures/native/pi/fork-parent.jsonl");
const CHILD: &[u8] = include_bytes!("../../../../../../fixtures/native/pi/fork-child.jsonl");
const PARENT_ID: &str = "4405a3af-3d99-4855-936b-c6c3fc61126d";
const CHILD_ID: &str = "5e7e68e3-9fac-44b7-ae4a-af71bc1adbf5";
const LOCATOR: &str = "/Users/hsw/.pi/agent/sessions/4405a3af-3d99-4855-936b-c6c3fc61126d.jsonl";
fn agent(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:pi:{id}")).unwrap())
}
fn observation(source: &str) -> ObservationRef {
    ObservationRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        producer_id: source.into(),
        producer_epoch: "retained".into(),
        producer_seq: 1,
    }
}
fn source(name: &str) -> SourceRef {
    SourceRef {
        issuer: observation(name).issuer,
        instance: name.into(),
    }
}
async fn admit(
    fixture: &Fixture,
    name: &str,
    raw: &[u8],
    inventory: bool,
    epoch: &IngestionEpoch,
) -> Capture {
    let capture = Capture {
        observation_ref: observation(name),
        source_position: SourcePosition {
            source: source(name),
            generation: 0,
            locator: if inventory {
                SourceLocator::Frame { ordinal: 0 }
            } else {
                SourceLocator::Jsonl {
                    byte_start: 0,
                    byte_end: raw.len() as u64,
                    record_ordinal: 0,
                }
            },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "fixture.wall".into(),
            value: 100,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: if inventory {
            TransportKind::SourceInventory
        } else {
            TransportKind::NativeJsonl
        },
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: fixture.stage(raw).await,
        attachments: vec![],
        capture_diagnostics: vec![],
    };
    fixture
        .evidence
        .admit_batch(
            &capture.observation_ref.issuer,
            std::slice::from_ref(&capture),
        )
        .await
        .unwrap();
    capture
}
fn definition(parent: &str) -> Vec<u8> {
    serde_json::to_vec(&SourceDefinition {
        source: source(parent),
        generation: 0,
        locator: LOCATOR.into(),
        owner: agent(PARENT_ID),
        basis: SourceOwnerBasis::NativeIdentity {
            observation: observation(parent),
        },
    })
    .unwrap()
}
async fn header(store: &GenerationStore, owner: &AgentRef) -> AgentHeader {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let row = store
        .read_page(
            &snapshot,
            &read_scope(owner, None, "header"),
            None,
            None,
            None,
            1,
            false,
        )
        .await
        .unwrap();
    serde_json::from_value(row.rows[0].body.clone()).unwrap()
}
async fn reopen(store: GenerationStore) -> GenerationStore {
    let path = store.path().to_owned();
    let manifest = store.manifest().clone();
    let limits = store.limits().clone();
    store.close().await;
    GenerationStore::open(path, manifest, limits).await.unwrap()
}

#[tokio::test]
async fn pi_origin_uses_retained_headers_and_inventory_in_every_order_across_restart_and_rebuild() {
    let mut expected = None;
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let fixture = Fixture::for_vendor("pi").await;
        let (mut store, _) = fixture.generation("pi-origin").await;
        let certificate = definition("original-header");
        let records = [
            (
                "fork-header",
                CHILD.split_inclusive(|byte| *byte == b'\n').next().unwrap(),
                false,
            ),
            (
                "original-header",
                PARENT
                    .split_inclusive(|byte| *byte == b'\n')
                    .next()
                    .unwrap(),
                false,
            ),
            ("origin-inventory", certificate.as_slice(), true),
        ];
        for index in order {
            let (name, raw, inventory) = records[index];
            admit(&fixture, name, raw, inventory, &fixture.epoch).await;
            let progress = fixture
                .worker()
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            assert_eq!(progress.suspended_sources, 0);
            store = reopen(store).await;
        }
        let value = header(&store, &agent(CHILD_ID)).await;
        assert_eq!(value.origin.unwrap().address.agent(), agent(PARENT_ID));
        assert!(value.parent.is_none());
        assert_eq!(value.descendant_count.value, Some(0));
        let before = claims(&store).await;
        let snapshot = reader_projection(&fixture, &store).await;
        let cut = fixture.evidence.evidence_cut().await.unwrap();
        let (rebuilt, _) = fixture.generation("pi-origin-rebuilt").await;
        fixture.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
        assert_eq!(claims(&rebuilt).await, before);
        assert_eq!(reader_projection(&fixture, &rebuilt).await, snapshot);
        let normalized = snapshot
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await;
        if let Some((claims, reader)) = &expected {
            assert_eq!(&before, claims);
            assert_eq!(&normalized, reader);
        } else {
            expected = Some((before, normalized));
        }
    }
}

#[tokio::test]
async fn pi_origin_keeps_an_independent_certificate_after_one_retention_scope_is_deleted() {
    let fixture = Fixture::for_vendor("pi").await;
    admit(
        &fixture,
        "fork-header",
        CHILD.split_inclusive(|byte| *byte == b'\n').next().unwrap(),
        false,
        &fixture.epoch,
    )
    .await;
    let mut scopes = vec![];
    for name in ["proofone", "prooftwo"] {
        let scope = IngestionScope::Inventory(InventoryRef {
            environment: EnvironmentId::parse("env7fk2").unwrap(),
            vendor: AgentName::parse("pi").unwrap(),
            instance: name.into(),
        });
        let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
        let source_name = format!("{name}-header");
        admit(
            &fixture,
            &source_name,
            PARENT
                .split_inclusive(|byte| *byte == b'\n')
                .next()
                .unwrap(),
            false,
            &epoch,
        )
        .await;
        admit(
            &fixture,
            &format!("{name}-inventory"),
            &definition(&source_name),
            true,
            &epoch,
        )
        .await;
        scopes.push(scope);
    }
    for (index, deleted) in [None, Some(&scopes[0]), Some(&scopes[1])]
        .into_iter()
        .enumerate()
    {
        if let Some(scope) = deleted {
            fixture.evidence.delete_scope(scope).await.unwrap();
        }
        let (store, _) = fixture
            .generation(&format!("retained-origin-{index}"))
            .await;
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        let value = header(&store, &agent(CHILD_ID)).await;
        if index < 2 {
            assert_eq!(value.origin.unwrap().address.agent(), agent(PARENT_ID));
        } else {
            assert!(
                value.origin.is_none(),
                "deleted source certificates must leave lineage unresolved"
            );
        }
        assert!(value.parent.is_none());
    }
}

#[tokio::test]
async fn a_late_pi_inventory_certificate_resumes_bounded_origin_updates_after_restart() {
    use mapping::owner_resolution::OWNER_PAGE_WORK;
    let fixture = Fixture::for_vendor("pi").await;
    let count = OWNER_PAGE_WORK * 2;
    for index in 0..count {
        let raw = serde_json::to_vec(&serde_json::json!({"type":"session","id":format!("fork-{index:04}"),"parentSession":LOCATOR})).unwrap();
        admit(
            &fixture,
            &format!("fork-{index:04}"),
            &raw,
            false,
            &fixture.epoch,
        )
        .await;
    }
    admit(
        &fixture,
        "original-header",
        PARENT
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap(),
        false,
        &fixture.epoch,
    )
    .await;
    let (store, _) = fixture.generation("many-pi-origins").await;
    let worker = fixture.worker();
    let before = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&store, 1, before).await.unwrap();
    admit(
        &fixture,
        "late-inventory",
        &definition("original-header"),
        true,
        &fixture.epoch,
    )
    .await;
    let checkpoint = store.checkpoint().await.unwrap();
    let (state, _) = worker
        .load_state(&store, &checkpoint.snapshot)
        .await
        .unwrap();
    let captures = fixture
        .evidence
        .admissions_after(before, 1)
        .await
        .unwrap()
        .captures;
    let unit = worker
        .prepare_unit(&store, &state, &captures, before + 1)
        .await
        .unwrap();
    assert!(unit.next.pending.values().any(|pending| {
        pending
            .requests
            .contains(&ArchiveRequest::ContinueOwnerResolution)
    }));
    assert!(
        unit.batch
            .claims
            .iter()
            .filter(|claim| !claim.supersedes.is_empty())
            .count()
            <= OWNER_PAGE_WORK
    );
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    let store = reopen(store).await;
    let progress = fixture
        .worker()
        .catch_up(&store, 1, before + 1)
        .await
        .unwrap();
    assert_eq!(progress.pending_mappings, 0);
    assert_eq!(progress.suspended_sources, 0);
    for index in 0..count {
        let value = header(&store, &agent(&format!("fork-{index:04}"))).await;
        assert_eq!(value.origin.unwrap().address.agent(), agent(PARENT_ID));
    }
}

#[tokio::test]
async fn contradictory_pi_locator_certificates_preserve_old_views_without_choosing_a_current_origin()
 {
    let fixture = Fixture::for_vendor("pi").await;
    let (store, _) = fixture.generation("conflicting-pi-path").await;
    admit(
        &fixture,
        "fork-header",
        CHILD.split_inclusive(|byte| *byte == b'\n').next().unwrap(),
        false,
        &fixture.epoch,
    )
    .await;
    admit(
        &fixture,
        "original-header",
        PARENT
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap(),
        false,
        &fixture.epoch,
    )
    .await;
    admit(
        &fixture,
        "origin-inventory",
        &definition("original-header"),
        true,
        &fixture.epoch,
    )
    .await;
    fixture
        .worker()
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let old = store.checkpoint().await.unwrap().snapshot;
    assert_eq!(
        header(&store, &agent(CHILD_ID))
            .await
            .origin
            .unwrap()
            .address
            .agent(),
        agent(PARENT_ID)
    );
    let other = "different-native-identity";
    admit(
        &fixture,
        "other-header",
        &serde_json::to_vec(&serde_json::json!({"type":"session","id":other})).unwrap(),
        false,
        &fixture.epoch,
    )
    .await;
    let mut certificate: SourceDefinition =
        serde_json::from_slice(&definition("other-header")).unwrap();
    certificate.owner = agent(other);
    admit(
        &fixture,
        "other-inventory",
        &serde_json::to_vec(&certificate).unwrap(),
        true,
        &fixture.epoch,
    )
    .await;
    let progress = fixture
        .worker()
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    assert_eq!(progress.suspended_sources, 0);
    assert!(header(&store, &agent(CHILD_ID)).await.origin.is_none());
    let retained = store
        .read_page(
            &old,
            &read_scope(&agent(CHILD_ID), None, "header"),
            None,
            None,
            None,
            1,
            false,
        )
        .await
        .unwrap();
    let retained: AgentHeader = serde_json::from_value(retained.rows[0].body.clone()).unwrap();
    assert_eq!(retained.origin.unwrap().address.agent(), agent(PARENT_ID));
    let mut reduction = Reducer::default();
    let claims = claims(&store).await;
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    reduction.apply(&claims, &evidence, claims.len()).unwrap();
    let origins: BTreeSet<_> = reduction
        .resolve()
        .relations
        .into_iter()
        .filter_map(|relation| match relation.relation {
            Relation::ContextLineage {
                recipient,
                original_owner: OwnerRef::Agent { agent: origin },
                ..
            } if recipient == agent(CHILD_ID) => Some(origin),
            _ => None,
        })
        .collect();
    assert_eq!(origins, [agent(PARENT_ID), agent(other)].into());
}
