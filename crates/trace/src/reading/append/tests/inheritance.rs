use super::*;

#[test]
fn the_first_local_tail_cannot_keep_an_inherited_only_header() {
    let owner = AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:child").unwrap());
    let origin = AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:origin").unwrap());
    let mut reducer = Reducer::default();
    let evidence = ObservationRef {
        issuer: Issuer::Environment(owner.environment().clone()),
        producer_id: "inheritance".into(),
        producer_epoch: "one".into(),
        producer_seq: 1,
    };
    let mut sequence = 0;
    let mut apply = |reducer: &mut Reducer, facts: Vec<Fact>| {
        let claims: Vec<_> = facts
            .into_iter()
            .map(|fact| {
                sequence += 1;
                Claim {
                    claim_ref: ClaimRef::native(
                        owner.clone(),
                        "inheritance",
                        vec![sequence.to_string()],
                    )
                    .unwrap(),
                    fact,
                    evidence: vec![evidence.clone()],
                    rule_id: "fixture".into(),
                    mapper_build: "fixture".into(),
                    supersedes: vec![],
                    source_revision: None,
                }
            })
            .collect();
        reducer
            .apply(&claims, &imbl::ordset![evidence.clone()], 100)
            .unwrap();
        reducer.resolve_incremental(10000)
    };
    let entry =
        |index: usize| EntryRef::native(owner.clone(), "entry", vec![index.to_string()]).unwrap();
    let facts = |index: usize| {
        let message =
            MessageRef::native(owner.clone(), "message", vec![index.to_string()]).unwrap();
        let revision =
            MessageRevisionRef::native(owner.clone(), "revision", vec![index.to_string()]).unwrap();
        vec![
            Fact::Declare {
                object: TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry(index),
                    containing_agent: owner.clone(),
                    predecessor: index.checked_sub(1).map(entry),
                    original_owner: (index == 0).then(|| OwnerRef::Agent {
                        agent: origin.clone(),
                    }),
                }),
            },
            Fact::Declare {
                object: TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Unknown,
                    protocol_role: None,
                    purpose: MessagePurpose::Unclassified,
                    production: None,
                }),
            },
            Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: revision.clone(),
                    message,
                    representation: Representation::Conversation,
                    form: RevisionForm::Complete,
                    parts: vec![],
                }),
            },
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry(index),
                    SourcePosition {
                        source: SourceRef {
                            issuer: evidence.issuer.clone(),
                            instance: "history".into(),
                        },
                        generation: 0,
                        locator: SourceLocator::Frame {
                            ordinal: index as u64,
                        },
                    },
                    "fixture",
                    vec![HistoryContent::Message(revision)],
                ),
            },
        ]
    };
    let mut initial = facts(0);
    initial.push(Fact::Declare {
        object: TraceObject::Agent(AgentThread {
            agent_ref: owner.clone(),
            vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
            name: None,
            model: None,
        }),
    });
    let before = apply(&mut reducer, initial).reduction;
    assert!(project(&before, &PayloadPreviews::new(), 32).headers[&owner].inherited_context_only);
    let index = AppendIndex::build(before.clone(), 10000, &mut AppendWork::default()).unwrap();
    let after = apply(&mut reducer, facts(1));
    let proof = after
        .append
        .as_ref()
        .expect("this is a core independent append");
    assert!(project_independent_append(
        &index,
        proof,
        &PayloadPreviews::new(),
        32,
        10000,
        &mut AppendWork::default()
    )
    .is_none());
    assert!(
        !project(&after.reduction, &PayloadPreviews::new(), 32).headers[&owner]
            .inherited_context_only
    );
    let index =
        AppendIndex::build(after.reduction.clone(), 10000, &mut AppendWork::default()).unwrap();
    let ordinary = apply(&mut reducer, facts(2));
    assert!(project_independent_append(
        &index,
        ordinary.append.as_ref().unwrap(),
        &PayloadPreviews::new(),
        32,
        10000,
        &mut AppendWork::default()
    )
    .is_some());
}
