use super::super::*;

fn agent(name: &str) -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env:codex:{name}")).unwrap())
}
fn claim(index: u64, fact: Fact) -> Claim {
    let position = SourcePosition {
        source: SourceRef {
            issuer: Issuer::Server("incremental-tests".into()),
            instance: "tail".into(),
        },
        generation: 0,
        locator: SourceLocator::Frame { ordinal: index },
    };
    Claim {
        claim_ref: ClaimRef::occurrence(agent("root"), position, vec!["claim".into()]).unwrap(),
        fact,
        evidence: vec![ObservationRef {
            issuer: Issuer::Server("incremental-tests".into()),
            producer_id: "fixture".into(),
            producer_epoch: "one".into(),
            producer_seq: index + 1,
        }],
        rule_id: "incremental-test".into(),
        mapper_build: "test".into(),
        supersedes: vec![],
        source_revision: None,
    }
}
fn available(claims: &[Claim]) -> imbl::OrdSet<ObservationRef> {
    claims
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect()
}
fn user_message(index: u64) -> Claim {
    let owner = agent("root");
    let message = MessageRef::native(
        owner.clone(),
        "incremental.message",
        vec![format!("message-{index}")],
    )
    .unwrap();
    let base = index * 4;
    claim(
        base,
        Fact::Declare {
            object: TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Agent {
                    agent: OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                },
                protocol_role: Some(ProtocolRole::Assistant),
                purpose: MessagePurpose::Answer,
                production: None,
            }),
        },
    )
}
fn message_revision(index: u64) -> Claim {
    let owner = agent("root");
    let message = MessageRef::native(
        owner.clone(),
        "incremental.message",
        vec![format!("message-{index}")],
    )
    .unwrap();
    let revision = MessageRevisionRef::native(
        owner.clone(),
        "incremental.revision",
        vec![format!("message-{index}")],
    )
    .unwrap();
    let part = MessagePartRef::native(
        owner,
        "incremental.part",
        vec![format!("message-{index}"), "0".into()],
    )
    .unwrap();
    claim(
        index * 4 + 1,
        Fact::Declare {
            object: TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision,
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: part,
                    kind: ContentKind::Text,
                    payload: PayloadRef::bytes(
                        blake3::hash(format!("body-{index}").as_bytes()).to_hex(),
                    )
                    .unwrap(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
        },
    )
}
fn entry(index: u64) -> Claim {
    let owner = agent("root");
    let entry = EntryRef::native(
        owner.clone(),
        "incremental.entry",
        vec![format!("entry-{index}")],
    )
    .unwrap();
    let predecessor = (index > 0).then(|| {
        EntryRef::native(
            owner.clone(),
            "incremental.entry",
            vec![format!("entry-{}", index - 1)],
        )
        .unwrap()
    });
    claim(
        index * 4 + 2,
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry,
                containing_agent: owner,
                predecessor,
                original_owner: None,
            }),
        },
    )
}
fn content(index: u64) -> Claim {
    let owner = agent("root");
    let entry = EntryRef::native(
        owner.clone(),
        "incremental.entry",
        vec![format!("entry-{index}")],
    )
    .unwrap();
    let revision = MessageRevisionRef::native(
        owner,
        "incremental.revision",
        vec![format!("message-{index}")],
    )
    .unwrap();
    claim(
        index * 4 + 3,
        Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry,
                SourcePosition {
                    source: SourceRef {
                        issuer: Issuer::Server("incremental-tests".into()),
                        instance: "tail".into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Frame { ordinal: index },
                },
                "incremental/native",
                vec![HistoryContent::Message(revision)],
            ),
        },
    )
}
fn tail(index: u64) -> Vec<Claim> {
    vec![
        user_message(index),
        message_revision(index),
        entry(index),
        content(index),
    ]
}
fn full_reduce(claims: &[Claim]) -> Reduction {
    let mut reducer = Reducer::default();
    reducer
        .apply(claims, &available(claims), claims.len() + 1)
        .unwrap();
    reducer.resolve()
}

#[test]
fn independent_tails_at_twenty_and_two_hundred_match_full_oracle_with_constant_work() {
    let mut work = Vec::new();
    for count in [20usize, 200] {
        let mut reducer = Reducer::default();
        let mut claims = Vec::new();
        for index in 0..count {
            let batch = tail(index as u64);
            let evidence = available(&batch);
            reducer.apply(&batch, &evidence, batch.len()).unwrap();
            let resolution = reducer.resolve_incremental(100_000);
            claims.extend(batch);
            assert_eq!(*resolution.reduction, full_reduce(&claims));
            let before = if index == 0 {
                Reduction::default()
            } else {
                full_reduce(&claims[..claims.len() - 4])
            };
            assert_eq!(
                resolution.changes,
                ReductionChanges::between(&before, &full_reduce(&claims),)
            );
            if index > 0 {
                assert_eq!(resolution.work.full_resolutions, 0);
            }
            work.push(resolution.work.claims_resolved);
        }
    }
    assert!(work[20..].iter().all(|work| *work <= 4));
}

#[test]
fn redelivery_has_no_incremental_work_or_changes() {
    let claims = tail(0);
    let mut reducer = Reducer::default();
    reducer
        .apply(&claims, &available(&claims), claims.len())
        .unwrap();
    let first = reducer.resolve_incremental(100_000);
    reducer
        .apply(&claims, &available(&claims), claims.len())
        .unwrap();
    let second = reducer.resolve_incremental(100_000);
    assert!(second.changes.is_empty());
    assert_eq!(second.work.claims_resolved, 0);
    assert_eq!(*second.reduction, *first.reduction);
}

#[test]
fn dependency_budget_falls_back_without_losing_semantics() {
    let claims = tail(0);
    let mut reducer = Reducer::default();
    reducer
        .apply(&claims, &available(&claims), claims.len())
        .unwrap();
    let resolution = reducer.resolve_incremental(1);
    assert_eq!(*resolution.reduction, full_reduce(&claims));
    assert_eq!(resolution.work.full_resolutions, 1);
    assert!(resolution.work.dependency_index_unavailable);
}

#[test]
fn serde_restore_rebuilds_the_incremental_index_and_keeps_oracle_equivalence() {
    let first = tail(0);
    let mut reducer = Reducer::default();
    reducer
        .apply(&first, &available(&first), first.len())
        .unwrap();
    let initial = reducer.resolve_incremental(100_000);
    let bytes = serde_json::to_vec(&reducer).unwrap();
    let mut restored: Reducer = serde_json::from_slice(&bytes).unwrap();
    let second = tail(1);
    restored
        .apply(&second, &available(&second), second.len())
        .unwrap();
    let resolution = restored.resolve_incremental(100_000);
    assert_eq!(
        *resolution.reduction,
        full_reduce(&[first, second].concat())
    );
    assert_eq!(resolution.work.full_resolutions, 1);
    assert_eq!(initial.reduction.objects.len(), 3);
}

#[test]
fn missing_predecessor_or_revision_ref_forces_full_resolution() {
    let mut reducer = Reducer::default();
    let initial = tail(0);
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    reducer.resolve_incremental(100_000);
    let mut missing = tail(1);
    if let Fact::Declare {
        object: TraceObject::HistoryEntry(entry),
    } = &mut missing[2].fact
    {
        entry.predecessor = Some(
            EntryRef::native(agent("root"), "incremental.entry", vec!["never".into()]).unwrap(),
        );
    }
    reducer
        .apply(&missing, &available(&missing), missing.len())
        .unwrap();
    let resolution = reducer.resolve_incremental(100_000);
    assert_eq!(resolution.work.full_resolutions, 1);
    assert_eq!(*resolution.reduction, reducer.resolve());
}

#[test]
fn selection_sibling_order_and_correction_dependencies_force_full_resolution() {
    let mut reducer = Reducer::default();
    let initial = tail(0);
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    reducer.resolve_incremental(100_000);
    let owner = agent("root");
    let selection =
        SelectionRef::native(owner.clone(), "selection", vec!["current".into()]).unwrap();
    let selection_claim = claim(
        900,
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: owner.clone(),
                membership: SelectionMembership::EntryPath {
                    head: EntryRef::native(
                        owner.clone(),
                        "incremental.entry",
                        vec!["entry-0".into()],
                    )
                    .unwrap(),
                },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        },
    );
    reducer
        .apply(
            std::slice::from_ref(&selection_claim),
            &available(std::slice::from_ref(&selection_claim)),
            1,
        )
        .unwrap();
    let result = reducer.resolve_incremental(100_000);
    assert_eq!(result.work.full_resolutions, 1);
    let mut correction = tail(1);
    correction[0].supersedes.push(initial[0].claim_ref.clone());
    reducer
        .apply(&correction, &available(&correction), correction.len())
        .unwrap();
    assert_eq!(
        reducer.resolve_incremental(100_000).work.full_resolutions,
        1
    );
}

#[test]
fn a_late_message_declaration_reconciles_its_preexisting_revision_evidence() {
    let mut initial = tail(0);
    let Fact::Declare {
        object: TraceObject::MessageRevision(revision),
    } = &mut initial[1].fact
    else {
        unreachable!()
    };
    revision.message = MessageRef::native(
        agent("root"),
        "incremental.message",
        vec!["message-1".into()],
    )
    .unwrap();
    let mut reducer = Reducer::default();
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    let old = reducer.resolve_incremental(100_000);
    let next = tail(1);
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(100_000);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
    assert_eq!(
        resolved.changes,
        ReductionChanges::between(&old.reduction, &resolved.reduction)
    );
    assert!(resolved
        .reduction
        .messages
        .values()
        .any(|history| history.revisions.len() == 2));
}

fn extent() -> SourceExtent {
    SourceExtent {
        source: SourceRef {
            issuer: Issuer::Server("incremental-tests".into()),
            instance: "tail".into(),
        },
        generation: 0,
        coordinate: HistoryCoordinate::LocalRecordIndex,
        start_inclusive: 0,
        end_exclusive: 2,
        mapping_rule: "incremental/native".into(),
        original_owner: None,
    }
}

#[test]
fn a_preexisting_correction_waiting_for_a_new_claim_reconciles_withdrawal() {
    let future = claim(8, Fact::HistoryMappingComplete { extent: extent() });
    let mut correction = claim(1000, future.fact.clone());
    correction.supersedes.push(future.claim_ref.clone());
    let mut initial = tail(0);
    initial.push(correction);
    let mut reducer = Reducer::default();
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    let old = reducer.resolve_incremental(100_000);
    assert!(old
        .reduction
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingSource));
    let mut next = tail(1);
    next.push(future.clone());
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(100_000);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
    assert!(resolved
        .reduction
        .withdrawn_claims
        .contains(&future.claim_ref));
    assert!(!resolved
        .reduction
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingSource));
}

#[test]
fn a_source_completion_reconciles_a_selection_owned_by_another_agent() {
    let owner = agent("child");
    let selection =
        SelectionRef::native(owner.clone(), "selection", vec!["source".into()]).unwrap();
    let mut initial = tail(0);
    initial.push(claim(
        1000,
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: owner,
                membership: SelectionMembership::SourceExtents {
                    extents: vec![extent()],
                    inherited_selections: vec![],
                },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        },
    ));
    let mut reducer = Reducer::default();
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    let old = reducer.resolve_incremental(100_000);
    assert_eq!(old.reduction.selection_memberships[&selection], None);
    let mut next = tail(1);
    let Fact::HistoryContentObserved { observation } = &next[3].fact else {
        unreachable!()
    };
    let added = observation.entry.clone();
    next.push(claim(
        8,
        Fact::HistoryPosition {
            entry: added.clone(),
            position: observation.position.clone(),
            coordinate: HistoryCoordinate::LocalRecordIndex,
            value: 1,
            mapping_rule: "incremental/native".into(),
        },
    ));
    next.push(claim(9, Fact::HistoryMappingComplete { extent: extent() }));
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(100_000);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
    assert_eq!(
        resolved.reduction.selection_memberships[&selection],
        Some(vec![added])
    );
}

#[test]
fn a_deferred_equivalence_endpoint_reconciles_the_existing_message_class() {
    let mut initial = tail(0);
    let left = AnyObjectRef::Message(
        MessageRef::native(
            agent("root"),
            "incremental.message",
            vec!["message-0".into()],
        )
        .unwrap(),
    );
    let right = AnyObjectRef::Message(
        MessageRef::native(
            agent("root"),
            "incremental.message",
            vec!["message-1".into()],
        )
        .unwrap(),
    );
    initial.push(claim(
        1000,
        Fact::Equivalence {
            left: left.clone(),
            right: right.clone(),
            rule: "explicit-vendor-identity".into(),
        },
    ));
    let mut reducer = Reducer::default();
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    reducer.resolve_incremental(100_000);
    let next = tail(1);
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(100_000);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
    assert_eq!(
        resolved.reduction.representatives[&left],
        resolved.reduction.representatives[&right]
    );
}

#[test]
fn sibling_insertion_reconciles_existing_order_labels_and_heads() {
    let initial = [tail(0), tail(1)].concat();
    let mut reducer = Reducer::default();
    reducer
        .apply(&initial, &available(&initial), initial.len())
        .unwrap();
    reducer.resolve_incremental(100_000);
    let mut next = tail(2);
    let Fact::Declare {
        object: TraceObject::HistoryEntry(entry),
    } = &mut next[2].fact
    else {
        unreachable!()
    };
    entry.predecessor =
        Some(EntryRef::native(agent("root"), "incremental.entry", vec!["entry-0".into()]).unwrap());
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(100_000);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
    assert_eq!(
        resolved.reduction.histories[&agent("root")]
            .candidate_heads
            .len(),
        2
    );
}

#[test]
fn lowering_the_dependency_budget_without_new_claims_releases_the_old_index() {
    let first = tail(0);
    let mut reducer = Reducer::default();
    reducer
        .apply(&first, &available(&first), first.len())
        .unwrap();
    let before = reducer.resolve_incremental(100_000);
    assert!(!before.work.dependency_index_unavailable);
    let limited = reducer.resolve_incremental(1);
    assert!(limited.work.dependency_index_unavailable);
    assert_eq!(limited.reduction, before.reduction);
    assert!(limited.changes.is_empty());
    let next = tail(1);
    reducer.apply(&next, &available(&next), next.len()).unwrap();
    let resolved = reducer.resolve_incremental(1);
    assert_eq!(resolved.work.full_resolutions, 1);
    assert_eq!(*resolved.reduction, reducer.resolve());
}

fn entry_ref(index: u64) -> EntryRef {
    EntryRef::native(
        agent("root"),
        "incremental.entry",
        vec![format!("entry-{index}")],
    )
    .unwrap()
}

fn append_resolution(reducer: &mut Reducer, batch: &[Claim]) -> Resolution {
    reducer
        .apply(batch, &available(batch), batch.len())
        .unwrap();
    reducer.resolve_incremental(100_000)
}

#[test]
fn append_certificate_binds_exact_snapshots_and_names_the_ordered_tail() {
    let mut reducer = Reducer::default();
    let before = append_resolution(&mut reducer, &[tail(0), tail(1)].concat());
    assert!(before.append.is_none());
    let mut added = [tail(3), tail(2)].concat();
    for index in [3, 2] {
        added.push(claim(
            1000 + index,
            Fact::Relation {
                relation: Relation::HistoryPredecessor {
                    predecessor: entry_ref(index - 1),
                    entry: entry_ref(index),
                },
            },
        ));
    }
    let after = append_resolution(&mut reducer, &added);
    let proof = after.append.as_ref().unwrap();
    assert!(proof.matches(&before.reduction, &after.reduction));
    assert!(std::sync::Arc::ptr_eq(proof.before(), &before.reduction));
    assert!(std::sync::Arc::ptr_eq(proof.after(), &after.reduction));
    assert!(!proof.matches(&before.reduction.as_ref().clone(), &after.reduction));
    assert!(!proof.matches(&before.reduction, &after.reduction.as_ref().clone()));
    assert!(!proof.matches(&after.reduction, &before.reduction));
    assert_eq!(proof.agents().len(), 1);
    let affected = &proof.agents()[&agent("root")];
    assert_eq!(affected.previous_archive_len(), 2);
    assert_eq!(affected.previous_head(), &entry_ref(1));
    assert_eq!(affected.added_entries(), &[entry_ref(2), entry_ref(3)]);
    let expected_objects = added
        .iter()
        .filter_map(|claim| match &claim.fact {
            Fact::Declare { object } => Some(object.object_ref()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(proof.objects(), &expected_objects);
    assert_eq!(proof.objects().len(), 6);
    assert_eq!(proof.predecessor_relations().count(), 2);
    for index in [2, 3] {
        assert!(proof.predecessor_relations().any(|relation| matches!(
            &relation.relation, Relation::HistoryPredecessor { predecessor, entry }
                if predecessor == &entry_ref(index - 1) && entry == &entry_ref(index)
        )));
    }
    assert_eq!(after.reduction.as_ref(), &reducer.resolve());
}

#[test]
fn retained_append_certificate_prevents_mutating_its_snapshot_in_place() {
    let mut reducer = Reducer::default();
    append_resolution(&mut reducer, &tail(0));
    let resolved = append_resolution(&mut reducer, &tail(1));
    let proof = resolved.append.unwrap();
    let mut snapshot = resolved.reduction;
    drop(reducer);
    assert!(std::sync::Arc::get_mut(&mut snapshot).is_none());
    let original = proof.after().clone();
    std::sync::Arc::make_mut(&mut snapshot)
        .diagnostics
        .push_back(ReductionDiagnostic {
            code: DiagnosticCode::CaptureGap,
            claims: vec![],
            message: "candidate mutation".into(),
        });
    assert!(!proof.matches(proof.before(), &snapshot));
    assert!(proof.matches(proof.before(), &original));
    assert!(original.diagnostics.is_empty());
}

#[test]
fn consecutive_append_certificates_compose_through_exact_noop_snapshots() {
    let mut reducer = Reducer::default();
    let initial = append_resolution(&mut reducer, &tail(0));
    let first = append_resolution(&mut reducer, &tail(1));
    let unchanged = reducer.resolve_incremental(100_000);
    let identity = unchanged.append.unwrap();
    assert!(identity.agents().is_empty());
    assert!(identity.objects().is_empty());
    assert_eq!(identity.predecessor_relations().count(), 0);
    assert!(identity.matches(&first.reduction, &first.reduction));
    let last = append_resolution(&mut reducer, &tail(2));
    let proof = first
        .append
        .unwrap()
        .compose(identity)
        .unwrap()
        .compose(last.append.unwrap())
        .unwrap();
    assert!(proof.matches(&initial.reduction, &last.reduction));
    let affected = &proof.agents()[&agent("root")];
    assert_eq!(affected.previous_archive_len(), 1);
    assert_eq!(affected.previous_head(), &entry_ref(0));
    assert_eq!(affected.added_entries(), &[entry_ref(1), entry_ref(2)]);
    assert_eq!(proof.objects().len(), 6);
}

#[test]
fn equal_reductions_from_another_resolver_cannot_supply_a_composition_boundary() {
    let mut first_reducer = Reducer::default();
    append_resolution(&mut first_reducer, &tail(0));
    let first = append_resolution(&mut first_reducer, &tail(1));
    let mut other_reducer = Reducer::default();
    let equal_middle = append_resolution(&mut other_reducer, &[tail(0), tail(1)].concat());
    assert_eq!(first.reduction, equal_middle.reduction);
    let other = append_resolution(&mut other_reducer, &tail(2));
    assert!(first
        .append
        .unwrap()
        .compose(other.append.unwrap())
        .is_none());
}

#[test]
fn full_reconciliation_breaks_the_certificate_chain_even_before_a_later_valid_append() {
    let mut reducer = Reducer::default();
    append_resolution(&mut reducer, &tail(0));
    let first = append_resolution(&mut reducer, &tail(1));
    let diagnostic = claim(
        1000,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                subject: None,
                code: DiagnosticCode::CaptureGap,
                message: "unrelated retained evidence".into(),
                related_claims: vec![],
            },
        },
    );
    let fallback = append_resolution(&mut reducer, &[diagnostic]);
    assert_eq!(fallback.work.full_resolutions, 1);
    assert!(fallback.append.is_none());
    let last = append_resolution(&mut reducer, &tail(2));
    assert_eq!(last.work.full_resolutions, 0);
    let later = last.append.unwrap();
    assert!(later.matches(&fallback.reduction, &last.reduction));
    assert!(first.append.unwrap().compose(later).is_none());
}
