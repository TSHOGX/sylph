use super::*;

mod owners;

fn agent(name: &str) -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env7fk2:codex:{name}")).unwrap())
}

fn source() -> SourceRef {
    SourceRef {
        issuer: Issuer::Server("fixture".into()),
        instance: "observed-source".into(),
    }
}

fn position(index: u64) -> SourcePosition {
    SourcePosition {
        source: source(),
        generation: 0,
        locator: SourceLocator::Frame { ordinal: index },
    }
}

fn claim(index: u64, fact: Fact) -> Claim {
    Claim {
        claim_ref: ClaimRef::occurrence(agent("root"), position(index), vec!["fact".into()])
            .unwrap(),
        fact,
        evidence: vec![ObservationRef {
            issuer: Issuer::Server("fixture".into()),
            producer_id: "capturer".into(),
            producer_epoch: "1".into(),
            producer_seq: index,
        }],
        rule_id: "fixture/explicit-evidence".into(),
        mapper_build: "test-1".into(),
        supersedes: vec![],
        source_revision: None,
    }
}

fn declared_agent(index: u64, name: &str) -> Claim {
    claim(
        index,
        Fact::Declare {
            object: TraceObject::Agent(AgentThread {
                agent_ref: agent(name),
                vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                name: None,
                model: None,
            }),
        },
    )
}

fn activation(name: &str, key: &str) -> ActivationRef {
    ActivationRef::native(agent(name), "activation", vec![key.into()]).unwrap()
}

fn declared_activation(index: u64, name: &str, key: &str) -> Claim {
    claim(
        index,
        Fact::Declare {
            object: TraceObject::Activation(Activation {
                activation_ref: activation(name, key),
                owner: OwnerRef::Agent { agent: agent(name) },
            }),
        },
    )
}

fn execution(index: u64, name: &str, key: &str, boundary: ExecutionBoundary) -> Claim {
    claim(
        index,
        Fact::Execution {
            execution: ExecutionRef::Activation(activation(name, key)),
            boundary,
        },
    )
}

fn evidence(claims: &[Claim]) -> imbl::OrdSet<ObservationRef> {
    claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect()
}

fn reduce(claims: &[Claim]) -> Reduction {
    let mut reducer = Reducer::default();
    reducer.apply(claims, &evidence(claims), 1000).unwrap();
    reducer.resolve()
}

#[test]
fn batching_redelivery_and_parent_completion_preserve_independent_child_outcomes() {
    let claims = vec![
        declared_agent(1, "root"),
        declared_agent(2, "child"),
        declared_activation(3, "root", "a1"),
        declared_activation(4, "child", "b1"),
        declared_activation(5, "child", "b2"),
        execution(
            6,
            "root",
            "a1",
            ExecutionBoundary::Outcome {
                outcome: TerminalOutcome::Succeeded,
                reason: None,
                detail: None,
            },
        ),
        execution(
            7,
            "child",
            "b1",
            ExecutionBoundary::Outcome {
                outcome: TerminalOutcome::Failed,
                reason: None,
                detail: None,
            },
        ),
        execution(8, "child", "b2", ExecutionBoundary::Started { time: None }),
        claim(
            9,
            Fact::Relation {
                relation: Relation::Spawn {
                    parent: agent("root"),
                    child: agent("child"),
                    operation: None,
                },
            },
        ),
    ];
    let expected = reduce(&claims);
    let mut incremental = Reducer::default();
    let available = evidence(&claims);
    for index in [8, 6, 2, 0, 7, 1, 3, 5, 4, 0, 6] {
        incremental
            .apply(&[claims[index].clone()], &available, 1)
            .unwrap();
    }
    assert_eq!(incremental.resolve(), expected);
    assert_eq!(
        expected.executions[&AnyObjectRef::Activation(activation("root", "a1"))]
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert_eq!(
        expected.executions[&AnyObjectRef::Activation(activation("child", "b1"))]
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    let following = &expected.executions[&AnyObjectRef::Activation(activation("child", "b2"))];
    assert!(following.terminated.is_empty());
    assert!(matches!(following.outcome, Resolved::Unknown));
    assert_eq!(
        expected
            .objects
            .values()
            .filter(|v| matches!(v.reference, AnyObjectRef::Agent(_)))
            .count(),
        2
    );
}

#[test]
fn generic_termination_cannot_overwrite_a_named_terminal_error() {
    let failed = execution(
        1,
        "root",
        "a1",
        ExecutionBoundary::Outcome {
            outcome: TerminalOutcome::Failed,
            reason: Some(ExecutionCloseReason::VendorStop),
            detail: Some(PayloadRef::bytes("a".repeat(64)).unwrap()),
        },
    );
    let generic = execution(
        2,
        "root",
        "a1",
        ExecutionBoundary::Terminated {
            time: None,
            reason: None,
        },
    );
    let result = reduce(&[failed, generic]);
    assert_eq!(
        result.executions[&AnyObjectRef::Activation(activation("root", "a1"))]
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    let unknown = reduce(&[execution(
        3,
        "root",
        "a2",
        ExecutionBoundary::Terminated {
            time: None,
            reason: None,
        },
    )]);
    assert!(matches!(
        unknown.executions[&AnyObjectRef::Activation(activation("root", "a2"))].outcome,
        Resolved::Unknown
    ));
}

#[test]
fn conflicting_outcomes_require_a_correction_in_the_same_field() {
    let failed = execution(
        1,
        "root",
        "a1",
        ExecutionBoundary::Outcome {
            outcome: TerminalOutcome::Failed,
            reason: None,
            detail: None,
        },
    );
    let mut success = execution(
        2,
        "root",
        "a1",
        ExecutionBoundary::Outcome {
            outcome: TerminalOutcome::Succeeded,
            reason: None,
            detail: None,
        },
    );
    let reference = AnyObjectRef::Activation(activation("root", "a1"));
    assert!(matches!(
        reduce(&[failed.clone(), success.clone()]).executions[&reference].outcome,
        Resolved::Conflicted { .. }
    ));
    success.supersedes = vec![failed.claim_ref.clone()];
    assert_eq!(
        reduce(&[failed.clone(), success.clone()]).executions[&reference]
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let mut wrong = execution(
        3,
        "child",
        "b1",
        ExecutionBoundary::Outcome {
            outcome: TerminalOutcome::Succeeded,
            reason: None,
            detail: None,
        },
    );
    wrong.supersedes = vec![failed.claim_ref.clone()];
    assert_eq!(
        reduce(&[failed, wrong]).executions[&reference]
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
}

fn message(index: u64, key: &str) -> Claim {
    claim(
        index,
        Fact::Declare {
            object: TraceObject::Message(Message {
                message_ref: MessageRef::native(agent("root"), "message", vec![key.into()])
                    .unwrap(),
                author: Author::Human {
                    subject: Some("person".into()),
                },
                protocol_role: Some(ProtocolRole::User),
                purpose: MessagePurpose::TaskInput,
                production: None,
            }),
        },
    )
}

#[test]
fn equivalence_is_reversible_and_never_inferred_from_equal_message_content() {
    let first = message(1, "one");
    let second = message(2, "two");
    let a = AnyObjectRef::Message(
        MessageRef::native(agent("root"), "message", vec!["one".into()]).unwrap(),
    );
    let b = AnyObjectRef::Message(
        MessageRef::native(agent("root"), "message", vec!["two".into()]).unwrap(),
    );
    let separate = reduce(&[first.clone(), second.clone()]);
    assert_ne!(separate.representatives[&a], separate.representatives[&b]);
    let equivalence = claim(
        3,
        Fact::Equivalence {
            left: a.clone(),
            right: b.clone(),
            rule: "explicit-native-correlation".into(),
        },
    );
    let merged = reduce(&[equivalence, second.clone(), first.clone()]);
    assert_eq!(merged.representatives[&a], merged.representatives[&b]);
    assert!(merged.object(&a).is_some() && merged.object(&b).is_some());
    let split = reduce(&[first, second]);
    assert_ne!(split.representatives[&a], split.representatives[&b]);
}

#[test]
fn incompatible_owner_claims_do_not_move_work_to_the_current_root() {
    let operation = OperationRef::native(agent("root"), "tool", vec!["call-1".into()]).unwrap();
    let owned = Operation {
        operation_ref: operation.clone(),
        owner: OwnerRef::Agent {
            agent: agent("child"),
        },
        activation: Some(activation("child", "b1")),
        requester: None,
        category: OperationCategory::Tool {
            namespace: Some("shell".into()),
            name: "exec".into(),
        },
        input_revision: None,
        argument_state: ArgumentState::Sealed,
    };
    let a = claim(
        1,
        Fact::Declare {
            object: TraceObject::Operation(owned.clone()),
        },
    );
    let mut moved = owned;
    moved.owner = OwnerRef::Agent {
        agent: agent("root"),
    };
    let b = claim(
        2,
        Fact::Declare {
            object: TraceObject::Operation(moved),
        },
    );
    let result = reduce(&[a, b]);
    assert!(matches!(
        result.objects[&AnyObjectRef::Operation(operation)].declaration,
        Resolved::Conflicted { .. }
    ));
}

fn entry(key: &str) -> EntryRef {
    EntryRef::native(agent("root"), "history", vec![key.into()]).unwrap()
}
fn history(index: u64, key: &str, parent: Option<&str>) -> Claim {
    claim(
        index,
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry(key),
                containing_agent: agent("root"),
                predecessor: parent.map(entry),
                original_owner: None,
            }),
        },
    )
}

#[test]
fn selected_history_follows_predecessors_and_preserves_other_branches() {
    let mut claims = vec![
        history(1, "p", None),
        history(2, "a", Some("p")),
        history(3, "a2", Some("a")),
        history(4, "b", Some("p")),
        history(5, "b2", Some("b")),
    ];
    let unknown = reduce(&claims);
    let view = &unknown.histories[&agent("root")];
    assert!(view.selected_entries.is_none());
    assert_eq!(
        view.candidate_heads,
        imbl::vector![entry("a2"), entry("b2")]
    );
    let selection = SelectionRef::native(agent("root"), "selected", vec!["b2".into()]).unwrap();
    claims.push(claim(
        6,
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent("root"),
                membership: SelectionMembership::EntryPath { head: entry("b2") },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![entry("a2")],
            }),
        },
    ));
    claims.push(claim(
        7,
        Fact::SelectHistory {
            agent: agent("root"),
            selection,
            selected_at: position(7),
        },
    ));
    let result = reduce(&claims);
    let view = &result.histories[&agent("root")];
    assert_eq!(
        view.selected_entries,
        Some(imbl::vector![entry("p"), entry("b"), entry("b2")])
    );
    assert_eq!(view.archive_order.len(), 5);
}

#[test]
fn spawn_cycles_and_competing_parents_are_quarantined_without_inventing_agents() {
    let spawn = |index, parent, child| {
        claim(
            index,
            Fact::Relation {
                relation: Relation::Spawn {
                    parent: agent(parent),
                    child: agent(child),
                    operation: None,
                },
            },
        )
    };
    let result = reduce(&[spawn(1, "a", "b"), spawn(2, "b", "a"), spawn(3, "c", "b")]);
    assert!(result.relations.is_empty());
    assert!(result.objects.is_empty());
    assert!(result
        .diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::CyclicSpawn));
    assert!(result
        .diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::ConflictingOwner));
}

#[test]
fn a_late_append_is_retained_without_reopening_a_final_revision() {
    let message_ref = MessageRef::native(agent("root"), "message", vec!["m".into()]).unwrap();
    let rev = |key: &str| {
        MessageRevisionRef::native(agent("root"), "revision", vec![key.into()]).unwrap()
    };
    let revision = |index, key, form| {
        claim(
            index,
            Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: rev(key),
                    message: message_ref.clone(),
                    representation: Representation::Conversation,
                    form,
                    parts: vec![],
                }),
            },
        )
    };
    let values = vec![
        revision(1, "a", RevisionForm::Append { predecessor: None }),
        revision(2, "b", RevisionForm::Replacement { covers: None }),
        revision(3, "final", RevisionForm::Final { covers: None }),
        revision(
            4,
            "late",
            RevisionForm::Append {
                predecessor: Some(rev("final")),
            },
        ),
    ];
    let result = reduce(&values);
    let history = &result.messages[&message_ref];
    assert_eq!(history.revisions.len(), 4);
    assert_eq!(history.selected.known(), Some(&rev("final")));
    assert!(history.sealed);
    assert_eq!(history.late_segments, vec![rev("late")]);
    assert!(result
        .diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::LateSegment));
    let mut corrected = values;
    corrected.push(revision(
        5,
        "corrected",
        RevisionForm::Correction {
            replaces: rev("final"),
        },
    ));
    assert_eq!(
        reduce(&corrected).messages[&message_ref].selected.known(),
        Some(&rev("corrected"))
    );
}

#[test]
fn cumulative_measurements_require_the_same_counter_domain_and_known_baseline() {
    let sample = |seq, value| Measurement {
        model: None,
        measurement_ref: MeasurementRef::native(agent("root"), "usage", vec![format!("{seq}")])
            .unwrap(),
        subject: AnyObjectRef::Agent(agent("root")),
        metric: Metric::TotalTokens,
        unit: MeasurementUnit::Tokens,
        value: serde_json::Number::from(value),
        mode: MeasurementMode::Cumulative {
            counter_domain: Some("root-total".into()),
            reset_epoch: Some("first".into()),
            sample_position: position(seq),
        },
        aggregation_scope: AggregationScope::NamedSubject,
        components: vec![],
    };
    let a = sample(1, 100u64);
    let b = sample(2, 100);
    let c = sample(3, 130);
    let reset = sample(4, 5);
    assert_eq!(cumulative_delta(&a, &b), Some(0.into()));
    assert_eq!(cumulative_delta(&b, &c), Some(30.into()));
    assert_eq!(cumulative_delta(&c, &reset), None);
    let mut unrelated = c.clone();
    unrelated.subject = AnyObjectRef::Agent(agent("child"));
    assert_eq!(cumulative_delta(&b, &unrelated), None);
    let mut different_model = c.clone();
    different_model.model = Some(MeasurementModel {
        reported_name: "another-model".into(),
        canonical_name: None,
        provider: None,
    });
    assert_eq!(cumulative_delta(&b, &different_model), None);
    let mut unknown = a;
    unknown.mode = MeasurementMode::Gauge;
    assert_eq!(cumulative_delta(&unknown, &c), None);
}

#[test]
fn invalid_claim_batches_are_atomic_and_cannot_reuse_a_receipt_as_proof() {
    let one = declared_agent(1, "root");
    let mut two = declared_agent(2, "child");
    let available = evidence(std::slice::from_ref(&one));
    let mut reducer = Reducer::default();
    assert_eq!(
        reducer.apply(&[one.clone(), two.clone()], &available, 2),
        Err(ReductionError::MissingEvidence)
    );
    assert_eq!(reducer.claims().count(), 0);
    reducer
        .apply(std::slice::from_ref(&one), &available, 1)
        .unwrap();
    two.claim_ref = one.claim_ref.clone();
    two.evidence = one.evidence;
    assert_eq!(
        reducer.apply(&[two], &available, 1),
        Err(ReductionError::ClaimCollision)
    );
    assert_eq!(reducer.claims().count(), 1);
}

#[test]
fn retained_claim_snapshots_are_isolated_from_later_batches_and_external_edits() {
    let first = declared_agent(1, "root");
    let second = declared_agent(2, "child");
    let available = evidence(&[first.clone(), second.clone()]);
    let mut reducer = Reducer::default();
    reducer
        .apply(std::slice::from_ref(&first), &available, 1)
        .unwrap();
    let mut snapshot = reducer.claim_snapshot();
    reducer
        .apply(std::slice::from_ref(&second), &available, 1)
        .unwrap();
    assert_eq!(snapshot.values().collect::<Vec<_>>(), vec![&first]);
    assert_eq!(reducer.claim_snapshot().len(), 2);
    snapshot.remove(&first.claim_ref);
    snapshot.insert(second.claim_ref.clone(), second.clone());
    assert_eq!(reducer.claim_snapshot()[&first.claim_ref], first);
    assert_eq!(reducer.claim_snapshot()[&second.claim_ref], second);
}

#[test]
fn a_failing_batch_changes_neither_the_committed_claims_nor_shared_snapshots() {
    let first = declared_agent(1, "root");
    let second = declared_agent(2, "child");
    let missing = declared_agent(3, "unretained");
    let available = evidence(&[first.clone(), second.clone()]);
    let mut reducer = Reducer::default();
    reducer
        .apply(std::slice::from_ref(&first), &available, 1)
        .unwrap();
    let snapshot = reducer.claim_snapshot();
    assert_eq!(
        reducer.apply(&[second.clone(), missing], &available, 2),
        Err(ReductionError::MissingEvidence)
    );
    assert_eq!(reducer.claim_snapshot(), snapshot);
    let mut collision = first.clone();
    collision.rule_id = "different-proof".into();
    assert_eq!(
        reducer.apply(&[second, collision], &available, 2),
        Err(ReductionError::ClaimCollision)
    );
    assert_eq!(reducer.claim_snapshot(), snapshot);
    assert_eq!(snapshot.values().collect::<Vec<_>>(), vec![&first]);
}

#[test]
fn a_frozen_evidence_snapshot_rejects_later_proofs_and_replays_in_canonical_order() {
    let first = declared_agent(1, "root");
    let second = declared_agent(2, "child");
    let mut available = evidence(std::slice::from_ref(&first));
    let frozen = available.clone();
    available.extend(second.evidence.iter().cloned());
    let mut reducer = Reducer::default();
    assert_eq!(
        reducer.apply(std::slice::from_ref(&second), &frozen, 1),
        Err(ReductionError::MissingEvidence)
    );
    assert!(reducer.is_empty());
    reducer
        .apply(&[second.clone(), first.clone()], &available, 2)
        .unwrap();
    let claims = reducer.claim_snapshot();
    let mut replay = Reducer::default();
    let serialized = serde_json::to_vec(&available).unwrap();
    let restored: imbl::OrdSet<ObservationRef> = serde_json::from_slice(&serialized).unwrap();
    replay
        .apply(&[first.clone(), second], &restored, 2)
        .unwrap();
    assert_eq!(replay.claim_snapshot(), claims);
    assert_eq!(replay.resolve(), reducer.resolve());
    assert_eq!(frozen.iter().cloned().collect::<Vec<_>>(), first.evidence);
}

#[test]
fn evidenced_metadata_refines_unknown_fields_without_overwriting_known_authorship() {
    let reference = MessageRef::native(agent("root"), "message", vec!["stream-1".into()]).unwrap();
    let base = Message {
        message_ref: reference.clone(),
        author: Author::Unknown,
        protocol_role: Some(ProtocolRole::Assistant),
        purpose: MessagePurpose::Unclassified,
        production: None,
    };
    let known = Message {
        author: Author::Agent {
            agent: OwnerRef::Agent {
                agent: agent("root"),
            },
        },
        purpose: MessagePurpose::Answer,
        production: Some(ProductionScope {
            owner: OwnerRef::Agent {
                agent: agent("root"),
            },
            activation: Some(activation("root", "a1")),
        }),
        ..base.clone()
    };
    let a = claim(
        1,
        Fact::Declare {
            object: TraceObject::Message(base),
        },
    );
    let b = claim(
        2,
        Fact::Declare {
            object: TraceObject::Message(known.clone()),
        },
    );
    let result = reduce(&[a, b.clone()]);
    assert_eq!(
        result.objects[&AnyObjectRef::Message(reference.clone())]
            .declaration
            .known(),
        Some(&TraceObject::Message(known.clone()))
    );
    let wrong = claim(
        3,
        Fact::Declare {
            object: TraceObject::Message(Message {
                author: Author::Human {
                    subject: Some("person".into()),
                },
                ..known
            }),
        },
    );
    assert!(matches!(
        reduce(&[b, wrong]).objects[&AnyObjectRef::Message(reference)].declaration,
        Resolved::Conflicted { .. }
    ));
}

#[test]
fn conflicting_predecessors_cannot_silently_select_a_different_branch() {
    let mut claims = vec![
        history(1, "a", None),
        history(2, "b", None),
        history(3, "c", Some("a")),
    ];
    claims.push(claim(
        4,
        Fact::Relation {
            relation: Relation::HistoryPredecessor {
                predecessor: entry("b"),
                entry: entry("c"),
            },
        },
    ));
    let selection = SelectionRef::native(agent("root"), "selected", vec!["c".into()]).unwrap();
    claims.push(claim(
        5,
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent("root"),
                membership: SelectionMembership::EntryPath { head: entry("c") },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        },
    ));
    claims.push(claim(
        6,
        Fact::SelectHistory {
            agent: agent("root"),
            selection,
            selected_at: position(6),
        },
    ));
    let result = reduce(&claims);
    let view = &result.histories[&agent("root")];
    assert!(view.selected_entries.is_none());
    assert_eq!(view.archive_order.len(), 3);
    assert!(view.unresolved_entries.contains(&entry("c")));
}

#[test]
fn vendor_ordinal_extents_are_not_physical_lines_or_child_authorship() {
    let original = agent("root");
    let child = agent("child");
    let e = history(1, "parent-entry", None);
    let selection = SelectionRef::native(child.clone(), "selected", vec!["prefix".into()]).unwrap();
    let claims = vec![
        e,
        claim(
            2,
            Fact::HistoryPosition {
                entry: entry("parent-entry"),
                position: position(1),
                coordinate: HistoryCoordinate::VendorOrdinal {
                    domain_ref: source(),
                },
                value: 214,
                mapping_rule: "codex/native-history".into(),
            },
        ),
        claim(
            3,
            Fact::HistoryPosition {
                entry: entry("parent-entry"),
                position: position(1),
                coordinate: HistoryCoordinate::LocalRecordIndex,
                value: 1,
                mapping_rule: "codex/native-history".into(),
            },
        ),
        claim(
            4,
            Fact::Declare {
                object: TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selection.clone(),
                    agent: child.clone(),
                    content_scope: HistoryContentScope::AllObservedAtCut,
                    membership: SelectionMembership::SourceExtents {
                        extents: vec![SourceExtent {
                            source: source(),
                            generation: 0,
                            coordinate: HistoryCoordinate::VendorOrdinal {
                                domain_ref: source(),
                            },
                            start_inclusive: 214,
                            end_exclusive: 215,
                            mapping_rule: "codex/native-history".into(),
                            original_owner: Some(OwnerRef::Agent {
                                agent: original.clone(),
                            }),
                        }],
                        inherited_selections: vec![],
                    },
                    excluded_heads: vec![],
                }),
            },
        ),
        claim(
            5,
            Fact::SelectHistory {
                agent: child.clone(),
                selection,
                selected_at: position(5),
            },
        ),
        claim(
            6,
            Fact::HistoryMappingComplete {
                extent: SourceExtent {
                    source: source(),
                    generation: 0,
                    coordinate: HistoryCoordinate::VendorOrdinal {
                        domain_ref: source(),
                    },
                    start_inclusive: 214,
                    end_exclusive: 215,
                    mapping_rule: "codex/native-history".into(),
                    original_owner: Some(OwnerRef::Agent {
                        agent: original.clone(),
                    }),
                },
            },
        ),
    ];
    let result = reduce(&claims);
    assert_eq!(
        result.histories[&child].selected_entries,
        Some(imbl::vector![entry("parent-entry")])
    );
    assert!(result.histories[&child].archive_order.is_empty());
    let TraceObject::HistoryEntry(retained) = result.objects
        [&AnyObjectRef::HistoryEntry(entry("parent-entry"))]
        .declaration
        .known()
        .unwrap()
    else {
        panic!("history entry expected")
    };
    assert_eq!(retained.containing_agent, original);
}

#[test]
fn proved_reader_order_never_changes_native_history_predecessors() {
    let claims = vec![
        history(1, "a", None),
        history(2, "b", None),
        claim(
            3,
            Fact::Relation {
                relation: Relation::ReadingOrder {
                    before: entry("b"),
                    after: entry("a"),
                },
            },
        ),
    ];
    let result = reduce(&claims);
    let view = &result.histories[&agent("root")];
    assert_eq!(view.archive_order, imbl::vector![entry("b"), entry("a")]);
    assert_eq!(view.candidate_heads, imbl::vector![entry("a"), entry("b")]);
}

#[test]
fn conflicting_revision_bodies_never_become_a_known_final_message() {
    let message = MessageRef::native(agent("root"), "message", vec!["m".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent("root"), "revision", vec!["final".into()]).unwrap();
    let declaration = |index, digest| {
        claim(
            index,
            Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: revision.clone(),
                    message: message.clone(),
                    representation: Representation::Conversation,
                    form: RevisionForm::Final { covers: None },
                    parts: vec![ContentPart {
                        media_type: Some("text/plain".into()),
                        encoding: PayloadEncoding::Utf8,
                        part_ref: MessagePartRef::native(
                            agent("root"),
                            "part",
                            vec!["m".into(), "0".into()],
                        )
                        .unwrap(),
                        kind: ContentKind::Text,
                        payload: PayloadRef::bytes(digest).unwrap(),
                    }],
                }),
            },
        )
    };
    let result = reduce(&[
        declaration(1, "a".repeat(64)),
        declaration(2, "b".repeat(64)),
    ]);
    assert!(matches!(
        result.objects[&AnyObjectRef::MessageRevision(revision)].declaration,
        Resolved::Conflicted { .. }
    ));
    assert!(matches!(
        result.messages[&message].selected,
        Resolved::Unknown
    ));
}

#[test]
fn equivalent_invocations_share_outcome_evidence_without_losing_issued_addresses() {
    let operation =
        |key: &str| OperationRef::native(agent("root"), "tool", vec![key.into()]).unwrap();
    let declare = |index, key| {
        claim(
            index,
            Fact::Declare {
                object: TraceObject::Operation(Operation {
                    operation_ref: operation(key),
                    owner: OwnerRef::Agent {
                        agent: agent("root"),
                    },
                    activation: None,
                    requester: None,
                    category: OperationCategory::Tool {
                        namespace: Some("shell".into()),
                        name: "run".into(),
                    },
                    input_revision: None,
                    argument_state: ArgumentState::Sealed,
                }),
            },
        )
    };
    let a = AnyObjectRef::Operation(operation("a"));
    let b = AnyObjectRef::Operation(operation("b"));
    let mut claims = vec![
        declare(1, "a"),
        declare(2, "b"),
        claim(
            3,
            Fact::Equivalence {
                left: a.clone(),
                right: b.clone(),
                rule: "vendor-explicit-call-join".into(),
            },
        ),
        claim(
            4,
            Fact::Execution {
                execution: ExecutionRef::Operation(operation("b")),
                boundary: ExecutionBoundary::Outcome {
                    outcome: TerminalOutcome::Failed,
                    reason: None,
                    detail: None,
                },
            },
        ),
    ];
    let result = reduce(&claims);
    assert_eq!(
        result.execution(&a).unwrap().outcome.known(),
        Some(&TerminalOutcome::Failed)
    );
    assert_eq!(result.execution(&a), result.execution(&b));
    assert!(result.object(&a).is_some() && result.object(&b).is_some());
    assert_eq!(result.objects.len(), 1);
    claims.push(claim(
        5,
        Fact::Execution {
            execution: ExecutionRef::Operation(operation("a")),
            boundary: ExecutionBoundary::Outcome {
                outcome: TerminalOutcome::Succeeded,
                reason: None,
                detail: None,
            },
        },
    ));
    assert!(matches!(
        reduce(&claims).execution(&a).unwrap().outcome,
        Resolved::Conflicted { .. }
    ));
}
