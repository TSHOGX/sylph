use super::*;
use sylph_protocol::{AgentName, SessionId};

fn agent(name: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("support:codex:{name}")).unwrap())
}

fn claim(number: u64, fact: Fact) -> Claim {
    Claim {
        claim_ref: ClaimRef::native(agent("recipient"), "support", vec![number.to_string()])
            .unwrap(),
        fact,
        evidence: vec![ObservationRef {
            issuer: Issuer::Server("support-test".into()),
            producer_id: "retained-source".into(),
            producer_epoch: "one".into(),
            producer_seq: number,
        }],
        rule_id: "support-test".into(),
        mapper_build: "test".into(),
        supersedes: vec![],
        source_revision: None,
    }
}

fn declared(number: u64, object: TraceObject) -> Claim {
    claim(number, Fact::Declare { object })
}

fn declared_agent(number: u64, name: &str) -> Claim {
    declared(
        number,
        TraceObject::Agent(AgentThread {
            agent_ref: agent(name),
            vendor: AgentName::parse("codex").unwrap(),
            name: None,
            model: None,
        }),
    )
}

fn apply(reducer: &mut Reducer, claims: &[Claim]) -> sylph_trace::reducer::Resolution {
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect();
    reducer.apply(claims, &evidence, 1000).unwrap();
    reducer.resolve_incremental(10_000)
}

fn support(claims: &[Claim]) -> (SupportIndex, Arc<Reduction>, Reducer) {
    let mut reducer = Reducer::default();
    let resolution = apply(&mut reducer, claims);
    let (index, _, _) = SupportIndex::advance(
        None,
        &resolution.reduction,
        &reducer.claim_snapshot(),
        claims,
        &resolution.changes,
        false,
    );
    (index, resolution.reduction, reducer)
}

#[test]
fn execution_and_content_binding_evidence_are_support_without_becoming_declarations() {
    let activation =
        ActivationRef::native(agent("recipient"), "activation", vec!["one".into()]).unwrap();
    let entry = EntryRef::native(agent("recipient"), "entry", vec!["one".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent("recipient"), "revision", vec!["one".into()]).unwrap();
    let mut claims = vec![
        declared_agent(1, "recipient"),
        declared(
            2,
            TraceObject::Activation(Activation {
                activation_ref: activation.clone(),
                owner: OwnerRef::Agent {
                    agent: agent("recipient"),
                },
            }),
        ),
        claim(
            3,
            Fact::Execution {
                execution: ExecutionRef::Activation(activation.clone()),
                boundary: ExecutionBoundary::Started { time: None },
            },
        ),
        declared(
            4,
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent("recipient"),
                predecessor: None,
                original_owner: Some(OwnerRef::Agent {
                    agent: agent("private-origin"),
                }),
            }),
        ),
    ];
    let source = SourcePosition {
        source: SourceRef {
            issuer: Issuer::Server("support-test".into()),
            instance: "copy".into(),
        },
        generation: 0,
        locator: SourceLocator::Frame { ordinal: 1 },
    };
    claims.push(claim(
        5,
        Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                source,
                "recipient-copy",
                vec![HistoryContent::Message(revision.clone())],
            ),
        },
    ));
    let message = MessageRef::native(agent("recipient"), "message", vec!["copy".into()]).unwrap();
    claims.push(declared(
        6,
        TraceObject::Message(Message {
            message_ref: message.clone(),
            author: Author::Agent {
                agent: OwnerRef::Agent {
                    agent: agent("private-origin"),
                },
            },
            protocol_role: Some(ProtocolRole::Assistant),
            purpose: MessagePurpose::Answer,
            production: None,
        }),
    ));
    claims.push(declared(
        7,
        TraceObject::MessageRevision(MessageRevision {
            revision_ref: revision.clone(),
            message,
            representation: Representation::Conversation,
            form: RevisionForm::Complete,
            parts: vec![],
        }),
    ));
    let (index, reduction, _) = support(&claims);
    assert_eq!(
        index.support[&AnyObjectRef::Activation(activation.clone())],
        imbl::ordset![claims[1].claim_ref.clone(), claims[2].claim_ref.clone()]
    );
    assert_eq!(
        resolved_claims(
            &reduction
                .object(&AnyObjectRef::Activation(activation))
                .unwrap()
                .declaration
        ),
        imbl::ordset![claims[1].claim_ref.clone()]
    );
    assert_eq!(
        index.support[&AnyObjectRef::HistoryEntry(entry)],
        imbl::ordset![claims[3].claim_ref.clone(), claims[4].claim_ref.clone()]
    );
    assert!(
        !index
            .support
            .contains_key(&AnyObjectRef::Agent(agent("private-origin")))
    );
    assert_eq!(
        index.support[&AnyObjectRef::MessageRevision(revision)],
        imbl::ordset![claims[6].claim_ref.clone()],
        "A recipient's binding cannot become evidence about the original revision"
    );
}

#[test]
fn equivalence_evidence_follows_canonical_objects_and_disappears_when_the_join_is_withdrawn() {
    let message =
        |name: &str| MessageRef::native(agent("recipient"), "message", vec![name.into()]).unwrap();
    let object = |name: &str| {
        TraceObject::Message(Message {
            message_ref: message(name),
            author: Author::Unknown,
            protocol_role: None,
            purpose: MessagePurpose::Unclassified,
            production: None,
        })
    };
    let left = AnyObjectRef::Message(message("left"));
    let right = AnyObjectRef::Message(message("right"));
    let claims = vec![
        declared_agent(1, "recipient"),
        declared(2, object("left")),
        declared(3, object("right")),
        claim(
            4,
            Fact::Equivalence {
                left: left.clone(),
                right: right.clone(),
                rule: "retained native correlation".into(),
            },
        ),
    ];
    let (before, reduction, mut reducer) = support(&claims);
    let canonical = reduction.object(&left).unwrap().reference.clone();
    assert_eq!(canonical, reduction.object(&right).unwrap().reference);
    assert_eq!(
        before.support[&canonical],
        imbl::ordset![
            claims[1].claim_ref.clone(),
            claims[2].claim_ref.clone(),
            claims[3].claim_ref.clone()
        ]
    );
    let mut correction = claim(
        5,
        Fact::Equivalence {
            left: left.clone(),
            right: right.clone(),
            rule: String::new(),
        },
    );
    correction.supersedes.push(claims[3].claim_ref.clone());
    let resolution = apply(&mut reducer, std::slice::from_ref(&correction));
    assert_ne!(
        resolution.reduction.object(&left).unwrap().reference,
        resolution.reduction.object(&right).unwrap().reference
    );
    let (after, _, _) = SupportIndex::advance(
        Some(&before),
        &resolution.reduction,
        &reducer.claim_snapshot(),
        &[correction],
        &resolution.changes,
        false,
    );
    assert_eq!(
        after.support[&left],
        imbl::ordset![claims[1].claim_ref.clone()]
    );
    assert_eq!(
        after.support[&right],
        imbl::ordset![claims[2].claim_ref.clone()]
    );
}

#[test]
fn relation_evidence_is_checked_on_the_link_without_withholding_endpoint_headers() {
    let claims = vec![
        declared_agent(1, "parent"),
        declared_agent(2, "child"),
        claim(
            3,
            Fact::Relation {
                relation: Relation::Spawn {
                    parent: agent("parent"),
                    child: agent("child"),
                    operation: None,
                },
            },
        ),
    ];
    let (mut index, _, _) = support(&claims);
    let parent = AnyObjectRef::Agent(agent("parent"));
    let child = AnyObjectRef::Agent(agent("child"));
    assert_eq!(
        index.support[&parent],
        imbl::ordset![claims[0].claim_ref.clone()]
    );
    assert_eq!(
        index.support[&child],
        imbl::ordset![claims[1].claim_ref.clone()]
    );
    let epoch = IngestionEpoch::new("link-only").unwrap();
    index
        .claim_epochs
        .insert(claims[2].claim_ref.clone(), imbl::ordset![epoch.clone()]);
    assert_eq!(
        index.relation_dependencies(&parent, &child),
        imbl::ordset![epoch]
    );
}

#[test]
fn scoped_object_coverage_contributes_to_the_header_that_summarizes_it() {
    let activation =
        ActivationRef::native(agent("recipient"), "activation", vec!["covered".into()]).unwrap();
    let subject = AnyObjectRef::Activation(activation.clone());
    let claims = vec![
        declared_agent(1, "recipient"),
        declared(
            2,
            TraceObject::Activation(Activation {
                activation_ref: activation,
                owner: OwnerRef::Agent {
                    agent: agent("recipient"),
                },
            }),
        ),
        claim(
            3,
            Fact::Coverage {
                coverage: Coverage {
                    subject: subject.clone(),
                    source: SourceRef {
                        issuer: Issuer::Server("support-test".into()),
                        instance: "independent-coverage".into(),
                    },
                    dimension: CoverageDimension::Timing,
                    interval: CoverageInterval::Object {
                        object: subject.clone(),
                    },
                    state: CoverageState::Gap {
                        reason: "An independently retained timing gap".into(),
                    },
                },
            },
        ),
    ];
    let (index, reduction, _) = support(&claims);
    let projection = sylph_trace::reading::project(&reduction, &Default::default(), 32);
    assert_eq!(projection.headers[&agent("recipient")].coverage.len(), 1);
    assert_eq!(
        index.support[&AnyObjectRef::Agent(agent("recipient"))],
        imbl::ordset![claims[0].claim_ref.clone(), claims[2].claim_ref.clone()]
    );
    assert_eq!(
        index.support[&subject],
        imbl::ordset![claims[1].claim_ref.clone(), claims[2].claim_ref.clone()]
    );
}

#[test]
fn selected_source_completeness_retains_only_matching_witnesses_through_inherited_selections() {
    let source = SourceRef {
        issuer: Issuer::Server("support-test".into()),
        instance: "selected-source".into(),
    };
    let extent = SourceExtent {
        source,
        generation: 3,
        coordinate: HistoryCoordinate::LocalRecordIndex,
        start_inclusive: 0,
        end_exclusive: 10,
        mapping_rule: "selected/native".into(),
        original_owner: None,
    };
    let inner =
        SelectionRef::native(agent("recipient"), "selection", vec!["inner".into()]).unwrap();
    let outer =
        SelectionRef::native(agent("recipient"), "selection", vec!["outer".into()]).unwrap();
    let selection = |reference, extents, inherited_selections| {
        TraceObject::HistorySelection(HistorySelection {
            selection_ref: reference,
            agent: agent("recipient"),
            membership: SelectionMembership::SourceExtents {
                extents,
                inherited_selections,
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        })
    };
    let mut wrong_rule = extent.clone();
    wrong_rule.mapping_rule = "another/rule".into();
    let mut outside = extent.clone();
    outside.start_inclusive = 20;
    outside.end_exclusive = 30;
    let claims = vec![
        declared_agent(1, "recipient"),
        declared(2, selection(inner.clone(), vec![extent.clone()], vec![])),
        claim(3, Fact::HistoryMappingComplete { extent }),
        claim(4, Fact::HistoryMappingComplete { extent: wrong_rule }),
        claim(5, Fact::HistoryMappingComplete { extent: outside }),
        declared(6, selection(outer.clone(), vec![], vec![inner.clone()])),
    ];
    let (index, reduction, _) = support(&claims);
    assert_eq!(reduction.selection_memberships[&inner], Some(vec![]));
    assert_eq!(reduction.selection_memberships[&outer], Some(vec![]));
    assert_eq!(
        index.support[&AnyObjectRef::HistorySelection(inner)],
        imbl::ordset![claims[1].claim_ref.clone(), claims[2].claim_ref.clone()]
    );
    assert_eq!(
        index.support[&AnyObjectRef::HistorySelection(outer)],
        imbl::ordset![
            claims[1].claim_ref.clone(),
            claims[2].claim_ref.clone(),
            claims[5].claim_ref.clone()
        ]
    );
}

#[test]
fn proved_tail_support_matches_reconciliation_and_shares_untouched_buckets() {
    let previous_entry =
        EntryRef::native(agent("recipient"), "entry", vec!["previous".into()]).unwrap();
    let claims = vec![
        declared_agent(1, "recipient"),
        declared_agent(2, "unrelated"),
        declared(
            9,
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: previous_entry.clone(),
                containing_agent: agent("recipient"),
                predecessor: None,
                original_owner: None,
            }),
        ),
    ];
    let (before, _, mut reducer) = support(&claims);
    let message = MessageRef::native(agent("recipient"), "message", vec!["tail".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent("recipient"), "revision", vec!["tail".into()]).unwrap();
    let entry = EntryRef::native(agent("recipient"), "entry", vec!["tail".into()]).unwrap();
    let source = SourcePosition {
        source: SourceRef {
            issuer: Issuer::Server("support-test".into()),
            instance: "tail".into(),
        },
        generation: 0,
        locator: SourceLocator::Frame { ordinal: 1 },
    };
    let incoming = vec![
        declared(
            3,
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Unknown,
                protocol_role: Some(ProtocolRole::Assistant),
                purpose: MessagePurpose::Answer,
                production: None,
            }),
        ),
        declared(
            4,
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![],
            }),
        ),
        declared(
            5,
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent("recipient"),
                predecessor: Some(previous_entry),
                original_owner: None,
            }),
        ),
        claim(
            6,
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry,
                    source,
                    "tail",
                    vec![HistoryContent::Message(revision)],
                ),
            },
        ),
    ];
    let resolution = apply(&mut reducer, &incoming);
    assert!(
        resolution.append.is_some(),
        "This checks the production independent-tail proof"
    );
    let (incremental, affected, needed) = SupportIndex::advance(
        Some(&before),
        &resolution.reduction,
        &reducer.claim_snapshot(),
        &incoming,
        &resolution.changes,
        true,
    );
    let (full, _, _) = SupportIndex::advance(
        None,
        &resolution.reduction,
        &reducer.claim_snapshot(),
        &incoming,
        &resolution.changes,
        false,
    );
    assert_eq!(incremental.support, full.support);
    assert_eq!(incremental.direct, full.direct);
    assert_eq!(incremental.relations, full.relations);
    let unrelated = AnyObjectRef::Agent(agent("unrelated"));
    assert!(!affected.contains(&unrelated));
    assert!(!needed.contains(&claims[1].claim_ref));
    assert!(before.support[&unrelated].ptr_eq(&incremental.support[&unrelated]));
}
