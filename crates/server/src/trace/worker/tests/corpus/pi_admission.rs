use super::*;

const FORK_PARENT: &[u8] =
    include_bytes!("../../../../../../../fixtures/native/pi/fork-parent.jsonl");
const FORK_CHILD: &[u8] =
    include_bytes!("../../../../../../../fixtures/native/pi/fork-child.jsonl");
const PARENT_ID: &str = "4405a3af-3d99-4855-936b-c6c3fc61126d";
const CHILD_ID: &str = "5e7e68e3-9fac-44b7-ae4a-af71bc1adbf5";
fn owner(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:pi:{id}")).unwrap())
}
struct NativeRow {
    source: &'static str,
    ordinal: u64,
    offset: u64,
    raw: &'static [u8],
}
fn native_rows(source: &'static str, raw: &'static [u8]) -> Vec<NativeRow> {
    let mut offset = 0;
    raw.split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .map(|(index, raw)| {
            let row = NativeRow {
                source,
                ordinal: index as u64,
                offset,
                raw,
            };
            offset += raw.len() as u64;
            row
        })
        .collect()
}
async fn admit(fixture: &Fixture, row: &NativeRow, epoch: &IngestionEpoch) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: row.source.into(),
            producer_epoch: "retained".into(),
            producer_seq: row.ordinal + 1,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer: issuer.clone(),
                instance: row.source.into(),
            },
            generation: 0,
            locator: SourceLocator::Jsonl {
                byte_start: row.offset,
                byte_end: row.offset + row.raw.len() as u64,
                record_ordinal: row.ordinal,
            },
        },
        ingestion_authorization_epoch: epoch.clone(),
        declared_subject_refs: if row.source == "parent" { vec![owner(PARENT_ID)] } else { vec![] },
        captured_at: TimeEvidence {
            domain: "fixture.wall".into(),
            value: 100,
            unit: TimeUnit::Milliseconds,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: TransportKind::NativeJsonl,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: fixture.stage(row.raw).await,
        attachments: vec![],
        capture_diagnostics: vec![],
    };
    fixture
        .evidence
        .admit_batch(&issuer, std::slice::from_ref(&capture))
        .await
        .unwrap();
    capture
}
async fn child_is_readable_without_admission(
    fixture: &Fixture,
    store: &GenerationStore,
    parent_epoch: &IngestionEpoch,
) {
    let reduced = reduction(&claims(store).await);
    assert!(!reduced.objects.values().any(|object| matches!(object.declaration.known(),Some(TraceObject::Delivery(delivery)) if delivery.delivery_ref.scope() == &owner(CHILD_ID))));
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let rows = store
        .read_page(
            &snapshot,
            &read_scope(&owner(CHILD_ID), None, "history"),
            None,
            None,
            None,
            16,
            false,
        )
        .await
        .unwrap();
    let mut text = vec![];
    for row in rows.rows {
        assert_ne!(&row.epoch, parent_epoch);
        assert!(!row.dependency_epochs.contains(parent_epoch));
        let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
        if let ReadingContent::Message {
            parts, delivery, ..
        } = unit.content
        {
            assert!(delivery.is_none());
            for part in parts {
                assert_eq!(
                    fixture
                        .evidence
                        .payload_bytes(&part.payload.payload_ref, 4096)
                        .await
                        .unwrap(),
                    part.preview.as_bytes()
                );
                text.push(part.preview);
            }
        }
    }
    assert_eq!(text, ["Say one", "hello"]);
}

#[tokio::test]
async fn retained_pi_fork_copies_stay_readable_without_new_admissions_through_restart_reread_and_origin_deletion()
 {
    let mut expected = None;
    for child_first in [false, true] {
        let fixture = Fixture::for_vendor("pi").await;
        let parent_scope = IngestionScope::Agent(owner(PARENT_ID));
        let parent_epoch = fixture
            .evidence
            .authorize_scope(&parent_scope, &[owner(PARENT_ID)])
            .await
            .unwrap();
        let (mut store, _) = fixture.generation("pi-copy-admission").await;
        let parent = native_rows("parent", FORK_PARENT);
        let child = native_rows("child", FORK_CHILD);
        let records: Vec<_> = if child_first {
            child.into_iter().chain(parent).collect()
        } else {
            parent.into_iter().chain(child).collect()
        };
        let mut captures = vec![];
        for row in &records {
            captures.push(
                admit(
                    &fixture,
                    row,
                    if row.source == "parent" {
                        &parent_epoch
                    } else {
                        &fixture.epoch
                    },
                )
                .await,
            );
            let progress = fixture
                .worker()
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            assert_eq!(
                progress.pending_mappings, 0,
                "unproved membership must not wait on an unproducible correlation"
            );
            assert_eq!(progress.suspended_sources, 0);
            store = reopened(store).await;
        }
        child_is_readable_without_admission(&fixture, &store, &parent_epoch).await;
        let reduced = reduction(&claims(&store).await);
        assert_eq!(reduced.objects.values().filter(|object| matches!(object.declaration.known(),Some(TraceObject::Delivery(delivery)) if delivery.delivery_ref.scope()==&owner(PARENT_ID))).count(),3,"the original local inputs still establish admission");
        let before = projection(&fixture, &store).await;
        for mut capture in captures {
            capture.observation_ref.producer_id =
                format!("reread-{}", capture.observation_ref.producer_id);
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
        }
        let cut = fixture.evidence.evidence_cut().await.unwrap();
        fixture.worker().catch_up(&store, 1, cut).await.unwrap();
        assert_eq!(projection(&fixture, &store).await, before);
        let normalized = Projection {
            claims: before.claims,
            reader: before
                .reader
                .normalize_separate_archive_epochs(&fixture.catalogue)
                .await,
        };
        if let Some(expected) = &expected {
            assert_eq!(&normalized, expected);
        } else {
            expected = Some(normalized);
        }
        fixture.evidence.delete_scope(&parent_scope).await.unwrap();
        child_is_readable_without_admission(&fixture, &store, &parent_epoch).await;
        let (rebuilt, _) = fixture.generation("pi-copy-after-deletion").await;
        fixture.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
        child_is_readable_without_admission(&fixture, &rebuilt, &parent_epoch).await;
    }
}
