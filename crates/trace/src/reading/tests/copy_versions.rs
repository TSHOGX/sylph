use super::*;

#[test]
fn selected_copy_context_uses_only_the_selected_content_version() {
    let source_owner =
        AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:source").unwrap());
    let source = EntryRef::native(source_owner.clone(), "entry", vec!["source".into()]).unwrap();
    let copied = EntryRef::native(agent(), "entry", vec!["copy".into()]).unwrap();
    let selection = SelectionRef::native(agent(), "selection", vec!["version".into()]).unwrap();
    let mut facts = vec![
        Fact::Declare {
            object: declared_agent(agent()),
        },
        Fact::Declare {
            object: declared_agent(source_owner.clone()),
        },
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: source.clone(),
                containing_agent: source_owner.clone(),
                predecessor: None,
                original_owner: None,
            }),
        },
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: copied.clone(),
                containing_agent: agent(),
                predecessor: None,
                original_owner: Some(OwnerRef::Unknown),
            }),
        },
    ];
    for (entry, position) in [(&source, 1), (&copied, 2), (&copied, 3)] {
        facts.push(Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                capture(position).source_position,
                "versions",
                vec![HistoryContent::Opaque(capture(1).payload_ref)],
            ),
        });
    }
    facts.push(Fact::Relation {
        relation: Relation::ContextCopy {
            original: source,
            copied: copied.clone(),
            original_position: capture(1).source_position,
            copied_position: capture(2).source_position,
        },
    });
    facts.push(Fact::Declare {
        object: TraceObject::HistorySelection(HistorySelection {
            selection_ref: selection.clone(),
            agent: agent(),
            membership: SelectionMembership::EntrySet {
                entries: vec![copied.clone()],
            },
            content_scope: HistoryContentScope::ObservationSet {
                observations: vec![reader_claim_ref(5)],
            },
            excluded_heads: vec![],
        }),
    });
    let projection = facts_projection(facts, &PayloadPreviews::new());
    let archive = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    assert_eq!(
        archive[0].entry_context,
        Some(ReadEntryContext::Unresolved),
        "another observed version has no copy proof"
    );
    let selected = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: Some(selection),
    }];
    assert!(
        matches!(&selected[0].entry_context, Some(ReadEntryContext::Inherited { original_owner: Some(owner) }) if owner.address.agent() == source_owner)
    );
    assert_eq!(
        projection
            .copy_contexts
            .values()
            .filter(|proof| !proof.roots.is_empty())
            .count(),
        1
    );
}

#[test]
fn deeply_nested_copy_proofs_keep_linear_storage_and_constant_reader_metadata() {
    let mut measurements = Vec::new();
    for count in [100, 1000] {
        let owners: Vec<_> = (0..count)
            .map(|index| {
                AgentRef::Session(
                    sylph_protocol::SessionId::decode(&format!("env7:codex:copy-{index:04}"))
                        .unwrap(),
                )
            })
            .collect();
        let entries: Vec<_> = owners
            .iter()
            .map(|owner| EntryRef::native(owner.clone(), "entry", vec!["copy".into()]).unwrap())
            .collect();
        let mut facts = Vec::new();
        for (index, owner) in owners.iter().enumerate() {
            facts.push(Fact::Declare {
                object: declared_agent(owner.clone()),
            });
            facts.push(Fact::Declare {
                object: TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entries[index].clone(),
                    containing_agent: owner.clone(),
                    predecessor: None,
                    original_owner: None,
                }),
            });
            facts.push(Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entries[index].clone(),
                    capture(index as u64 + 1).source_position,
                    "deep-copy",
                    vec![HistoryContent::Opaque(capture(1).payload_ref)],
                ),
            });
            if index > 0 {
                facts.push(Fact::Relation {
                    relation: Relation::ContextCopy {
                        original: entries[index - 1].clone(),
                        copied: entries[index].clone(),
                        original_position: capture(index as u64).source_position,
                        copied_position: capture(index as u64 + 1).source_position,
                    },
                });
            }
        }
        let claims: Vec<_> = facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| Claim {
                claim_ref: reader_claim_ref(index),
                fact,
                evidence: vec![capture(1).observation_ref],
                rule_id: "deep-copy".into(),
                mapper_build: "fixture".into(),
                supersedes: vec![],
                source_revision: None,
            })
            .collect();
        let mut reducer = Reducer::default();
        for batch in claims.chunks(64) {
            reducer
                .apply(batch, &[capture(1).observation_ref].into_iter().collect(), 64)
                .unwrap();
        }
        let projection = project(&reducer.resolve(), &PayloadPreviews::new(), 32);
        let last = &projection.windows[&ReadingScope {
            agent: owners.last().unwrap().clone(),
            selection: None,
        }][0];
        assert!(
            matches!(&last.entry_context, Some(ReadEntryContext::Inherited { original_owner: Some(owner) }) if owner.address.agent() == owners[0])
        );
        let metadata_bytes = serde_json::to_vec(&last.entry_context).unwrap().len();
        let proof_bytes: usize = projection
            .copy_nodes
            .values()
            .map(|node| serde_json::to_vec(node).unwrap().len())
            .sum();
        let roots: usize = projection
            .copy_contexts
            .values()
            .map(|proof| proof.roots.len())
            .sum();
        assert_eq!(
            roots,
            count - 1,
            "reader roots must not embed complete ancestry paths"
        );
        measurements.push((metadata_bytes, proof_bytes));
    }
    assert_eq!(measurements[0].0, measurements[1].0);
    assert!(
        measurements[1].1 < measurements[0].1 * 12,
        "tenfold chain depth must not create quadratic proof storage: {measurements:?}"
    );
}
