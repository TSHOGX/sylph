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
