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
        declared_subject_refs: if row.source == "parent" {
            vec![owner(PARENT_ID)]
        } else {
            vec![]
        },
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
        let copied = unit.anchor.object();
        let witnesses: Vec<_> = reduced
            .relations
            .iter()
            .filter_map(|relation| match &relation.relation {
                Relation::ContextCopy {
                    original,
                    copied: entry,
                    original_position,
                    copied_position,
                } if copied == AnyObjectRef::HistoryEntry(entry.clone()) => {
                    Some((original, original_position, copied_position))
                }
                _ => None,
            })
            .collect();
        if !witnesses.is_empty() {
            let Some(ReadEntryContext::Inherited {
                original_owner: Some(origin),
            }) = &unit.entry_context
            else {
                panic!("this copied entry must retain its own exact provenance: {copied:?}");
            };
            assert_eq!(origin.address.agent(), owner(PARENT_ID));
            let AnyObjectRef::HistoryEntry(entry) = &copied else {
                unreachable!()
            };
            for (original, original_position, copied_position) in witnesses {
                assert!(
                    reduced.history_contents[original]
                        .values()
                        .any(|content| &content.position == original_position)
                );
                assert!(
                    reduced.history_contents[entry]
                        .values()
                        .any(|content| &content.position == copied_position)
                );
            }
        } else {
            assert_eq!(unit.entry_context, Some(ReadEntryContext::Unresolved));
        }
        assert!(unit.activation.is_none());
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

async fn inventory(fixture: &Fixture, parent: &Capture) -> Capture {
    let header: serde_json::Value = serde_json::from_slice(
        FORK_CHILD
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap(),
    )
    .unwrap();
    let definition = SourceDefinition {
        source: parent.source_position.source.clone(),
        generation: 0,
        locator: header["parent_session"].as_str().unwrap().into(),
        owner: owner(PARENT_ID),
        basis: SourceOwnerBasis::NativeIdentity {
            observation: parent.observation_ref.clone(),
        },
    };
    let mut value = parent.clone();
    value.observation_ref.producer_id = "inventory".into();
    value.source_position.source.instance = "inventory".into();
    value.source_position.locator = SourceLocator::Frame { ordinal: 0 };
    value.transport_kind = TransportKind::SourceInventory;
    value.payload_ref = fixture
        .stage(&serde_json::to_vec(&definition).unwrap())
        .await;
    fixture
        .evidence
        .admit_batch(&value.observation_ref.issuer, std::slice::from_ref(&value))
        .await
        .unwrap();
    value
}

async fn copy_links(store: &GenerationStore) -> Vec<(EntryRef, EntryRef)> {
    reduction(&claims(store).await)
        .relations
        .into_iter()
        .filter_map(|value| match value.relation {
            Relation::ContextCopy {
                original,
                copied,
                original_position,
                copied_position,
            } => {
                assert_eq!(original.scope(), &owner(PARENT_ID));
                assert_eq!(copied.scope(), &owner(CHILD_ID));
                assert_eq!(original_position.source.instance, "parent");
                assert_eq!(copied_position.source.instance, "child");
                assert_ne!(original_position, copied_position);
                Some((original, copied))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn retained_pi_fork_copies_stay_readable_without_new_admissions_through_restart_reread_and_origin_deletion(
) {
    let mut expected = None;
    for (child_first, early_inventory) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
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
            if early_inventory && row.source == "parent" && row.ordinal == 0 {
                captures.push(inventory(&fixture, captures.last().unwrap()).await);
            }
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
        if !early_inventory {
            let parent = captures
                .iter()
                .find(|capture| {
                    capture.observation_ref.producer_id == "parent"
                        && capture.observation_ref.producer_seq == 1
                })
                .unwrap();
            captures.push(inventory(&fixture, parent).await);
            fixture
                .worker()
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
        }
        assert_eq!(copy_links(&store).await.len(), 4);
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
        let (same_cut, _) = fixture.generation("pi-copies-same-cut").await;
        fixture.worker().catch_up(&same_cut, 1, cut).await.unwrap();
        assert_eq!(copy_links(&same_cut).await.len(), 4);
        child_is_readable_without_admission(&fixture, &same_cut, &parent_epoch).await;
        fixture.evidence.delete_scope(&parent_scope).await.unwrap();
        child_is_readable_without_admission(&fixture, &store, &parent_epoch).await;
        let (rebuilt, _) = fixture.generation("pi-copy-after-deletion").await;
        fixture.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
        child_is_readable_without_admission(&fixture, &rebuilt, &parent_epoch).await;
        assert!(copy_links(&rebuilt).await.is_empty());
    }
}

#[tokio::test]
async fn a_copied_predecessor_does_not_classify_an_unmatched_retained_pi_successor_as_local() {
    for child_first in [false, true] {
        let fixture = Fixture::for_vendor("pi").await;
        let parent_scope = IngestionScope::Agent(owner(PARENT_ID));
        let parent_epoch = fixture
            .evidence
            .authorize_scope(&parent_scope, &[owner(PARENT_ID)])
            .await
            .unwrap();
        let parent = native_rows("parent", FORK_PARENT);
        let child = native_rows("child", FORK_CHILD);
        // Retain the child exactly, but delay the parent's corresponding final
        // assistant record. Both copied and genuinely new entries can follow
        // the same copied predecessor; the edge cannot distinguish them.
        let early: Vec<_> = if child_first {
            child.iter().chain(parent[..4].iter()).collect()
        } else {
            parent[..4].iter().chain(child.iter()).collect()
        };
        let mut parent_header = None;
        for row in early {
            let capture = admit(
                &fixture,
                row,
                if row.source == "parent" {
                    &parent_epoch
                } else {
                    &fixture.epoch
                },
            )
            .await;
            if row.source == "parent" && row.ordinal == 0 {
                parent_header = Some(capture);
            }
        }
        inventory(&fixture, &parent_header.unwrap()).await;
        let cut = fixture.evidence.evidence_cut().await.unwrap();
        let (store, _) = fixture.generation("pi-partial-copy").await;
        fixture.worker().catch_up(&store, 1, cut).await.unwrap();
        let snapshot = store.checkpoint().await.unwrap().snapshot;
        let history = read_scope(&owner(CHILD_ID), None, "history");
        let page = store
            .read_page(&snapshot, &history, None, None, None, 16, false)
            .await
            .unwrap();
        assert_eq!(copy_links(&store).await.len(), 3);
        let row = page
            .rows
            .iter()
            .find(|row| row.body.to_string().contains("hello"))
            .unwrap();
        let unit: ReadingUnit = serde_json::from_value(row.body.clone()).unwrap();
        assert_eq!(
            unit.entry_context,
            Some(ReadEntryContext::Unresolved),
            "a missing parent counterpart cannot establish child-local production"
        );
        assert!(matches!(
            unit.content,
            ReadingContent::Message { delivery: None, .. }
        ));
        assert!(!row.dependency_epochs.contains(&parent_epoch));
        let store = reopened(store).await;
        let (rebuilt, _) = fixture.generation("pi-partial-copy-rebuilt").await;
        fixture.worker().catch_up(&rebuilt, 1, cut).await.unwrap();
        assert_eq!(
            projection(&fixture, &store).await,
            projection(&fixture, &rebuilt).await
        );

        admit(&fixture, &parent[4], &parent_epoch).await;
        let later_cut = fixture.evidence.evidence_cut().await.unwrap();
        fixture
            .worker()
            .catch_up(&store, 1, later_cut)
            .await
            .unwrap();
        let later = store.checkpoint().await.unwrap().snapshot;
        let page = store
            .read_page(&later, &history, None, None, None, 16, false)
            .await
            .unwrap();
        let row = page
            .rows
            .iter()
            .find(|row| row.body.to_string().contains("hello"))
            .unwrap();
        let unit: ReadingUnit = serde_json::from_value(row.body.clone()).unwrap();
        assert!(
            matches!(unit.entry_context, Some(ReadEntryContext::Inherited { original_owner: Some(origin) }) if origin.address.agent() == owner(PARENT_ID))
        );
        assert!(matches!(
            unit.content,
            ReadingContent::Message { delivery: None, .. }
        ));
        let old = store
            .read_page(&snapshot, &history, None, None, None, 16, false)
            .await
            .unwrap();
        let old = old
            .rows
            .iter()
            .find(|row| row.body.to_string().contains("hello"))
            .unwrap();
        assert_eq!(
            old.body["entryContext"]["kind"], "unresolved",
            "a later copy proof cannot rewrite the held earlier history"
        );
        let (rebuilt, _) = fixture.generation("pi-complete-copy-rebuilt").await;
        fixture
            .worker()
            .catch_up(&rebuilt, 1, later_cut)
            .await
            .unwrap();
        assert_eq!(
            projection(&fixture, &store).await,
            projection(&fixture, &rebuilt).await
        );
    }
}

#[tokio::test]
async fn pi_copy_witness_indexes_and_continuations_resume_from_committed_state() {
    use mapping::pi::copies::COPY_PAGE_WORK;
    let fixture = Fixture::for_vendor("pi").await;
    let parent_scope = IngestionScope::Agent(owner(PARENT_ID));
    let parent_epoch = fixture
        .evidence
        .authorize_scope(&parent_scope, &[owner(PARENT_ID)])
        .await
        .unwrap();
    let parent_header = admit(
        &fixture,
        &native_rows("parent", FORK_PARENT)[0],
        &parent_epoch,
    )
    .await;
    let child_header = admit(
        &fixture,
        &native_rows("child", FORK_CHILD)[0],
        &fixture.epoch,
    )
    .await;
    let count = COPY_PAGE_WORK * 2;
    for template in [&parent_header, &child_header] {
        let SourceLocator::Jsonl {
            byte_end: mut offset,
            ..
        } = template.source_position.locator
        else {
            unreachable!()
        };
        for index in 0..count {
            let raw=serde_json::to_vec(&serde_json::json!({"type":"message","id":format!("entry-{index:04}"),"parentId":if index==0 {None}else{Some(format!("entry-{:04}",index-1))},"message":{"role":"assistant","content":[{"type":"text","text":format!("Matched record {index}")}]}})).unwrap();
            let mut capture = template.clone();
            capture.observation_ref.producer_seq = index as u64 + 2;
            capture.source_position.locator = SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + raw.len() as u64,
                record_ordinal: index as u64 + 1,
            };
            offset += raw.len() as u64;
            capture.payload_ref = fixture.stage(&raw).await;
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
        }
    }
    let (store, _) = fixture.generation("pi-copy-continuation").await;
    let worker = fixture.worker();
    let before = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&store, 1, before).await.unwrap();
    inventory(&fixture, &parent_header).await;
    let checkpoint = store.checkpoint().await.unwrap();
    let (state, _) = worker
        .load_state(&store, &checkpoint.snapshot)
        .await
        .unwrap();
    assert!(state.pi_copies.shares_roots(&state.clone().pi_copies));
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
    assert!(unit
        .next
        .pending
        .values()
        .any(|pending| pending.requests.contains(&ArchiveRequest::ContinueCopies)));
    assert!(
        unit.batch
            .claims
            .iter()
            .filter(|claim| matches!(
                claim.fact,
                Fact::Relation {
                    relation: Relation::ContextCopy { .. }
                }
            ))
            .count()
            <= COPY_PAGE_WORK
    );
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    let store = reopened(store).await;
    let progress = fixture
        .worker()
        .catch_up(&store, 1, before + 1)
        .await
        .unwrap();
    assert_eq!(progress.pending_mappings, 0);
    assert_eq!(progress.suspended_sources, 0);
    assert_eq!(copy_links(&store).await.len(), count);
    let (rebuilt, _) = fixture.generation("pi-copy-continuation-rebuilt").await;
    fixture
        .worker()
        .catch_up(&rebuilt, 1, before + 1)
        .await
        .unwrap();
    assert_eq!(
        projection(&fixture, &store).await,
        projection(&fixture, &rebuilt).await
    );
}
