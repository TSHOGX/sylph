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
    let mut previews = PayloadPreviews::new();
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
                Arc::new(PayloadPreview {
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    preview_limit: 2048,
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..PayloadPreview::default()
                }),
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
        let delivery = DeliveryRef::native(agent(), "delivery", vec![key.into()]).unwrap();
        facts.push(Fact::Declare {
            object: TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender: Author::Human {
                    subject: Some("writer".into()),
                },
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                route: DeliveryRoute::ServerCommand {
                    command_id: key.into(),
                },
            }),
        });
        facts.push(Fact::DeliveryRevisionObserved {
            delivery,
            revision: revision.clone(),
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
    let projection = project(&reduction, &PayloadPreviews::new(), 32);
    let window = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    assert_eq!(window.len(), 2);
    assert_ne!(window[0].address, window[1].address);
    assert!(reduction.histories.is_empty());
    assert!(window.iter().all(|unit| unit.activation.is_none()));
}

fn objects_projection(
    objects: Vec<TraceObject>,
    relations: Vec<Relation>,
    observations: Vec<HistoryContentObservation>,
) -> ReadingProjection {
    let facts = objects
        .into_iter()
        .map(|object| Fact::Declare { object })
        .chain(
            relations
                .into_iter()
                .map(|relation| Fact::Relation { relation }),
        )
        .chain(
            observations
                .into_iter()
                .map(|observation| Fact::HistoryContentObserved { observation }),
        )
        .collect();
    facts_projection(facts, &PayloadPreviews::new())
}

fn reader_claim_ref(index: usize) -> ClaimRef {
    ClaimRef::native(agent(), "reader-test-claim", vec![index.to_string()]).unwrap()
}

fn facts_projection(facts: Vec<Fact>, payloads: &PayloadPreviews) -> ReadingProjection {
    let claims: Vec<_> = facts
        .into_iter()
        .enumerate()
        .map(|(index, fact)| Claim {
            claim_ref: reader_claim_ref(index),
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
    project(&reducer.resolve(), payloads, 32)
}
fn declared_agent(reference: AgentRef) -> TraceObject {
    TraceObject::Agent(AgentThread {
        agent_ref: reference,
        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
        name: None,
        model: None,
    })
}

#[test]
fn activation_catalogues_require_own_proven_execution_even_without_history() {
    let child = AgentRef::Session(sylph_protocol::SessionId::decode("env7:codex:child").unwrap());
    let own = ActivationRef::native(agent(), "activation", vec!["own".into()]).unwrap();
    let child_activation =
        ActivationRef::native(child.clone(), "activation", vec!["child".into()]).unwrap();
    let unknown = ActivationRef::native(agent(), "activation", vec!["unresolved".into()]).unwrap();
    let projection = objects_projection(
        vec![
            declared_agent(agent()),
            declared_agent(child.clone()),
            TraceObject::Activation(Activation {
                activation_ref: own.clone(),
                owner: OwnerRef::Agent { agent: agent() },
            }),
            TraceObject::Activation(Activation {
                activation_ref: child_activation.clone(),
                owner: OwnerRef::Agent {
                    agent: child.clone(),
                },
            }),
            TraceObject::Activation(Activation {
                activation_ref: unknown.clone(),
                owner: OwnerRef::Unresolved {
                    scope: agent(),
                    namespace: "native-owner".into(),
                    native_key: vec!["missing".into()],
                    reason: "Owner is not established".into(),
                },
            }),
        ],
        vec![Relation::Spawn {
            parent: agent(),
            child: child.clone(),
            operation: None,
        }],
        vec![],
    );
    assert!(projection.windows.values().all(Vec::is_empty));
    let parent_rows = &projection.activations[&agent()];
    assert_eq!(parent_rows.len(), 1);
    assert_eq!(
        parent_rows[0].reference,
        Some(TraceAddress::of(&AnyObjectRef::Activation(own)))
    );
    assert_eq!(parent_rows[0].state, ReadExecutionState::Unavailable);
    let child_rows = &projection.activations[&child];
    assert_eq!(child_rows.len(), 1);
    assert_eq!(
        child_rows[0].reference,
        Some(TraceAddress::of(&AnyObjectRef::Activation(
            child_activation
        )))
    );
    assert!(
        projection
            .objects
            .contains_key(&AnyObjectRef::Activation(unknown)),
        "unresolved execution stays inspectable outside an invented owner catalogue"
    );
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
                predecessor: None,
                original_owner: None,
            }),
            TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent(),
                membership: SelectionMembership::EntrySet {
                    entries: vec![entry.clone()],
                },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        ],
        vec![Relation::Nesting {
            enclosing: included.operation_ref.clone(),
            nested: nested.operation_ref.clone(),
        }],
        vec![HistoryContentObservation::complete(
            entry,
            capture(1).source_position,
            "reader-test",
            vec![HistoryContent::Operation(included.operation_ref.clone())],
        )],
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
    let entry =
        EntryRef::native(recipient.clone(), "inherited-entry", vec!["copy".into()]).unwrap();
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
                entry_ref: entry.clone(),
                containing_agent: recipient.clone(),
                predecessor: None,
                original_owner: Some(OwnerRef::Agent { agent: agent() }),
            }),
        ],
        vec![],
        vec![HistoryContentObservation::complete(
            entry,
            capture(1).source_position,
            "reader-test",
            vec![HistoryContent::Message(revision)],
        )],
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

fn observed_content_fixture(
    payloads: [&PayloadRef; 2],
) -> (Vec<Fact>, EntryRef, [MessageRevisionRef; 2]) {
    let entry = EntryRef::native(agent(), "entry", vec!["shared".into()]).unwrap();
    let message = MessageRef::native(agent(), "message", vec!["shared".into()]).unwrap();
    let revisions = ["native", "snapshot"]
        .map(|name| MessageRevisionRef::native(agent(), "revision", vec![name.into()]).unwrap());
    let mut facts = vec![
        Fact::Declare {
            object: declared_agent(agent()),
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
            object: TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: agent(),
                predecessor: None,
                original_owner: None,
            }),
        },
    ];
    for (index, (reference, payload)) in revisions.iter().zip(payloads).enumerate() {
        facts.push(Fact::Declare {
            object: TraceObject::MessageRevision(MessageRevision {
                revision_ref: reference.clone(),
                message: message.clone(),
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(agent(), "part", vec![index.to_string()])
                        .unwrap(),
                    kind: ContentKind::Text,
                    payload: payload.clone(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
        });
        facts.push(Fact::HistoryContentObserved {
            observation: HistoryContentObservation::complete(
                entry.clone(),
                capture(index as u64 + 1).source_position,
                "reader-test",
                vec![HistoryContent::Message(reference.clone())],
            ),
        });
    }
    (facts, entry, revisions)
}

fn text_preview(value: &str) -> Arc<PayloadPreview> {
    Arc::new(PayloadPreview {
        preview: value.into(),
        preview_limit: 2048,
        total_bytes: Some(value.len() as u64),
        availability: Some(ReadAvailability::Available),
        ..PayloadPreview::default()
    })
}

#[test]
fn composing_previews_shares_source_metadata_without_mutating_the_committed_snapshot() {
    let first = PayloadRef::bytes("a".repeat(64)).unwrap();
    let second = PayloadRef::bytes("b".repeat(64)).unwrap();
    let (facts, _, revisions) = observed_content_fixture([&first, &second]);
    let part = MessagePartRef::native(agent(), "part", vec!["stream".into()]).unwrap();
    let mut reduction = Reduction::default();
    for fact in facts {
        let Fact::Declare {
            object: TraceObject::MessageRevision(mut revision),
        } = fact
        else {
            continue;
        };
        revision.form = RevisionForm::Append {
            predecessor: (revision.revision_ref == revisions[1]).then(|| revisions[0].clone()),
        };
        revision.parts[0].part_ref = part.clone();
        let object = TraceObject::MessageRevision(revision);
        let reference = object.object_ref();
        reduction.objects.insert(
            reference.clone(),
            crate::reducer::ResolvedObject {
                reference,
                declaration: Resolved::Known {
                    value: object,
                    claims: vec![],
                },
            },
        );
    }
    let committed = PayloadPreviews::from_iter([
        (first, text_preview("first")),
        (second, text_preview("second")),
    ]);
    let snapshot = || {
        serde_json::to_value(
            committed
                .iter()
                .map(|(reference, preview)| (reference, preview.as_ref()))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let before = snapshot();
    let mut candidate = committed.clone();
    let composed = compose_revisions(
        &reduction,
        &BTreeSet::from([revisions[1].clone()]),
        &mut candidate,
    );
    assert_eq!(composed.payloads.len(), 1);
    let composed_ref = composed.payloads.keys().next().unwrap();
    assert_eq!(candidate[composed_ref].preview, "firstsecond");
    assert!(!committed.contains_key(composed_ref));
    assert_eq!(snapshot(), before);
    for (reference, preview) in &committed {
        assert!(Arc::ptr_eq(preview, &candidate[reference]));
    }
}

#[test]
fn selected_entry_body_and_detail_reachability_exclude_other_source_observations() {
    let first = PayloadRef::bytes("a".repeat(64)).unwrap();
    let second = PayloadRef::bytes("b".repeat(64)).unwrap();
    let (mut facts, entry, revisions) = observed_content_fixture([&first, &second]);
    let selection =
        SelectionRef::native(agent(), "selection", vec!["native-observation".into()]).unwrap();
    facts.push(Fact::Declare {
        object: TraceObject::HistorySelection(HistorySelection {
            selection_ref: selection.clone(),
            agent: agent(),
            membership: SelectionMembership::EntrySet {
                entries: vec![entry.clone()],
            },
            content_scope: HistoryContentScope::ObservationSet {
                observations: vec![reader_claim_ref(4)],
            },
            excluded_heads: vec![],
        }),
    });
    let delivery = DeliveryRef::native(agent(), "delivery", vec!["other-source".into()]).unwrap();
    facts.push(Fact::Declare {
        object: TraceObject::Delivery(Delivery {
            delivery_ref: delivery.clone(),
            sender: Author::Unknown,
            recipient: Recipient::Unknown,
            route: DeliveryRoute::NativeHistory,
        }),
    });
    facts.push(Fact::DeliveryRevisionObserved {
        delivery: delivery.clone(),
        revision: revisions[1].clone(),
    });
    facts.push(Fact::DeliveryStage {
        delivery: delivery.clone(),
        stage: DeliveryStage::Admitted {
            entry: Some(entry.clone()),
        },
    });
    let projection = facts_projection(
        facts,
        &PayloadPreviews::from_iter([
            (first, text_preview("selected source")),
            (second, text_preview("unselected source")),
        ]),
    );
    let scope = ReadingScope {
        agent: agent(),
        selection: Some(selection.clone()),
    };
    let rows = &projection.windows[&scope];
    assert_eq!(rows.len(), 1);
    let ReadingContent::Message {
        revision_ref,
        parts,
        ..
    } = &rows[0].content
    else {
        panic!("selected content was lost");
    };
    assert_eq!(revision_ref, &revisions[0]);
    assert_eq!(parts[0].preview, "selected source");
    assert_eq!(rows[0].anchor.object(), AnyObjectRef::HistoryEntry(entry));
    let reachable = &projection.selected_objects[&scope];
    assert!(reachable.contains(&AnyObjectRef::MessageRevision(revisions[0].clone())));
    assert!(!reachable.contains(&AnyObjectRef::MessageRevision(revisions[1].clone())));
    assert!(!reachable.contains(&AnyObjectRef::Delivery(delivery)));
}

#[test]
fn equal_complete_observations_render_once_but_different_content_retains_the_entry_diagnostic() {
    let first = PayloadRef::bytes("a".repeat(64)).unwrap();
    let second = PayloadRef::bytes("b".repeat(64)).unwrap();
    for (other, expected_message) in [(&first, true), (&second, false)] {
        let (facts, entry, _) = observed_content_fixture([&first, other]);
        let projection = facts_projection(
            facts,
            &PayloadPreviews::from_iter([
                (first.clone(), text_preview("first")),
                (second.clone(), text_preview("different")),
            ]),
        );
        let rows = &projection.windows[&ReadingScope {
            agent: agent(),
            selection: None,
        }];
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].anchor.object(), AnyObjectRef::HistoryEntry(entry));
        assert_eq!(
            matches!(rows[0].content, ReadingContent::Message { .. }),
            expected_message
        );
        if !expected_message {
            assert!(matches!(rows[0].content, ReadingContent::Unknown { .. }));
        }
    }
}

#[test]
fn missing_content_is_visible_while_an_observed_empty_entry_has_no_fabricated_slot() {
    let entry = EntryRef::native(agent(), "entry", vec!["empty".into()]).unwrap();
    for complete in [false, true] {
        let projection = objects_projection(
            vec![
                declared_agent(agent()),
                TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry.clone(),
                    containing_agent: agent(),
                    predecessor: None,
                    original_owner: None,
                }),
            ],
            vec![],
            vec![HistoryContentObservation {
                entry: entry.clone(),
                position: capture(1).source_position,
                mapping_rule: "reader-test".into(),
                slots: vec![],
                complete,
            }],
        );
        let rows = &projection.windows[&ReadingScope {
            agent: agent(),
            selection: None,
        }];
        if complete {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0].content, ReadingContent::Unknown { .. }));
            assert!(rows[0].address.ends_with(":unobserved-content"));
        }
    }
}

#[test]
fn repeated_native_admission_observations_render_one_delivery_occurrence() {
    let payload = PayloadRef::bytes("a".repeat(64)).unwrap();
    let (mut facts, entry, revisions) = observed_content_fixture([&payload, &payload]);
    let delivery =
        DeliveryRef::native(agent(), "native-admission", vec!["same-entry".into()]).unwrap();
    facts.push(Fact::Declare {
        object: TraceObject::Delivery(Delivery {
            delivery_ref: delivery.clone(),
            sender: Author::Unknown,
            recipient: Recipient::Unknown,
            route: DeliveryRoute::NativeHistory,
        }),
    });
    for revision in &revisions {
        facts.push(Fact::DeliveryRevisionObserved {
            delivery: delivery.clone(),
            revision: revision.clone(),
        });
        facts.push(Fact::DeliveryStage {
            delivery: delivery.clone(),
            stage: DeliveryStage::Admitted {
                entry: Some(entry.clone()),
            },
        });
    }
    let projection = facts_projection(
        facts,
        &PayloadPreviews::from_iter([(payload, text_preview("one observed delivery"))]),
    );
    let rows = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: None,
    }];
    assert_eq!(rows.len(), 1);
    let ReadingContent::Message {
        parts,
        delivery: Some(observed),
        ..
    } = &rows[0].content
    else {
        panic!("native admission was lost");
    };
    assert_eq!(parts[0].preview, "one observed delivery");
    assert_eq!(
        observed.reference.object(),
        AnyObjectRef::Delivery(delivery)
    );
}

#[test]
fn message_corrections_do_not_rewrite_submitted_delivery_observations() {
    let first = PayloadRef::bytes("a".repeat(64)).unwrap();
    let second = PayloadRef::bytes("b".repeat(64)).unwrap();
    for conflicting_delivery in [false, true] {
        let (mut facts, _, revisions) = observed_content_fixture([&first, &second]);
        for fact in &mut facts {
            if let Fact::Declare {
                object: TraceObject::MessageRevision(revision),
            } = fact
            {
                if revision.revision_ref == revisions[1] {
                    revision.form = RevisionForm::Correction {
                        replaces: revisions[0].clone(),
                    };
                }
            }
        }
        let delivery =
            DeliveryRef::native(agent(), "submitted", vec!["one-attempt".into()]).unwrap();
        facts.push(Fact::Declare {
            object: TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender: Author::Unknown,
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                route: DeliveryRoute::ServerCommand {
                    command_id: "command".into(),
                },
            }),
        });
        facts.push(Fact::DeliveryRevisionObserved {
            delivery: delivery.clone(),
            revision: revisions[0].clone(),
        });
        if conflicting_delivery {
            facts.push(Fact::DeliveryRevisionObserved {
                delivery: delivery.clone(),
                revision: revisions[1].clone(),
            });
        }
        let projection = facts_projection(
            facts,
            &PayloadPreviews::from_iter([
                (first.clone(), text_preview("dispatched")),
                (second.clone(), text_preview("corrected message")),
            ]),
        );
        let rows = &projection.windows[&ReadingScope {
            agent: agent(),
            selection: None,
        }];
        let row = rows
            .iter()
            .find(|row| row.anchor.object() == AnyObjectRef::Delivery(delivery.clone()))
            .expect("submitted delivery must remain visible");
        if conflicting_delivery {
            assert!(matches!(row.content, ReadingContent::Unknown { .. }));
        } else {
            let ReadingContent::Message {
                parts,
                revision_ref,
                ..
            } = &row.content
            else {
                panic!("exact submitted content unavailable");
            };
            assert_eq!(revision_ref, &revisions[0]);
            assert_eq!(parts[0].preview, "dispatched");
        }
    }
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
    let mut previews = PayloadPreviews::new();
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
                Arc::new(PayloadPreview {
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    preview_limit: 2048,
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..PayloadPreview::default()
                }),
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

#[test]
fn a_selected_stream_prefix_assembles_its_latest_included_revision_after_a_later_final() {
    let frames = [
        serde_json::json!({"method":"thread/started","params":{"thread":{"id":"reader"}}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
        serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"reader","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
        serde_json::json!({"method":"item/completed","params":{"threadId":"reader","turnId":"turn-a","item":{"type":"agentMessage","id":"message-a","text":"Later final"}}}),
    ];
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut previews = PayloadPreviews::new();
    let mut prefix = vec![];
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
        if index < 3 {
            prefix.extend(output.claims.iter().filter_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::HistoryEntry(entry),
                } => Some(entry.entry_ref.clone()),
                _ => None,
            }));
        }
        reducer
            .apply(
                &output.claims,
                &output
                    .claims
                    .iter()
                    .flat_map(|claim| claim.evidence.clone())
                    .collect(),
                100,
            )
            .unwrap();
        state = output.state;
        for payload in output.payloads {
            previews.insert(
                payload.reference,
                Arc::new(PayloadPreview {
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    preview_limit: 128,
                    ..PayloadPreview::default()
                }),
            );
        }
    }
    let selection = SelectionRef::native(agent(), "selection", vec!["prefix".into()]).unwrap();
    let capture = capture(5);
    let claim = crate::mapping::claim(
        &MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: b"{}",
            state: &state,
            archive: &ArchiveContext::default(),
        },
        &agent(),
        "fixture-selection",
        "test",
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent(),
                membership: SelectionMembership::EntrySet { entries: prefix },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        },
    );
    reducer
        .apply(
            std::slice::from_ref(&claim),
            &claim.evidence.iter().cloned().collect(),
            100,
        )
        .unwrap();
    let projection = project(&reducer.resolve(), &previews, 32);
    let units = &projection.windows[&ReadingScope {
        agent: agent(),
        selection: Some(selection),
    }];
    assert_eq!(units.len(), 1);
    let ReadingContent::Message { parts, .. } = &units[0].content else {
        panic!("selected answer unavailable")
    };
    assert_eq!(parts[0].preview, "AA");
    assert!(projection
        .composed_payloads
        .contains_key(&parts[0].payload.payload_ref));
}

#[test]
fn a_selected_admitted_representation_exposes_the_related_submission_without_selecting_other_source_bytes()
 {
    let recorded = PayloadRef::bytes("a".repeat(64)).unwrap();
    let excluded = PayloadRef::bytes("b".repeat(64)).unwrap();
    let submitted_payload = PayloadRef::bytes("c".repeat(64)).unwrap();
    let (mut facts, entry, revisions) = observed_content_fixture([&recorded, &excluded]);
    let submitted_message =
        MessageRef::native(agent(), "submitted", vec!["message".into()]).unwrap();
    let submitted =
        MessageRevisionRef::native(agent(), "submitted", vec!["revision".into()]).unwrap();
    let delivery = DeliveryRef::native(agent(), "submitted", vec!["delivery".into()]).unwrap();
    let selection = SelectionRef::native(agent(), "selection", vec!["recorded".into()]).unwrap();
    facts.extend([
        Fact::Declare {
            object: TraceObject::Message(Message {
                message_ref: submitted_message.clone(),
                author: Author::Human {
                    subject: Some("person".into()),
                },
                protocol_role: None,
                purpose: MessagePurpose::TaskInput,
                production: None,
            }),
        },
        Fact::Declare {
            object: TraceObject::MessageRevision(MessageRevision {
                revision_ref: submitted.clone(),
                message: submitted_message.clone(),
                representation: Representation::Submitted,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(agent(), "submitted", vec!["part".into()])
                        .unwrap(),
                    kind: ContentKind::Text,
                    payload: submitted_payload.clone(),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
        },
        Fact::Declare {
            object: TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender: Author::Human {
                    subject: Some("person".into()),
                },
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent { agent: agent() },
                },
                route: DeliveryRoute::ServerCommand {
                    command_id: "command".into(),
                },
            }),
        },
        Fact::DeliveryRevisionObserved {
            delivery: delivery.clone(),
            revision: submitted.clone(),
        },
        Fact::DeliveryStage {
            delivery,
            stage: DeliveryStage::Admitted {
                entry: Some(entry.clone()),
            },
        },
        Fact::Relation {
            relation: Relation::Representation {
                original: submitted.clone(),
                transformed: revisions[0].clone(),
                rule: "explicit-input-correlation".into(),
            },
        },
        Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: agent(),
                membership: SelectionMembership::EntrySet {
                    entries: vec![entry],
                },
                content_scope: HistoryContentScope::ObservationSet {
                    observations: vec![reader_claim_ref(4)],
                },
                excluded_heads: vec![],
            }),
        },
    ]);
    let projection = facts_projection(
        facts,
        &PayloadPreviews::from_iter([
            (recorded, text_preview("recorded wrapper")),
            (excluded, text_preview("unselected body")),
            (submitted_payload, text_preview("original request")),
        ]),
    );
    let scope = ReadingScope {
        agent: agent(),
        selection: Some(selection),
    };
    let rows = &projection.windows[&scope];
    assert_eq!(rows.len(), 1);
    let ReadingContent::Message {
        revision_ref,
        message_ref,
        parts,
        ..
    } = &rows[0].content
    else {
        panic!("submitted presentation missing")
    };
    assert_eq!(revision_ref, &submitted);
    assert_eq!(message_ref, &submitted_message);
    assert_eq!(parts[0].preview, "original request");
    assert!(
        projection.selected_objects[&scope].contains(&AnyObjectRef::MessageRevision(submitted))
    );
    assert!(
        !projection.selected_objects[&scope]
            .contains(&AnyObjectRef::MessageRevision(revisions[1].clone()))
    );
}
