use super::*;

fn original() -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:original").unwrap())
}
fn mixed_entry(owner: Option<OwnerRef>) -> Vec<ReadingUnit> {
    let entry = EntryRef::native(agent(), "entry", vec!["mixed".into()]).unwrap();
    let message = MessageRef::native(agent(), "message", vec!["mixed".into()]).unwrap();
    let revision = MessageRevisionRef::native(agent(), "revision", vec!["mixed".into()]).unwrap();
    let checkpoint = CheckpointRef::native(agent(), "checkpoint", vec!["mixed".into()]).unwrap();
    let operation = Operation {
        owner: OwnerRef::Unknown,
        ..declared_operation("mixed")
    };
    let projection = objects_projection(
        vec![
            declared_agent(agent()),
            declared_agent(original()),
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent(),
                predecessor: None,
                original_owner: owner,
            }),
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Agent {
                    agent: OwnerRef::Agent { agent: original() },
                },
                protocol_role: Some(ProtocolRole::Assistant),
                purpose: MessagePurpose::Answer,
                production: None,
            }),
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![],
            }),
            TraceObject::Operation(operation.clone()),
            TraceObject::ContextCheckpoint(ContextCheckpoint {
                checkpoint_ref: checkpoint.clone(),
                owner: OwnerRef::Unknown,
                basis: ContextBasis::Summary,
                content: ContextContent::Unknown {
                    reason: "Summary not retained".into(),
                },
                coverage: InclusionCoverage::Unknown {
                    reason: "Membership not recorded".into(),
                },
            }),
        ],
        vec![],
        vec![HistoryContentObservation::complete(
            entry,
            capture(1).source_position,
            "entry-context-fixture",
            vec![
                HistoryContent::Message(revision),
                HistoryContent::Operation(operation.operation_ref),
                HistoryContent::Checkpoint(checkpoint),
                HistoryContent::Opaque(capture(1).payload_ref),
            ],
        )],
    );
    projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }]
        .clone()
}

#[test]
fn inherited_entry_provenance_survives_every_readable_content_kind_without_local_execution() {
    let rows = mixed_entry(Some(OwnerRef::Agent { agent: original() }));
    assert_eq!(rows.len(), 4);
    for row in &rows {
        let Some(ReadEntryContext::Inherited {
            original_owner: Some(owner),
        }) = &row.entry_context
        else {
            panic!("entry provenance missing from {:?}", row.content)
        };
        assert_eq!(owner.address.agent(), original());
        assert!(row.activation.is_none());
    }
    assert!(matches!(rows[0].content, ReadingContent::Message { .. }));
    assert!(matches!(rows[1].content, ReadingContent::Work { .. }));
    assert!(matches!(
        rows[2].content,
        ReadingContent::ContextBoundary { .. }
    ));
    assert!(matches!(rows[3].content, ReadingContent::Unknown { .. }));
}

#[test]
fn unresolved_entry_provenance_is_visible_without_claiming_copied_membership() {
    for owner in [
        OwnerRef::Unknown,
        OwnerRef::Unresolved {
            scope: agent(),
            namespace: "entry-origin".into(),
            native_key: vec!["fork-source".into()],
            reason: "Copied membership has not been established".into(),
        },
    ] {
        let rows = mixed_entry(Some(owner));
        assert_eq!(rows.len(), 4);
        assert!(rows
            .iter()
            .all(|row| row.entry_context == Some(ReadEntryContext::Unresolved)));
        assert!(rows.iter().all(|row| row.activation.is_none()));
    }
}

#[test]
fn a_foreign_author_does_not_create_inherited_entry_provenance() {
    let rows = mixed_entry(None);
    assert!(rows.iter().all(|row| row.entry_context.is_none()));
    let ReadingContent::Message {
        author: ReadAuthor::Agent {
            agent: Some(author),
            ..
        },
        ..
    } = &rows[0].content
    else {
        panic!("recorded authorship missing")
    };
    assert_eq!(author.address.agent(), original());
}

#[test]
fn work_grouping_preserves_a_change_in_entry_provenance_within_one_activation() {
    let activation = ActivationRef::native(agent(), "activation", vec!["active".into()]).unwrap();
    let mut objects = vec![
        declared_agent(agent()),
        TraceObject::Activation(Activation {
            activation_ref: activation.clone(),
            owner: OwnerRef::Agent { agent: agent() },
        }),
    ];
    let mut observations = vec![];
    let mut previous = None;
    for index in 0..3 {
        let entry = EntryRef::native(agent(), "entry", vec![format!("group-{index}")]).unwrap();
        let operation = Operation {
            activation: Some(activation.clone()),
            ..declared_operation(&format!("group-{index}"))
        };
        objects.push(TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: entry.clone(),
            containing_agent: agent(),
            predecessor: previous,
            original_owner: if index == 0 {
                Some(OwnerRef::Unknown)
            } else {
                None
            },
        }));
        objects.push(TraceObject::Operation(operation.clone()));
        observations.push(HistoryContentObservation::complete(
            entry.clone(),
            capture(index + 1).source_position,
            "grouping-context",
            vec![HistoryContent::Operation(operation.operation_ref)],
        ));
        previous = Some(entry);
    }
    let projection = objects_projection(objects, vec![], observations);
    let rows = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    assert_eq!(rows.len(), 2, "grouping erased the context boundary");
    assert_eq!(rows[0].entry_context, Some(ReadEntryContext::Unresolved));
    assert!(rows[1].entry_context.is_none());
    let ReadingContent::Work { operations, .. } = &rows[1].content else {
        panic!("work summary missing")
    };
    assert_eq!(operations.value, Some(2));
}

#[test]
fn inherited_only_requires_positive_complete_history_and_no_local_work_in_any_branch() {
    let entry = EntryRef::native(agent(), "context-only", vec!["entry".into()]).unwrap();
    let message = MessageRef::native(agent(), "context-only", vec!["message".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent(), "context-only", vec!["revision".into()]).unwrap();
    let base = vec![
        Fact::Declare {
            object: declared_agent(agent()),
        },
        Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent(),
                predecessor: None,
                original_owner: Some(OwnerRef::Agent { agent: original() }),
            }),
        },
        Fact::Declare {
            object: TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Unknown,
                protocol_role: None,
                purpose: MessagePurpose::Context,
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
                entry,
                capture(1).source_position,
                "context-only",
                vec![HistoryContent::Message(revision)],
            ),
        },
    ];
    let positive = facts_projection(base.clone(), &PayloadPreviews::new());
    assert!(positive.headers[&agent()].inherited_context_only);
    assert!(matches!(
        positive.headers[&agent()].own_execution.state,
        ReadExecutionState::Unavailable
    ));
    // No origin Agent declaration is required to retain the child's context evidence.
    assert_eq!(positive.inherited_context_sources[&agent()].len(), 1);
    for owner in [
        None,
        Some(OwnerRef::Unknown),
        Some(OwnerRef::Agent { agent: agent() }),
    ] {
        let mut changed = base.clone();
        let Fact::Declare {
            object: TraceObject::HistoryEntry(entry),
        } = &mut changed[1]
        else {
            unreachable!()
        };
        entry.original_owner = owner;
        assert!(
            !facts_projection(changed, &PayloadPreviews::new()).headers[&agent()]
                .inherited_context_only
        );
    }
    let conflicted_entry =
        EntryRef::native(agent(), "context-only", vec!["conflicted".into()]).unwrap();
    let mut conflicted = base.clone();
    for owner in [
        Some(OwnerRef::Agent { agent: original() }),
        Some(OwnerRef::Agent { agent: agent() }),
    ] {
        conflicted.push(Fact::Declare {
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: conflicted_entry.clone(),
                containing_agent: agent(),
                predecessor: None,
                original_owner: owner,
            }),
        });
    }
    assert!(
        !facts_projection(conflicted, &PayloadPreviews::new()).headers[&agent()]
            .inherited_context_only,
        "a conflicted entry omitted from the readable archive cannot disappear from this classification"
    );
    let mut missing_predecessor = base.clone();
    let Fact::Declare {
        object: TraceObject::HistoryEntry(entry),
    } = &mut missing_predecessor[1]
    else {
        unreachable!()
    };
    entry.predecessor =
        Some(EntryRef::native(agent(), "context-only", vec!["missing".into()]).unwrap());
    assert!(
        !facts_projection(missing_predecessor, &PayloadPreviews::new()).headers[&agent()]
            .inherited_context_only,
        "a missing predecessor leaves part of the retained history unclassified"
    );
    let mut undeclared_entry = base.clone();
    let mut orphan = base.last().unwrap().clone();
    let Fact::HistoryContentObserved { observation } = &mut orphan else {
        unreachable!()
    };
    observation.entry =
        EntryRef::native(agent(), "context-only", vec!["undeclared".into()]).unwrap();
    undeclared_entry.push(orphan);
    assert!(
        !facts_projection(undeclared_entry, &PayloadPreviews::new()).headers[&agent()]
            .inherited_context_only,
        "observed content without its entry declaration cannot disappear from classification"
    );
    let mut incomplete = base.clone();
    incomplete.pop();
    assert!(
        !facts_projection(incomplete, &PayloadPreviews::new()).headers[&agent()]
            .inherited_context_only
    );
    for object in [
        TraceObject::Activation(Activation {
            activation_ref: ActivationRef::native(
                agent(),
                "context-only",
                vec!["activation".into()],
            )
            .unwrap(),
            owner: OwnerRef::Agent { agent: agent() },
        }),
        TraceObject::Operation(declared_operation("local-without-activation")),
        TraceObject::Operation(Operation {
            owner: OwnerRef::Unknown,
            ..declared_operation("uncertain-work")
        }),
        TraceObject::Message(Message {
            message_ref: MessageRef::native(agent(), "context-only", vec!["local".into()]).unwrap(),
            author: Author::Unknown,
            protocol_role: None,
            purpose: MessagePurpose::Unclassified,
            production: Some(ProductionScope {
                owner: OwnerRef::Agent { agent: agent() },
                activation: None,
            }),
        }),
        TraceObject::Delivery(Delivery {
            delivery_ref: DeliveryRef::native(agent(), "context-only", vec!["input".into()])
                .unwrap(),
            sender: Author::Unknown,
            recipient: Recipient::Agent {
                agent: OwnerRef::Agent { agent: agent() },
            },
            route: DeliveryRoute::ServerCommand {
                command_id: "input".into(),
            },
        }),
    ] {
        let mut with_work = base.clone();
        with_work.push(Fact::Declare { object });
        assert!(
            !facts_projection(with_work, &PayloadPreviews::new()).headers[&agent()]
                .inherited_context_only
        );
    }
    let mut unbound_execution = base.clone();
    unbound_execution.push(Fact::Execution {
        execution: ExecutionRef::Activation(
            ActivationRef::native(agent(), "context-only", vec!["unbound".into()]).unwrap(),
        ),
        boundary: ExecutionBoundary::Started { time: None },
    });
    assert!(
        !facts_projection(unbound_execution, &PayloadPreviews::new()).headers[&agent()]
            .inherited_context_only,
        "an execution with missing ownership cannot be classified as inherited-only"
    );
    assert!(
        !objects_projection(vec![declared_agent(agent())], vec![], vec![]).headers[&agent()]
            .inherited_context_only
    );
}

#[test]
fn context_copy_proof_labels_copied_entries_and_follows_one_nested_origin_but_conflicts_stay_unresolved(
) {
    let root = AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:root").unwrap());
    let middle = original();
    let child = agent();
    let entry = |owner: &AgentRef, key: &str| {
        EntryRef::native(owner.clone(), "copy-entry", vec![key.into()]).unwrap()
    };
    let root_entry = entry(&root, "root");
    let middle_entry = entry(&middle, "middle");
    let child_entry = entry(&child, "child");
    let mut objects = vec![
        declared_agent(root.clone()),
        declared_agent(middle.clone()),
        declared_agent(child.clone()),
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: root_entry.clone(),
            containing_agent: root.clone(),
            predecessor: None,
            original_owner: None,
        }),
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: middle_entry.clone(),
            containing_agent: middle.clone(),
            predecessor: None,
            original_owner: None,
        }),
        TraceObject::HistoryEntry(HistoryEntry {
            entry_ref: child_entry.clone(),
            containing_agent: child.clone(),
            predecessor: None,
            original_owner: None,
        }),
    ];
    let observations = [
        (&root, &root_entry, "root", 1),
        (&middle, &middle_entry, "middle", 2),
        (&child, &child_entry, "child", 3),
    ]
    .into_iter()
    .map(|(owner, entry, key, capture_index)| {
        let message = MessageRef::native(owner.clone(), "copy-message", vec![key.into()]).unwrap();
        let revision =
            MessageRevisionRef::native(owner.clone(), "copy-revision", vec![key.into()]).unwrap();
        objects.push(TraceObject::Message(Message {
            message_ref: message.clone(),
            author: Author::Unknown,
            protocol_role: Some(ProtocolRole::User),
            purpose: MessagePurpose::Context,
            production: None,
        }));
        objects.push(TraceObject::MessageRevision(MessageRevision {
            revision_ref: revision.clone(),
            message,
            representation: Representation::Conversation,
            form: RevisionForm::Complete,
            parts: vec![],
        }));
        HistoryContentObservation::complete(
            entry.clone(),
            capture(capture_index).source_position,
            "copy",
            vec![HistoryContent::Message(revision)],
        )
    })
    .collect::<Vec<_>>();
    let relations = vec![
        Relation::ContextCopy {
            original: root_entry.clone(),
            copied: middle_entry.clone(),
            original_position: capture(1).source_position.clone(),
            copied_position: capture(2).source_position.clone(),
        },
        Relation::ContextCopy {
            original: middle_entry.clone(),
            copied: child_entry.clone(),
            original_position: capture(2).source_position.clone(),
            copied_position: capture(3).source_position.clone(),
        },
    ];
    let projection = objects_projection(objects.clone(), relations.clone(), observations.clone());
    let rows = &projection.windows[&ReadingScope {
        agent: child.clone(),
        selection: None,
    }];
    assert!(
        matches!(&rows[0].entry_context, Some(ReadEntryContext::Inherited { original_owner: Some(link) }) if link.address.agent() == root)
    );
    assert!(
        matches!(&rows[0].content, ReadingContent::Message { revision_ref, .. }
        if revision_ref == &MessageRevisionRef::native(child.clone(), "copy-revision", vec!["child".into()]).unwrap())
    );
    let mut unknown_root = objects.clone();
    for object in &mut unknown_root {
        if let TraceObject::HistoryEntry(entry) = object {
            if entry.entry_ref == root_entry {
                entry.original_owner = Some(OwnerRef::Unknown);
            }
        }
    }
    let unknown = objects_projection(unknown_root, relations.clone(), observations.clone());
    assert_eq!(
        unknown.windows[&ReadingScope {
            agent: child.clone(),
            selection: None
        }][0]
            .entry_context,
        Some(ReadEntryContext::Inherited {
            original_owner: None
        }),
        "a copy proves inheritance but not an unresolved ultimate owner"
    );
    let mut versions = observations.clone();
    let mut other_version = observations[1].clone();
    other_version.position = capture(4).source_position;
    versions.push(other_version);
    let mut mismatched_chain = relations.clone();
    if let Relation::ContextCopy {
        original_position, ..
    } = &mut mismatched_chain[1]
    {
        *original_position = capture(4).source_position;
    }
    let mismatched_chain = objects_projection(objects.clone(), mismatched_chain, versions);
    assert_eq!(
        mismatched_chain.windows[&ReadingScope {
            agent: child.clone(),
            selection: None
        }][0]
            .entry_context,
        Some(ReadEntryContext::Inherited {
            original_owner: None
        }),
        "the middle entry's other version cannot bridge to the root's copy proof"
    );
    let mut cyclic = relations.clone();
    cyclic.push(Relation::ContextCopy {
        original: child_entry.clone(),
        copied: root_entry.clone(),
        original_position: capture(3).source_position,
        copied_position: capture(1).source_position,
    });
    let cyclic = objects_projection(objects.clone(), cyclic, observations.clone());
    assert_eq!(
        cyclic.windows[&ReadingScope {
            agent: child.clone(),
            selection: None
        }][0]
            .entry_context,
        Some(ReadEntryContext::Unresolved)
    );
    let ambiguous = objects_projection(
        objects.clone(),
        vec![
            Relation::ContextCopy {
                original: root_entry.clone(),
                copied: child_entry.clone(),
                original_position: capture(1).source_position.clone(),
                copied_position: capture(3).source_position.clone(),
            },
            Relation::ContextCopy {
                original: middle_entry,
                copied: child_entry.clone(),
                original_position: capture(2).source_position,
                copied_position: capture(3).source_position,
            },
        ],
        observations.clone(),
    );
    assert_eq!(
        ambiguous.windows[&ReadingScope {
            agent: child.clone(),
            selection: None
        }][0]
            .entry_context,
        Some(ReadEntryContext::Unresolved)
    );
    let mismatched = objects_projection(
        objects,
        vec![Relation::ContextCopy {
            original: root_entry,
            copied: child_entry,
            original_position: capture(99).source_position,
            copied_position: capture(3).source_position,
        }],
        observations,
    );
    assert_eq!(
        mismatched.windows[&ReadingScope {
            agent: child,
            selection: None
        }][0]
            .entry_context,
        Some(ReadEntryContext::Unresolved),
        "a copy relation must not join an entry ID to a different source version"
    );
}
