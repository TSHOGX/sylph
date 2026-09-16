use super::*;
use crate::mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper};
use crate::reducer::Reducer;

fn agent() -> AgentRef {
    AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:reader").unwrap())
}
fn capture(index: u64) -> Capture {
    let issuer = Issuer::Environment(agent().environment().clone());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "fixture".into(),
            producer_epoch: "1".into(),
            producer_seq: index,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: "live".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: index },
        },
        ingestion_authorization_epoch: IngestionEpoch::new("epoch").unwrap(),
        declared_subject_refs: vec![agent()],
        captured_at: TimeEvidence {
            domain: "capture".into(),
            value: index as i64,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: TransportKind::LiveStdout,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}

#[test]
fn streamed_answer_is_one_reading_unit_and_retains_the_complete_final_body() {
    let frames = [
        serde_json::json!({"method":"thread/started","params":{"thread":{"id":"reader"}}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
        serde_json::json!({"method":"item/completed","params":{"threadId":"reader","turnId":"turn-a","item":{"type":"agentMessage","id":"message-a","text":"Complete answer","phase":"final_answer"}}}),
    ];
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut previews = BTreeMap::new();
    for (index, frame) in frames.iter().enumerate() {
        let capture = capture(index as u64 + 1);
        let raw = serde_json::to_vec(frame).unwrap();
        let output = crate::mapping::codex::CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        let evidence = output
            .claims
            .iter()
            .flat_map(|claim| claim.evidence.clone())
            .collect();
        reducer.apply(&output.claims, &evidence, 100).unwrap();
        state = output.state;
        for payload in output.payloads {
            previews.insert(
                payload.reference,
                PayloadPreview {
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    preview_limit: 2048,
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..PayloadPreview::default()
                },
            );
        }
    }
    let projection = project(&reducer.resolve(), &previews, 32);
    let window = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    let messages: Vec<_> = window
        .iter()
        .filter_map(|unit| match &unit.content {
            ReadingContent::Message { parts, purpose, .. } => Some((parts, purpose)),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(*messages[0].1, MessagePurpose::Answer);
    assert_eq!(messages[0].0[0].preview, "Complete answer");
}

#[test]
fn duplicate_deliveries_keep_distinct_reading_rows_without_inventing_history_entries() {
    let message = MessageRef::native(agent(), "message", vec!["submitted".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent(), "revision", vec!["submitted-1".into()]).unwrap();
    let mut facts = vec![
        Fact::Declare {
            object: TraceObject::Agent(AgentThread {
                agent_ref: agent(),
                vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                name: None,
                model: None,
            }),
        },
        Fact::Declare {
            object: TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Human {
                    subject: Some("writer".into()),
                },
                protocol_role: None,
                purpose: MessagePurpose::TaskInput,
                production: None,
            }),
        },
        Fact::Declare {
            object: TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Submitted,
                form: RevisionForm::Complete,
                parts: vec![],
            }),
        },
    ];
    for key in ["delivery-a", "delivery-b"] {
        facts.push(Fact::Declare {
            object: TraceObject::Delivery(Delivery {
                delivery_ref: DeliveryRef::native(agent(), "delivery", vec![key.into()]).unwrap(),
                sender: Author::Human {
                    subject: Some("writer".into()),
                },
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                message_revision: revision.clone(),
                route: DeliveryRoute::ServerCommand {
                    command_id: key.into(),
                },
            }),
        });
    }
    let claims: Vec<_> = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| Claim {
            claim_ref: ClaimRef::native(agent(), "claim", vec![index.to_string()]).unwrap(),
            fact,
            evidence: vec![capture(index as u64 + 1).observation_ref],
            rule_id: "fixture".into(),
            mapper_build: "1".into(),
            supersedes: vec![],
            source_revision: None,
        })
        .collect();
    let mut reducer = Reducer::default();
    reducer
        .apply(
            &claims,
            &claims
                .iter()
                .flat_map(|claim| claim.evidence.clone())
                .collect(),
            100,
        )
        .unwrap();
    let reduction = reducer.resolve();
    let projection = project(&reduction, &BTreeMap::new(), 32);
    let window = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    assert_eq!(window.len(), 2);
    assert_ne!(window[0].address, window[1].address);
    assert!(reduction.histories.is_empty());
    assert!(window.iter().all(|unit| unit.activation.is_none()));
}

fn objects_projection(objects: Vec<TraceObject>, relations: Vec<Relation>) -> ReadingProjection {
    let claims: Vec<_> = objects
        .into_iter()
        .map(|object| Fact::Declare { object })
        .chain(
            relations
                .into_iter()
                .map(|relation| Fact::Relation { relation }),
        )
        .enumerate()
        .map(|(index, fact)| Claim {
            claim_ref: ClaimRef::native(agent(), "reader-test-claim", vec![index.to_string()])
                .unwrap(),
            fact,
            evidence: vec![capture(index as u64 + 1).observation_ref],
            rule_id: "reader-test".into(),
            mapper_build: "1".into(),
            supersedes: vec![],
            source_revision: None,
        })
        .collect();
    let mut reducer = Reducer::default();
    reducer
        .apply(
            &claims,
            &claims
                .iter()
                .flat_map(|claim| claim.evidence.clone())
                .collect(),
            1000,
        )
        .unwrap();
    project(&reducer.resolve(), &BTreeMap::new(), 32)
}
fn declared_agent(reference: AgentRef) -> TraceObject {
    TraceObject::Agent(AgentThread {
        agent_ref: reference,
        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
        name: None,
        model: None,
    })
}
fn declared_operation(key: &str) -> Operation {
    Operation {
        operation_ref: OperationRef::native(agent(), "operation", vec![key.into()]).unwrap(),
        owner: OwnerRef::Agent { agent: agent() },
        activation: None,
        requester: None,
        category: OperationCategory::Wait,
        input_revision: None,
        argument_state: ArgumentState::Sealed,
    }
}

#[test]
fn selected_details_include_nested_work_without_reaching_discarded_siblings() {
    let included = declared_operation("selected");
    let nested = declared_operation("nested");
    let discarded = declared_operation("discarded");
    let entry = EntryRef::native(agent(), "entry", vec!["selected".into()]).unwrap();
    let selection = SelectionRef::native(agent(), "selection", vec!["branch".into()]).unwrap();
    let projection = objects_projection(
        vec![
            declared_agent(agent()),
            TraceObject::Operation(included.clone()),
            TraceObject::Operation(nested.clone()),
            TraceObject::Operation(discarded.clone()),
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent(),
                content: vec![HistoryContent::Operation(included.operation_ref.clone())],
                predecessor: None,
                original_owner: None,
            }),
            TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent(),
                membership: SelectionMembership::EntrySet {
                    entries: vec![entry],
                },
                excluded_heads: vec![],
            }),
        ],
        vec![Relation::Nesting {
            enclosing: included.operation_ref.clone(),
            nested: nested.operation_ref.clone(),
        }],
    );
    let selected = &projection.selected_objects[&ReadingScope {
        agent: agent(),
        selection: Some(selection),
    }];
    assert!(selected.contains(&AnyObjectRef::Operation(included.operation_ref)));
    assert!(selected.contains(&AnyObjectRef::Operation(nested.operation_ref)));
    assert!(!selected.contains(&AnyObjectRef::Operation(discarded.operation_ref)));
}

#[test]
fn inherited_content_keeps_original_authorship_without_a_recipient_activation() {
    let recipient =
        AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:child-reader").unwrap());
    let activation = ActivationRef::native(agent(), "activation", vec!["original".into()]).unwrap();
    let message = MessageRef::native(agent(), "message", vec!["original".into()]).unwrap();
    let revision =
        MessageRevisionRef::native(agent(), "revision", vec!["original".into()]).unwrap();
    let projection = objects_projection(
        vec![
            declared_agent(agent()),
            declared_agent(recipient.clone()),
            TraceObject::Activation(Activation {
                activation_ref: activation.clone(),
                owner: OwnerRef::Agent { agent: agent() },
            }),
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                protocol_role: None,
                purpose: MessagePurpose::Answer,
                production: Some(ProductionScope {
                    owner: OwnerRef::Agent { agent: agent() },
                    activation: Some(activation),
                }),
            }),
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![],
            }),
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: EntryRef::native(
                    recipient.clone(),
                    "inherited-entry",
                    vec!["copy".into()],
                )
                .unwrap(),
                containing_agent: recipient.clone(),
                content: vec![HistoryContent::Message(revision)],
                predecessor: None,
                original_owner: Some(OwnerRef::Agent { agent: agent() }),
            }),
        ],
        vec![],
    );
    let units = &projection.windows[&ReadingScope {
        agent: recipient,
        selection: None,
    }];
    assert_eq!(units.len(), 1);
    assert!(units[0].activation.is_none());
    let ReadingContent::Message {
        author: ReadAuthor::Agent {
            agent: Some(author),
            ..
        },
        original_owner: Some(original),
        ..
    } = &units[0].content
    else {
        panic!("authorship and inherited provenance missing")
    };
    assert_eq!(author.address.agent(), agent());
    assert_eq!(original.address.agent(), agent());
}

#[test]
fn an_unfinished_stream_retains_both_identical_append_occurrences_in_the_reader() {
    let frames = [
        serde_json::json!({"method":"thread/started","params":{"thread":{"id":"reader"}}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
    ];
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut previews = BTreeMap::new();
    for (index, frame) in frames.iter().enumerate() {
        let capture = capture(index as u64 + 1);
        let raw = serde_json::to_vec(frame).unwrap();
        let output = crate::mapping::codex::CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        let evidence = output
            .claims
            .iter()
            .flat_map(|claim| claim.evidence.clone())
            .collect();
        reducer.apply(&output.claims, &evidence, 100).unwrap();
        state = output.state;
        for payload in output.payloads {
            previews.insert(
                payload.reference,
                PayloadPreview {
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    preview_limit: 2048,
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..PayloadPreview::default()
                },
            );
        }
    }
    let projection = project(&reducer.resolve(), &previews, 32);
    let window = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    let messages: Vec<_> = window
        .iter()
        .filter_map(|unit| match &unit.content {
            ReadingContent::Message { parts, purpose, .. } => Some((parts, purpose)),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0[0].preview, "AA");
}
