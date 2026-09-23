//! Independent native headers may corroborate one route without replacing its proof.
use super::*;

async fn route_records(
    fixture: &Fixture,
    order: [usize; 4],
    store: &GenerationStore,
    conflicting: bool,
    local_path: bool,
) {
    let header = b"{\"type\":\"session_meta\",\"ordinal\":0,\"payload\":{\"id\":\"observer\",\"session_id\":\"root\",\"agent_path\":\"/root/observer\"}}\n";
    let message = b"{\"type\":\"response_item\",\"ordinal\":1,\"payload\":{\"type\":\"agent_message\",\"id\":\"corroborated\",\"author\":\"/root/sender\",\"recipient\":\"/root\",\"content\":[{\"type\":\"input_text\",\"text\":\"independently proved sender\"}]}}\n";
    let peer = b"{\"type\":\"session_meta\",\"ordinal\":0,\"payload\":{\"id\":\"sender\",\"session_id\":\"root\",\"agent_path\":\"/root/sender\"}}\n";
    let other = if conflicting {
        String::from_utf8(peer.to_vec())
            .unwrap()
            .replace("\"id\":\"sender\"", "\"id\":\"other-sender\"")
            .into_bytes()
    } else {
        peer.to_vec()
    };
    let header = if local_path {
        String::from_utf8(header.to_vec())
            .unwrap()
            .replace("/root/observer", "/root/sender")
            .into_bytes()
    } else {
        header.to_vec()
    };
    let records = [
        ("observer", 1, 0, header.as_slice()),
        ("observer", 2, header.len() as u64, message.as_slice()),
        ("sender-proof-a", 1, 0, peer.as_slice()),
        ("sender-proof-b", 1, 0, other.as_slice()),
    ];
    for index in order {
        let (source, sequence, offset, bytes) = records[index];
        let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
        fixture
            .admit(
                ObservationRef {
                    issuer: issuer.clone(),
                    producer_id: source.into(),
                    producer_epoch: "retained".into(),
                    producer_seq: sequence,
                },
                SourcePosition {
                    source: SourceRef {
                        issuer,
                        instance: source.into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        byte_start: offset,
                        byte_end: offset + bytes.len() as u64,
                        record_ordinal: sequence - 1,
                    },
                },
                bytes,
                TransportKind::NativeJsonl,
            )
            .await;
        // Reopening the worker after every admission also exercises durable joins.
        let progress = fixture
            .worker()
            .catch_up(store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_eq!(progress.suspended_sources, 0);
    }
}

#[tokio::test]
async fn corroborating_native_route_proofs_preserve_the_same_retained_claims_in_every_source_order()
{
    let mut expected = None;
    for (index, order) in [
        [0, 1, 2, 3],
        [0, 1, 3, 2],
        [2, 0, 1, 3],
        [3, 0, 1, 2],
        [2, 3, 0, 1],
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = Fixture::new().await;
        let (store, _) = fixture.generation(&format!("corroborated-{index}")).await;
        route_records(&fixture, order, &store, false, false).await;
        let actual: BTreeMap<_, _> = claims(&store)
            .await
            .into_iter()
            .map(|claim| (claim.claim_ref.clone(), claim))
            .collect();
        assert!(actual.values().any(|claim| matches!(&claim.fact, Fact::Declare { object: TraceObject::Message(message) } if matches!(&message.author, Author::Agent { agent: OwnerRef::Agent { agent } } if agent.anchor_session().native_id.as_str() == "sender"))));
        if let Some(expected) = &expected {
            assert_eq!(
                &actual, expected,
                "proof arrival order changed retained routing claims"
            );
        } else {
            expected = Some(actual);
        }
    }
}

fn owner(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{id}")).unwrap())
}
fn header(id: &str) -> Vec<u8> {
    let mut raw = serde_json::to_vec(&serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":id,"session_id":"root","agent_path":format!("/root/{id}")}})).unwrap();
    raw.push(b'\n');
    raw
}
fn routed(index: usize) -> Vec<u8> {
    let mut raw = serde_json::to_vec(&serde_json::json!({"type":"response_item","ordinal":index+1,"payload":{"type":"agent_message","id":format!("routed-{index}"),"author":"/root/sender","recipient":"/root","content":[{"type":"input_text","text":format!("retained routed body {index}")}]}})).unwrap();
    raw.push(b'\n');
    raw
}
async fn capture_on(
    fixture: &Fixture,
    source: &str,
    ordinal: u64,
    offset: u64,
    raw: &[u8],
    epoch: &IngestionEpoch,
) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: source.into(),
            producer_epoch: "retained".into(),
            producer_seq: ordinal + 1,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: source.into(),
            },
            generation: 0,
            locator: SourceLocator::Jsonl {
                byte_start: offset,
                byte_end: offset + raw.len() as u64,
                record_ordinal: ordinal,
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
        transport_kind: TransportKind::NativeJsonl,
        vendor_build: Some("synthetic-supported-shape".into()),
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
async fn observer_history(fixture: &Fixture, count: usize) {
    let header = header("observer");
    capture_on(fixture, "observer", 0, 0, &header, &fixture.epoch).await;
    let mut offset = header.len() as u64;
    for index in 0..count {
        let raw = routed(index);
        capture_on(
            fixture,
            "observer",
            index as u64 + 1,
            offset,
            &raw,
            &fixture.epoch,
        )
        .await;
        offset += raw.len() as u64;
    }
}
fn resolved_messages(values: &[Claim]) -> Vec<Message> {
    let mut reducer = sylph_trace::reducer::Reducer::default();
    reducer
        .apply(
            values,
            &values
                .iter()
                .flat_map(|claim| claim.evidence.iter().cloned())
                .collect(),
            values.len(),
        )
        .unwrap();
    reducer
        .resolve()
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Message(message)) => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn deleting_one_route_certificate_keeps_independent_support_until_the_last_proof_is_deleted()
{
    let fixture = Fixture::new().await;
    observer_history(&fixture, 1).await;
    let mut certificates = vec![];
    for name in ["proof-a", "proof-b"] {
        let scope = IngestionScope::Inventory(InventoryRef {
            environment: owner("sender").environment().clone(),
            vendor: AgentName::parse("codex").unwrap(),
            instance: name.into(),
        });
        let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
        let capture = capture_on(&fixture, name, 0, 0, &header("sender"), &epoch).await;
        certificates.push((scope, capture));
    }
    for deleted in 0..=2 {
        if deleted > 0 {
            fixture
                .evidence
                .delete_scope(&certificates[deleted - 1].0)
                .await
                .unwrap();
        }
        let (store, _) = fixture
            .generation(&format!("route-deletion-{deleted}"))
            .await;
        let progress = fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_eq!(progress.suspended_sources, 0);
        let retained = claims(&store).await;
        let messages = resolved_messages(&retained);
        assert_eq!(
            messages.len(),
            1,
            "proof deletion cannot erase the recipient-owned occurrence"
        );
        if deleted < 2 {
            assert_eq!(
                messages[0].author,
                Author::Agent {
                    agent: OwnerRef::Agent {
                        agent: owner("sender")
                    }
                }
            );
            let support: Vec<_> = retained.iter().filter(|claim| matches!(&claim.fact, Fact::Declare { object: TraceObject::Message(message) } if matches!(&message.author, Author::Agent { agent: OwnerRef::Agent { agent } } if agent == &owner("sender")))).collect();
            assert_eq!(support.len(), 2 - deleted);
            for claim in support {
                let used = certificates
                    .iter()
                    .filter(|(_, capture)| claim.evidence.contains(&capture.observation_ref))
                    .count();
                assert_eq!(
                    used, 1,
                    "independent certificates were made jointly mandatory"
                );
                for (_, capture) in &certificates[..deleted] {
                    assert!(!claim.evidence.contains(&capture.observation_ref));
                }
            }
        } else {
            assert!(matches!(
                messages[0].author,
                Author::Agent {
                    agent: OwnerRef::Unresolved { .. }
                }
            ));
        }
        let body = revision_bytes(
            &fixture,
            &store,
            &MessageRef::native(owner("observer"), "codex.message", vec!["routed-0".into()])
                .unwrap(),
        )
        .await;
        assert_eq!(body, vec![b"retained routed body 0".to_vec()]);
    }
}

#[tokio::test]
async fn a_late_route_certificate_resumes_bounded_refinements_from_a_committed_checkpoint() {
    use mapping::codex::routing::ROUTE_PAGE_WORK;
    let fixture = Fixture::new().await;
    let count = ROUTE_PAGE_WORK * 2;
    observer_history(&fixture, count).await;
    let (store, _) = fixture.generation("paged-routing").await;
    let worker = fixture.worker();
    let before = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&store, 1, before).await.unwrap();
    capture_on(
        &fixture,
        "late-sender",
        0,
        0,
        &header("sender"),
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
    assert!(
        unit.next
            .pending
            .values()
            .any(|pending| pending.requests.contains(&ArchiveRequest::ContinueRouting))
    );
    assert!(
        unit.batch
            .claims
            .iter()
            .filter(|claim| !claim.supersedes.is_empty())
            .count()
            <= ROUTE_PAGE_WORK
    );
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    // The restarted worker gets its plans and cursor solely from committed rows.
    let progress = fixture
        .worker()
        .catch_up(&store, 1, before + 1)
        .await
        .unwrap();
    assert_eq!(progress.pending_mappings, 0);
    assert_eq!(progress.suspended_sources, 0);
    let retained = claims(&store).await;
    let messages = resolved_messages(&retained);
    assert_eq!(messages.len(), count);
    assert!(messages.iter().all(|message| message.author
        == Author::Agent {
            agent: OwnerRef::Agent {
                agent: owner("sender")
            }
        }));
    let (rebuilt, _) = fixture.generation("paged-routing-rebuilt").await;
    fixture
        .worker()
        .catch_up(&rebuilt, 1, before + 1)
        .await
        .unwrap();
    assert_eq!(claims(&rebuilt).await, retained);
    assert_eq!(
        reader_projection(&fixture, &rebuilt).await,
        reader_projection(&fixture, &store).await
    );
}

#[tokio::test]
async fn repeated_goal_snapshots_keep_every_state_without_growing_the_identity_checkpoint() {
    use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};
    let fixture = Fixture::new().await;
    let count = 32;
    for sequence in 1..=count {
        let snapshot = NativeDatabaseSnapshot {
            database_locator: "/native/state_5.sqlite".into(),
            table: "thread_goals".into(),
            columns: vec![],
            status: NativeDatabaseStatus::Available,
            row: [
                (
                    "thread_id".into(),
                    NativeDatabaseValue::Text("observer".into()),
                ),
                (
                    "goal_id".into(),
                    NativeDatabaseValue::Text("ongoing-goal".into()),
                ),
                ("status".into(), NativeDatabaseValue::Text("active".into())),
                (
                    "tokens_used".into(),
                    NativeDatabaseValue::Integer(sequence as i64),
                ),
                (
                    "time_used_seconds".into(),
                    NativeDatabaseValue::Integer(sequence as i64),
                ),
                ("token_budget".into(), NativeDatabaseValue::Null),
            ]
            .into(),
            selected_source: None,
            selected_generation: None,
            selected_record_count: None,
            selected_pending_bytes: None,
        };
        let observation = native_observation(sequence);
        fixture
            .admit(
                observation.clone(),
                SourcePosition {
                    source: SourceRef {
                        issuer: observation.issuer,
                        instance: "goal-snapshots".into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Occurrence {
                        key: sequence.to_string(),
                    },
                },
                &serde_json::to_vec(&snapshot).unwrap(),
                TransportKind::NativeDatabase,
            )
            .await;
    }
    let (store, _) = fixture.generation("goal-checkpoint-bounds").await;
    fixture.worker().catch_up(&store, 1, count).await.unwrap();
    let retained = claims(&store).await;
    assert_eq!(
        retained
            .iter()
            .filter(|claim| matches!(claim.fact, Fact::ResourceState { .. }))
            .count(),
        count as usize
    );
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let states = store.mapper_states(&snapshot).await.unwrap();
    let bindings: Vec<CorrelationBinding> = states
        .into_iter()
        .filter(|(key, _)| key.starts_with("correlation:"))
        .map(|(_, value)| serde_json::from_value(value).unwrap())
        .collect();
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings[0].proofs.len(),
        1,
        "repeated identity checkpoints grew with goal polling despite retaining all resource states separately"
    );
}

#[tokio::test]
async fn conflicting_route_certificates_keep_content_readable_without_selecting_an_author_or_stalling_replay()
 {
    use crate::trace::reading_store::read_scope;
    for local_path in [false, true] {
        let mut expected = None;
        for (index, order) in [[0, 1, 2, 3], [0, 1, 3, 2], [2, 3, 0, 1], [3, 2, 0, 1]]
            .into_iter()
            .enumerate()
        {
            let fixture = Fixture::new().await;
            let (store, _) = fixture
                .generation(&format!("conflicting-route-{index}"))
                .await;
            route_records(&fixture, order, &store, true, local_path).await;
            let retained = claims(&store).await;
            let mut reducer = sylph_trace::reducer::Reducer::default();
            reducer
                .apply(
                    &retained,
                    &retained
                        .iter()
                        .flat_map(|claim| claim.evidence.iter().cloned())
                        .collect(),
                    retained.len(),
                )
                .unwrap();
            let reduced = reducer.resolve();
            let message = MessageRef::native(
                owner("observer"),
                "codex.message",
                vec!["corroborated".into()],
            )
            .unwrap();
            assert!(matches!(
                reduced
                    .object(&AnyObjectRef::Message(message.clone()))
                    .unwrap()
                    .declaration,
                sylph_trace::reducer::Resolved::Conflicted { .. }
            ));
            let snapshot = store.checkpoint().await.unwrap().snapshot;
            let page = store
                .read_page(
                    &snapshot,
                    &read_scope(&owner("observer"), None, "history"),
                    None,
                    None,
                    None,
                    store.limits().max_items,
                    false,
                )
                .await
                .unwrap();
            assert_eq!(page.rows.len(), 1);
            let unit: ReadingUnit = serde_json::from_value(page.rows[0].body.clone()).unwrap();
            let ReadingContent::Message { author, parts, .. } = unit.content else {
                panic!("routing uncertainty hid retained message content")
            };
            assert!(matches!(
                author,
                ReadAuthor::Unknown | ReadAuthor::Agent { agent: None, .. }
            ));
            assert_eq!(parts[0].preview, "independently proved sender");
            assert_eq!(
                revision_bytes(&fixture, &store, &message).await,
                vec![b"independently proved sender".to_vec()]
            );
            let actual: BTreeMap<_, _> = retained
                .into_iter()
                .map(|claim| (claim.claim_ref.clone(), claim))
                .collect();
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected);
            } else {
                expected = Some(actual);
            }
        }
    }
}

#[tokio::test]
async fn deleting_a_conflicting_certificate_resolves_current_routing_without_rewriting_an_older_snapshot()
 {
    use crate::trace::reading_store::read_scope;
    let fixture = Fixture::new().await;
    observer_history(&fixture, 1).await;
    let mut epochs = vec![];
    for name in ["route-a", "route-b"] {
        let scope = IngestionScope::Inventory(InventoryRef {
            environment: owner("sender").environment().clone(),
            vendor: AgentName::parse("codex").unwrap(),
            instance: name.into(),
        });
        let epoch = fixture.evidence.authorize_scope(&scope, &[]).await.unwrap();
        epochs.push((scope, epoch));
    }
    let (store, _) = fixture.generation("routing-conflict-deletion").await;
    capture_on(&fixture, "route-a", 0, 0, &header("sender"), &epochs[0].1).await;
    fixture
        .worker()
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let old = store.checkpoint().await.unwrap().snapshot;
    let mut other: serde_json::Value = serde_json::from_slice(&header("sender")).unwrap();
    other["payload"]["id"] = "other-sender".into();
    let mut raw = serde_json::to_vec(&other).unwrap();
    raw.push(b'\n');
    capture_on(&fixture, "route-b", 0, 0, &raw, &epochs[1].1).await;
    fixture
        .worker()
        .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let current = store.checkpoint().await.unwrap().snapshot;
    for (snapshot, conflicted) in [(&old, false), (&current, true)] {
        let page = store
            .read_page(
                snapshot,
                &read_scope(&owner("observer"), None, "history"),
                None,
                None,
                None,
                store.limits().max_items,
                false,
            )
            .await
            .unwrap();
        let unit: ReadingUnit = serde_json::from_value(page.rows[0].body.clone()).unwrap();
        let ReadingContent::Message { author, parts, .. } = unit.content else {
            panic!("uncertain metadata hid content")
        };
        if conflicted {
            assert!(matches!(author, ReadAuthor::Unknown));
        } else {
            assert!(
                matches!(author, ReadAuthor::Agent { agent: Some(agent), .. } if agent.address.agent() == owner("sender"))
            );
        }
        let access = &parts[0].payload;
        let anchor = crate::trace::generation::PayloadAnchor {
            via_ref: access.via_ref.object(),
            containing_agent: access.containing_agent.agent(),
            selection: access.selection.clone(),
            payload_ref: access.payload_ref.clone(),
            body_revision: access.body_revision.clone(),
            representation: access.representation,
            ranges: vec![],
            dependency_epochs: Default::default(),
        };
        store
            .authorize_payload(snapshot, &anchor, 0, b"retained routed body 0".len() as u64)
            .await
            .unwrap();
    }
    let message = AnyObjectRef::Message(
        MessageRef::native(owner("observer"), "codex.message", vec!["routed-0".into()]).unwrap(),
    );
    let page = store
        .claims_for(
            &current,
            &message,
            Some(TraceFactKind::Declaration),
            None,
            store.limits().max_items,
        )
        .await
        .unwrap();
    let mut witnesses = BTreeSet::new();
    for summary in page.claims {
        let claim = store.claim(&current, &summary.claim_ref).await.unwrap();
        witnesses.extend(
            claim
                .evidence
                .into_iter()
                .map(|evidence| evidence.producer_id),
        );
    }
    assert!(witnesses.contains("route-a") && witnesses.contains("route-b"));
    fixture.evidence.delete_scope(&epochs[1].0).await.unwrap();
    let (rebuilt, _) = fixture.generation("routing-conflict-removed").await;
    fixture
        .worker()
        .catch_up(&rebuilt, 1, fixture.evidence.evidence_cut().await.unwrap())
        .await
        .unwrap();
    let messages = resolved_messages(&claims(&rebuilt).await);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].author,
        Author::Agent {
            agent: OwnerRef::Agent {
                agent: owner("sender")
            }
        }
    );
    assert_eq!(
        revision_bytes(
            &fixture,
            &rebuilt,
            &MessageRef::native(owner("observer"), "codex.message", vec!["routed-0".into()])
                .unwrap()
        )
        .await,
        vec![b"retained routed body 0".to_vec()]
    );
}
