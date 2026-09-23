use super::*;
use crate::mapping::{ArchiveContext, MapperState};
use crate::reducer::{Reducer, Reduction};
use sylph_protocol::EnvironmentId;

fn capture(index: u64, live: bool) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env").unwrap());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: if live { "live" } else { "native" }.into(),
            producer_epoch: "epoch".into(),
            producer_seq: index + 1,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: if live { "live" } else { "native" }.into(),
            },
            generation: 0,
            locator: if live {
                SourceLocator::Frame { ordinal: index }
            } else {
                SourceLocator::Jsonl {
                    byte_start: index * 100,
                    byte_end: index * 100 + 100,
                    record_ordinal: index,
                }
            },
        },
        ingestion_authorization_epoch: IngestionEpoch::new("admission").unwrap(),
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "receipt".into(),
            value: index as i64,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::ServerReceipt,
        },
        source_clock: None,
        transport_kind: if live {
            TransportKind::LiveStdout
        } else {
            TransportKind::NativeJsonl
        },
        vendor_build: Some("0.84.3".into()),
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}
fn run(values: &[Value], live: bool) -> Vec<MapOutput> {
    let mut state = MapperState::default();
    let mut archive = ArchiveContext::default();
    let mut outputs = vec![];
    for (i, value) in values.iter().enumerate() {
        let capture = capture(i as u64, live);
        let output = PiMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(value).unwrap(),
            state: &state,
            archive: &archive,
        });
        state = serde_json::from_slice(&serde_json::to_vec(&output.state).unwrap()).unwrap();
        for claim in &output.claims {
            match &claim.fact {
                Fact::Declare {
                    object: TraceObject::HistoryEntry(entry),
                } => {
                    archive
                        .semantic_mut()
                        .history_entry_proofs
                        .entry(entry.entry_ref.clone())
                        .or_default()
                        .insert(
                            claim.claim_ref.clone(),
                            crate::mapping::ArchiveEntryProof {
                                position: capture.source_position.clone(),
                                evidence: claim.evidence.clone(),
                            },
                        );
                    archive.semantic_mut().object_evidence.insert(
                        AnyObjectRef::HistoryEntry(entry.entry_ref.clone()),
                        claim.evidence.clone().into(),
                    );
                    archive
                        .semantic_mut()
                        .history_entries
                        .insert(entry.entry_ref.clone(), entry.clone());
                }
                Fact::Declare {
                    object: TraceObject::Operation(operation),
                } => {
                    archive
                        .semantic_mut()
                        .operations
                        .insert(operation.operation_ref.clone(), operation.clone());
                }
                Fact::HistoryContentObserved { observation } => {
                    archive
                        .semantic_mut()
                        .history_contents
                        .entry(observation.entry.clone())
                        .or_default()
                        .push_back(ArchiveHistoryContent {
                            claim_ref: claim.claim_ref.clone(),
                            observation: observation.clone(),
                            evidence: claim.evidence.clone(),
                        });
                }
                _ => {}
            }
        }
        archive.correlations.extend(output.correlations.clone());
        archive.extend_correlation_evidence(output.correlation_evidence.clone());
        archive.observations.insert(capture.observation_ref);
        outputs.push(output);
    }
    outputs
}
fn reduce(outputs: &[MapOutput]) -> Reduction {
    let claims: Vec<_> = outputs
        .iter()
        .flat_map(|output| output.claims.clone())
        .collect();
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    let mut reducer = Reducer::default();
    for chunk in claims.chunks(5) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    let result = reducer.resolve();
    for chunk in claims.chunks(3) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    assert_eq!(result, reducer.resolve());
    result
}
fn header() -> Value {
    serde_json::json!({"type":"session","id":"root","version":3})
}
fn state() -> Value {
    serde_json::json!({"type":"response","command":"get_state","success":true,"data":{"sessionId":"root"}})
}
fn objects(outputs: &[MapOutput]) -> impl Iterator<Item = &TraceObject> {
    outputs
        .iter()
        .flat_map(|output| &output.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare { object } => Some(object),
            _ => None,
        })
}

#[test]
fn native_tree_keeps_nonmessage_predecessors_and_unknown_outer_execution() {
    let records: Vec<Value> =
        include_str!("../../../../../fixtures/native/pi/failure-reasoning-required.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&records, false);
    let reduced = reduce(&output);
    let entries: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::HistoryEntry(entry) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 7);
    for id in ["062f758f", "f90f671b"] {
        let entry=entries.iter().find(|entry|matches!(entry.entry_ref.address(),ObjectAddress::Native{key,..}if key==&[id])).unwrap();
        assert!(
            matches!(entry.predecessor.as_ref().unwrap().address(),ObjectAddress::Native{key,..}if key==&["17b6bee4"])
        );
    }
    assert!(reduced.executions.is_empty());
    assert!(reduced
        .histories
        .values()
        .all(|history| history.selected_entries.is_none()));
}

#[test]
fn native_and_live_admissions_remain_distinct_while_snapshots_only_reobserve_content() {
    let message = serde_json::json!({"role":"user","content":"retained admission bytes"});
    let entry = serde_json::json!({"type":"message","id":"admitted-entry","parentId":null,"message":message});
    let mut output = run(&[header(), entry.clone()], false);
    output.extend(run(
        &[
            state(),
            serde_json::json!({"type":"message_start","message":message}),
            serde_json::json!({"type":"message_end","message":message}),
            serde_json::json!({"type":"response","command":"get_entries","success":true,"data":{"entries":[entry],"leafId":"admitted-entry"}}),
        ],
        true,
    ));
    let deliveries: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Delivery(delivery) => Some(delivery),
            _ => None,
        })
        .collect();
    assert_eq!(deliveries.len(), 2);
    assert_ne!(deliveries[0].delivery_ref, deliveries[1].delivery_ref);
    assert!(
        matches!(deliveries[0].delivery_ref.address(), ObjectAddress::Native { namespace, key, .. } if namespace == "pi.entry-admission" && key == &["admitted-entry"])
    );
    assert!(matches!(
        deliveries[1].delivery_ref.address(),
        ObjectAddress::Occurrence { .. }
    ));

    let bindings: Vec<_> = output
        .iter()
        .flat_map(|output| &output.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::DeliveryRevisionObserved { delivery, revision } => Some((delivery, revision)),
            _ => None,
        })
        .collect();
    assert_eq!(bindings.len(), 2);
    for (binding, delivery) in bindings.iter().zip(&deliveries) {
        assert_eq!(binding.0, &delivery.delivery_ref);
    }
    assert_ne!(bindings[0].1, bindings[1].1);
    let revisions: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::MessageRevision(revision) => Some(revision),
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 3);
    assert_eq!(revisions[0].message, revisions[2].message);
    assert_ne!(revisions[0].revision_ref, revisions[2].revision_ref);
    assert_eq!(revisions[0].parts, revisions[2].parts);
    let admissions: Vec<_> = output
        .iter()
        .flat_map(|output| &output.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::DeliveryStage {
                delivery,
                stage: DeliveryStage::Admitted { entry },
            } => Some((delivery, entry)),
            _ => None,
        })
        .collect();
    assert_eq!(admissions.len(), 2);
    assert!(admissions[0].1.is_some());
    assert!(admissions[1].1.is_none());
    assert_eq!(
        output
            .iter()
            .flat_map(|output| &output.payloads)
            .filter(|payload| payload.bytes == b"retained admission bytes")
            .count(),
        3
    );
    assert!(!output
        .iter()
        .flat_map(|output| &output.claims)
        .any(|claim| matches!(claim.fact, Fact::Equivalence { .. })));
}

#[test]
fn recorded_failed_rpc_run_settles_only_after_inner_end() {
    let rows: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/pi/rpc-failure.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &Value| row["direction"] == "in")
            .map(|row| row["frame"].clone())
            .collect();
    let output = run(&rows, true);
    let inner = rows
        .iter()
        .position(|row| row["type"] == "agent_end")
        .unwrap();
    assert!(output[inner].state.values["pi"]["outer"].is_object());
    assert!(!output[inner].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            boundary: ExecutionBoundary::Outcome { .. },
            ..
        }
    )));
    let reduced = reduce(&output);
    assert_eq!(
        reduced
            .executions
            .iter()
            .filter(
                |(reference, execution)| matches!(reference, AnyObjectRef::Activation(_))
                    && execution.outcome.known() == Some(&TerminalOutcome::Failed)
            )
            .count(),
        1
    );
    assert!(output
        .iter()
        .flat_map(|output| &output.payloads)
        .any(|payload| payload.bytes == b"Reply with exactly SYLPH_PI_TRACE_OK."));
    assert!(reduced.histories.values().any(|history| history
        .selected_entries
        .as_ref()
        .is_some_and(|entries| entries.len() == 4)));
}

#[test]
fn retry_stays_in_one_outer_run_and_latest_actual_assistant_controls_outcome() {
    let error = serde_json::json!({"role":"assistant","content":[{"type":"text","text":"failed attempt"}],"stopReason":"error"});
    let success = serde_json::json!({"role":"assistant","content":[{"type":"text","text":"recovered"}],"stopReason":"stop"});
    let output = run(
        &[
            state(),
            serde_json::json!({"type":"agent_start"}),
            serde_json::json!({"type":"message_start","message":error}),
            serde_json::json!({"type":"message_end","message":error}),
            serde_json::json!({"type":"agent_end","willRetry":true}),
            serde_json::json!({"type":"agent_start"}),
            serde_json::json!({"type":"message_start","message":success}),
            serde_json::json!({"type":"message_end","message":success}),
            serde_json::json!({"type":"agent_settled"}),
        ],
        true,
    );
    assert_eq!(
        objects(&output)
            .filter(|object| matches!(object, TraceObject::Activation(_)))
            .count(),
        1
    );
    let reduced = reduce(&output);
    assert_eq!(
        reduced.executions.values().next().unwrap().outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert!(output
        .iter()
        .flat_map(|output| &output.payloads)
        .any(|body| body.bytes == b"failed attempt"));
}

#[test]
fn reused_native_call_id_resolves_on_the_results_own_predecessor_branch() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"message","id":"assistant-a","parentId":null,"message":{"role":"assistant","content":[{"type":"toolCall","id":"same-id","name":"read","arguments":{"path":"a"}}]}}),
            serde_json::json!({"type":"message","id":"assistant-b","parentId":null,"message":{"role":"assistant","content":[{"type":"toolCall","id":"same-id","name":"read","arguments":{"path":"b"}}]}}),
            serde_json::json!({"type":"message","id":"result-a","parentId":"assistant-a","message":{"role":"toolResult","toolCallId":"same-id","content":[{"type":"text","text":"A bytes"}],"isError":false}}),
        ],
        false,
    );
    let result = objects(&output)
        .find_map(|object| match object {
            TraceObject::Result(result) => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(result.operation.address(),ObjectAddress::Native{key,..}if key==&["assistant-a","0","same-id"])
    );
    assert!(output[3]
        .payloads
        .iter()
        .any(|body| body.bytes == b"A bytes"));
}

#[test]
fn custom_hidden_content_is_context_and_binary_image_has_an_evidenced_descriptor() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"custom_message","id":"custom-a","parentId":null,"customType":"extension-context","display":false,"content":[{"type":"text","text":"retained hidden context"},{"type":"image","mimeType":"image/png","data":"AQID"}]}),
        ],
        false,
    );
    let message = objects(&output)
        .find_map(|object| match object {
            TraceObject::Message(message) => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(message.purpose, MessagePurpose::Context);
    assert!(matches!(message.author, Author::Harness { .. }));
    let revision = objects(&output)
        .find_map(|object| match object {
            TraceObject::MessageRevision(revision) => Some(revision),
            _ => None,
        })
        .unwrap();
    assert_eq!(revision.parts[1].kind, ContentKind::Image);
    assert_eq!(revision.parts[1].media_type.as_deref(), Some("image/png"));
    assert_eq!(revision.parts[1].encoding, PayloadEncoding::Binary);
    let body = output[1]
        .payloads
        .iter()
        .find(|body| body.reference == revision.parts[1].payload)
        .unwrap();
    assert_eq!(body.bytes, [1, 2, 3]);
    assert!(!output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Included { .. },
            ..
        }
    )));
}

#[test]
fn live_partial_message_snapshots_replace_instead_of_append() {
    let output = run(
        &[
            state(),
            serde_json::json!({"type":"agent_start"}),
            serde_json::json!({"type":"message_start","message":{"role":"assistant","content":[]}}),
            serde_json::json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"A"}]}}),
            serde_json::json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"AB"}]}}),
            serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"rewritten"}],"stopReason":"stop"}}),
        ],
        true,
    );
    let revisions: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::MessageRevision(value) => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 3);
    assert!(matches!(
        revisions[1].form,
        RevisionForm::Replacement { .. }
    ));
    assert!(matches!(revisions[2].form, RevisionForm::Final { .. }));
    assert_eq!(output[4].payloads[0].bytes, b"AB");
    assert_eq!(output[5].payloads[0].bytes, b"rewritten");
}

#[test]
fn compaction_and_unresolved_fork_keep_original_archive_and_uncertainty() {
    let output = run(
        &[
            serde_json::json!({"type":"session","id":"child","parentSession":"/source/parent.jsonl"}),
            serde_json::json!({"type":"message","id":"copied","parentId":null,"message":{"role":"assistant","content":[{"type":"text","text":"inherited"}]}}),
            serde_json::json!({"type":"compaction","id":"compact","parentId":"copied","summary":"summary bytes","firstKeptEntryId":"copied","tokensBefore":900}),
        ],
        false,
    );
    let message = objects(&output)
        .find_map(|object| match object {
            TraceObject::Message(value) => Some(value),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        message.author,
        Author::Agent {
            agent: OwnerRef::Unresolved { .. }
        }
    ));
    assert_eq!(
        objects(&output)
            .filter(|object| matches!(object, TraceObject::HistoryEntry(_)))
            .count(),
        2
    );
    assert!(objects(&output).any(|object| matches!(
        object,
        TraceObject::ContextCheckpoint(ContextCheckpoint {
            coverage: InclusionCoverage::Summary,
            ..
        })
    )));
    assert!(objects(&output).any(|object| matches!(object,
        TraceObject::HistoryEntry(HistoryEntry { original_owner: Some(OwnerRef::Unresolved { namespace, .. }), .. }) if namespace == "pi.entry-origin"
    )));
}

#[test]
fn extension_tool_name_and_unknown_payload_never_invent_child_agents() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"message","id":"extension","parentId":null,"message":{"role":"assistant","content":[{"type":"toolCall","id":"tool-a","name":"subagent","arguments":{"task":"task"}},{"type":"new-vendor-content","secret_shape":{"retained":true}}]}}),
        ],
        false,
    );
    assert_eq!(
        objects(&output)
            .filter(|object| matches!(object, TraceObject::Agent(_)))
            .count(),
        1
    );
    assert!(!output
        .iter()
        .flat_map(|out| &out.claims)
        .any(|claim| matches!(
            claim.fact,
            Fact::Relation {
                relation: Relation::Spawn { .. }
            }
        )));
    assert!(output[1]
        .payloads
        .iter()
        .any(|body| serde_json::from_slice::<Value>(&body.bytes)
            .ok()
            .is_some_and(|value| value["secret_shape"]["retained"] == true)));
}

#[test]
fn accepted_rpc_input_and_missing_leaf_do_not_invent_execution_or_history_selection() {
    let output = run(
        &[
            state(),
            serde_json::json!({"type":"response","id":"input-a","command":"prompt","success":true}),
            serde_json::json!({"type":"response","command":"get_entries","success":true,"data":{"entries":[]}}),
        ],
        true,
    );
    assert!(!objects(&output).any(|object| matches!(
        object,
        TraceObject::Activation(_) | TraceObject::Message(_) | TraceObject::HistorySelection(_)
    )));
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::CaptureGap,
                ..
            }
        }
    )));
    assert!(output[2].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::UnknownSelection,
                ..
            }
        }
    )));
}

#[test]
fn rpc_history_snapshots_bind_only_the_entry_observations_they_carry() {
    for command in ["get_entries", "get_tree"] {
        let response = |entries: Vec<Value>, leaf: &str| {
            let data = if command == "get_tree" {
                serde_json::json!({
                    "tree": entries.into_iter().map(|entry| serde_json::json!({
                        "entry": entry,
                        "children": [],
                    })).collect::<Vec<_>>(),
                    "leafId": leaf,
                })
            } else {
                serde_json::json!({"entries": entries, "leafId": leaf})
            };
            serde_json::json!({"type":"response","command":command,"success":true,"data":data})
        };
        let output = run(
            &[
                state(),
                response(
                    vec![
                        serde_json::json!({"type":"message","id":"ancestor","parentId":null,"message":{"role":"user","content":"prior snapshot"}}),
                    ],
                    "ancestor",
                ),
                response(
                    vec![
                        serde_json::json!({"type":"message","id":"current","parentId":"ancestor","message":{"role":"assistant","content":"current snapshot"}}),
                    ],
                    "current",
                ),
                response(vec![], "current"),
            ],
            true,
        );
        let mut previous = BTreeSet::new();
        for (index, expected_entry) in [(1, Some("ancestor")), (2, Some("current")), (3, None)] {
            let selection = output[index]
                .claims
                .iter()
                .find_map(|claim| match &claim.fact {
                    Fact::Declare {
                        object: TraceObject::HistorySelection(selection),
                    } => Some(selection),
                    _ => None,
                })
                .unwrap();
            let HistoryContentScope::ObservationSet { observations } = &selection.content_scope
            else {
                panic!("RPC snapshots must retain their explicit content scope");
            };
            assert_eq!(observations.len(), usize::from(expected_entry.is_some()));
            for reference in observations {
                assert!(!previous.contains(reference));
                let observation = output[index]
                    .claims
                    .iter()
                    .find_map(|claim| match &claim.fact {
                        Fact::HistoryContentObserved { observation }
                            if claim.claim_ref == *reference =>
                        {
                            Some(observation)
                        }
                        _ => None,
                    })
                    .expect("selection content belongs to this response");
                assert!(
                    matches!(observation.entry.address(), ObjectAddress::Native { key, .. } if key == &[expected_entry.unwrap()])
                );
                assert_eq!(observation.mapping_rule, "pi/rpc-entry-history");
                assert_eq!(
                    observation.position,
                    capture(index as u64, true).source_position
                );
                assert!(observation.complete);
                assert_eq!(observation.slots.len(), 1);
                assert_eq!(observation.slots[0].slot, 0);
                assert!(matches!(
                    observation.slots[0].content,
                    HistoryContent::Message(_)
                ));
                previous.insert(reference.clone());
            }
        }
    }
}

#[test]
fn compaction_preserves_the_exact_kept_path_and_cites_its_entry_proofs() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"message","id":"discarded","parentId":null,"message":{"role":"user","content":"old"}}),
            serde_json::json!({"type":"message","id":"kept-a","parentId":"discarded","message":{"role":"user","content":"new request"}}),
            serde_json::json!({"type":"message","id":"kept-b","parentId":"kept-a","message":{"role":"assistant","content":"answer"}}),
            serde_json::json!({"type":"compaction","id":"compact","parentId":"kept-b","firstKeptEntryId":"kept-a","summary":"summary bytes"}),
        ],
        false,
    );
    let selection = objects(&output)
        .find_map(|object| match object {
            TraceObject::HistorySelection(selection) => Some(selection),
            _ => None,
        })
        .unwrap();
    let SelectionMembership::EntrySet { entries } = &selection.membership else {
        panic!("known predecessor range produces explicit immutable membership");
    };
    assert_eq!(entries.len(), 2);
    for (entry, id) in entries.iter().zip(["kept-a", "kept-b"]) {
        assert!(matches!(entry.address(),ObjectAddress::Native{key,..}if key==&[id]));
    }
    let membership = output[4]
        .claims
        .iter()
        .find(|claim| {
            matches!(
                claim.fact,
                Fact::Relation {
                    relation: Relation::ContextMembership { .. }
                }
            )
        })
        .unwrap();
    assert!(membership
        .evidence
        .contains(&capture(2, false).observation_ref));
    assert!(membership
        .evidence
        .contains(&capture(3, false).observation_ref));
    assert!(!membership
        .evidence
        .contains(&capture(1, false).observation_ref));
    let reduced = reduce(&output);
    assert!(reduced.relations.iter().any(|relation| matches!(
        relation.relation,
        Relation::ContextMembership {
            disposition: ContextMembershipKind::Preserved,
            ..
        }
    )));
    assert!(objects(&output).any(|object| matches!(
        object,
        TraceObject::ContextCheckpoint(ContextCheckpoint {
            coverage: InclusionCoverage::Summary,
            ..
        })
    )));
}

#[test]
fn missing_compaction_predecessor_stays_unresolved_instead_of_empty_membership() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"compaction","id":"compact","parentId":"missing","firstKeptEntryId":"first","summary":"retained summary"}),
        ],
        false,
    );
    assert!(objects(&output).any(|object| matches!(object, TraceObject::ContextCheckpoint(_))));
    assert!(!objects(&output).any(|object| matches!(object, TraceObject::HistorySelection(_))));
    assert!(output[1].requests.iter().any(|request|matches!(request,ArchiveRequest::Object{object_ref:AnyObjectRef::HistoryEntry(reference)}if matches!(reference.address(),ObjectAddress::Native{key,..}if key==&["missing"]))));
}

#[test]
fn absent_text_field_is_opaque_while_explicit_empty_text_remains_empty() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"message","id":"missing","parentId":null,"message":{"role":"user","content":[{"type":"text"}]}}),
            serde_json::json!({"type":"message","id":"empty","parentId":"missing","message":{"role":"user","content":[{"type":"text","text":""}]}}),
        ],
        false,
    );
    let revisions: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::MessageRevision(value) => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(revisions[0].parts[0].kind, ContentKind::Opaque);
    assert_eq!(revisions[1].parts[0].kind, ContentKind::Text);
    assert_eq!(output[2].payloads[0].bytes, b"");
    assert_eq!(
        serde_json::from_slice::<Value>(&output[1].payloads[0].bytes).unwrap()["type"],
        "text"
    );
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::DecodeFailure,
                ..
            }
        }
    )));
}

#[test]
fn reported_usage_keeps_cache_write_and_usd_total_without_adding_components() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"message","id":"usage","parentId":null,"message":{"role":"assistant","content":"answer","usage":{"input":10,"output":2,"cacheRead":4,"cacheWrite":3,"cost":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"total":10}}}}),
        ],
        false,
    );
    let costs: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Measurement(measurement)
                if matches!(measurement.metric, Metric::Cost { .. }) =>
            {
                Some(measurement)
            }
            _ => None,
        })
        .collect();
    assert_eq!(costs.len(), 1);
    assert_eq!(costs[0].value, 10.into());
    assert_eq!(
        costs[0].unit,
        MeasurementUnit::Currency { code: "USD".into() }
    );
    assert!(objects(&output).any(|object|matches!(object,TraceObject::Measurement(measurement)if matches!(&measurement.metric,Metric::Extension{namespace,name}if namespace=="pi.usage"&&name=="cache_write_tokens")&&measurement.value==3.into())));
}

#[test]
fn intrinsic_source_owner_requires_native_header_and_keeps_its_generation() {
    let mut capture = capture(0, false);
    capture.source_position.generation = 9;
    capture.declared_subject_refs = vec![AgentRef::Session(
        SessionId::decode("env:pi:wrong-hint").unwrap(),
    )];
    let output = PiMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: br#"{"type":"session","id":"root","version":3}"#,
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert_eq!(output.source_owners.len(), 1);
    assert_eq!(output.source_owners[0].generation, 9);
    assert_eq!(
        output.source_owners[0].owner.agent,
        AgentRef::Session(SessionId::decode("env:pi:root").unwrap())
    );
    assert_eq!(
        output.source_owners[0].owner.evidence,
        [capture.observation_ref]
    );
    assert!(run(&[state()], true)[0].source_owners.is_empty());
}

mod maintenance;
mod runtime_tools;

mod origin;

#[test]
fn retained_pi_fork_entries_do_not_replay_user_or_tool_result_admission() {
    let values: Vec<Value> = include_str!("../../../../../fixtures/native/pi/fork-child.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut values = values;
    values.push(serde_json::json!({"type":"message","id":"tool-copy","parent_id":"92123d75-0330-46ae-abbe-4e5435c601ac","message":{"role":"toolResult","toolCallId":"unknown-original-call","content":[{"type":"text","text":"Copied tool output"}]}}));
    let output = run(&values, false);
    let claims: Vec<_> = output.iter().flat_map(|value| &value.claims).collect();
    assert!(
        !claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::Declare {
                object: TraceObject::Delivery(_)
            } | Fact::DeliveryStage { .. }
                | Fact::DeliveryRevisionObserved { .. }
        )),
        "a copied record must not become a fresh input or tool-result delivery"
    );
    let entries: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::HistoryEntry(entry) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), values.len() - 1);
    assert!(
        entries
            .iter()
            .all(|entry| matches!(entry.original_owner, Some(OwnerRef::Unresolved { .. })))
    );
    assert!(
        output
            .iter()
            .flat_map(|value| &value.payloads)
            .any(|body| body.bytes == b"Copied tool output")
    );
    assert!(!output.iter().flat_map(|value| &value.requests).any(|request| matches!(request, ArchiveRequest::Correlation { namespace, .. } if namespace == "pi.fork-entry")), "unproved membership must not wait on a correlation with no producer");
}

#[test]
fn historical_pi_snapshots_do_not_create_admission_but_direct_message_events_do() {
    let user = serde_json::json!({"role":"user","content":[{"type":"text","text":"Already retained input"}]});
    let output = run(
        &[
            state(),
            serde_json::json!({"type":"response","command":"get_entries","success":true,"data":{"entries":[{"type":"message","id":"old-input","parentId":null,"message":user}],"leafId":"old-input"}}),
            serde_json::json!({"type":"response","command":"get_tree","success":true,"data":{"tree":[{"entry":{"type":"message","id":"old-result","parentId":"old-input","message":{"role":"toolResult","toolCallId":"original","content":[{"type":"text","text":"Already retained result"}]}},"children":[]}],"leafId":"old-result"}}),
            serde_json::json!({"type":"message_start","message":user}),
            serde_json::json!({"type":"message_end","message":user}),
        ],
        true,
    );
    for snapshot in &output[1..3] {
        assert!(
            !snapshot.claims.iter().any(|claim| matches!(
                claim.fact,
                Fact::Declare {
                    object: TraceObject::Delivery(_)
                } | Fact::DeliveryStage { .. }
                    | Fact::DeliveryRevisionObserved { .. }
            )),
            "reading historical membership cannot create an admission"
        );
        assert!(
            snapshot
                .claims
                .iter()
                .any(|claim| matches!(claim.fact, Fact::HistoryContentObserved { .. }))
        );
    }
    assert!(
        output[4].claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::DeliveryStage {
                stage: DeliveryStage::Admitted { .. },
                ..
            }
        )),
        "direct runtime receipt still establishes admission"
    );
}

#[test]
fn a_fork_tool_record_keeps_its_unresolved_production_without_naming_the_child_as_requester() {
    let output = run(
        &[
            serde_json::json!({"type":"session","id":"fork","parentSession":"/retained/original.jsonl"}),
            serde_json::json!({"type":"message","id":"copied-tool","parentId":null,"message":{"role":"assistant","content":[{"type":"toolCall","id":"call","name":"read","arguments":{"path":"recorded.txt"}}]}}),
        ],
        false,
    );
    let operation = objects(&output)
        .find_map(|object| match object {
            TraceObject::Operation(operation) => Some(operation),
            _ => None,
        })
        .unwrap();
    assert!(matches!(operation.owner, OwnerRef::Unresolved { .. }));
    assert!(
        operation.requester.is_none(),
        "an unresolved copied invocation cannot assert a child requester"
    );
    assert_eq!(
        operation.category,
        OperationCategory::Tool {
            namespace: Some("pi".into()),
            name: "read".into()
        }
    );
}
