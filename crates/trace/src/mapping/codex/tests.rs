use super::*;
use crate::mapping::{ArchiveContext, MapperState};
use sylph_protocol::EnvironmentId;

fn capture(seq: u64, live: bool) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env").unwrap());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "capture".into(),
            producer_epoch: "epoch".into(),
            producer_seq: seq,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: if live { "live" } else { "native" }.into(),
            },
            generation: 0,
            locator: if live {
                SourceLocator::Frame { ordinal: seq }
            } else {
                SourceLocator::Jsonl {
                    byte_start: seq * 100,
                    byte_end: seq * 100 + 100,
                    record_ordinal: seq - 1,
                }
            },
        },
        ingestion_authorization_epoch: IngestionEpoch::new("admission").unwrap(),
        declared_subject_refs: vec![agent("root")],
        captured_at: TimeEvidence {
            domain: "receipt".into(),
            value: seq as i64,
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
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}
fn agent(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env:codex:{id}")).unwrap())
}

#[test]
fn opening_reply_identity_requires_its_retained_request_in_the_same_runtime_source() {
    let mut request = capture(1, true);
    request.transport_kind = TransportKind::LiveStdin;
    request.declared_subject_refs.clear();
    let reply = capture(2, true);
    let state = MapperState::default();
    let sent = CodexMapper.map(MapInput {
        capture: &request,
        canonical_observation: &request.observation_ref,
        raw: br#"{"id":12,"method":"thread/start","params":{"cwd":"/workspace"}}"#,
        state: &state,
        archive: &ArchiveContext::default(),
    });
    let raw = br#"{"id":12,"result":{"thread":{"id":"actual-native"}}}"#;
    let missing = CodexMapper.map(MapInput {
        capture: &reply,
        canonical_observation: &reply.observation_ref,
        raw,
        state: &state,
        archive: &ArchiveContext::default(),
    });
    assert!(missing.claims.is_empty());
    let found = CodexMapper.map(MapInput {
        capture: &reply,
        canonical_observation: &reply.observation_ref,
        raw,
        state: &sent.state,
        archive: &ArchiveContext::default(),
    });
    let declaration=found.claims.iter().find(|claim|matches!(&claim.fact,Fact::Declare {object:TraceObject::Agent(agent)} if agent.agent_ref==self::agent("actual-native"))).unwrap();
    assert!(declaration.evidence.contains(&request.observation_ref));
    assert!(declaration.evidence.contains(&reply.observation_ref));
}

#[test]
fn selected_rollout_metadata_requires_matching_source_identity_and_a_complete_extent() {
    use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};
    let native = capture(1, false);
    let mut metadata = capture(2, false);
    metadata.transport_kind = TransportKind::NativeDatabase;
    let mut snapshot = NativeDatabaseSnapshot {
        database_locator: "/native/state_5.sqlite".into(),
        table: "threads".into(),
        columns: vec![],
        status: NativeDatabaseStatus::Available,
        row: [
            ("id".into(), NativeDatabaseValue::Text("root".into())),
            (
                "rollout_path".into(),
                NativeDatabaseValue::Text("/native/selected.jsonl".into()),
            ),
        ]
        .into(),
        selected_source: Some(native.source_position.source.clone()),
        selected_generation: Some(0),
        selected_record_count: Some(17),
        selected_pending_bytes: Some(0),
    };
    let archive = ArchiveContext {
        source_owners: [(
            (native.source_position.source.clone(), 0),
            crate::mapping::SourceOwner {
                agent: agent("root"),
                evidence: vec![native.observation_ref],
                rule: "codex/governing-native-header".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..ArchiveContext::default()
    };
    let map = |snapshot: &NativeDatabaseSnapshot| {
        CodexMapper.map(MapInput {
            capture: &metadata,
            canonical_observation: &metadata.observation_ref,
            raw: &serde_json::to_vec(snapshot).unwrap(),
            state: &MapperState::default(),
            archive: &archive,
        })
    };
    let selected = map(&snapshot);
    assert!(selected.claims.is_empty());
    assert!(
        selected
            .requests
            .iter()
            .any(|request| matches!(request, ArchiveRequest::HistorySource { .. })),
        "a declared count cannot replace actual retained coordinate witnesses"
    );
    snapshot.row.insert(
        "id".into(),
        NativeDatabaseValue::Text("another-agent".into()),
    );
    let wrong = map(&snapshot);
    assert!(wrong.claims.is_empty());
    assert_eq!(wrong.diagnostics[0].code, DiagnosticCode::ConflictingOwner);
}
#[test]
fn reverted_rollout_selection_stays_unknown_until_its_frozen_base_is_resolved() {
    use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};
    let mut native = capture(1, false);
    native.source_position.locator = SourceLocator::Jsonl {
        byte_start: 0,
        byte_end: 100,
        record_ordinal: 0,
    };
    let mut metadata = capture(2, false);
    metadata.transport_kind = TransportKind::NativeDatabase;
    let snapshot = NativeDatabaseSnapshot {
        database_locator: "/native/state_5.sqlite".into(),
        table: "threads".into(),
        columns: vec![],
        status: NativeDatabaseStatus::Available,
        row: [("id".into(), NativeDatabaseValue::Text("root".into()))].into(),
        selected_source: Some(native.source_position.source.clone()),
        selected_generation: Some(0),
        selected_record_count: Some(1),
        selected_pending_bytes: Some(0),
    };
    let source = (native.source_position.source.clone(), 0);
    let archive = ArchiveContext {
        source_owners: [(
            source.clone(),
            crate::mapping::SourceOwner {
                agent: agent("root"),
                evidence: vec![native.observation_ref.clone()],
                rule: "codex/governing-native-header".into(),
            },
        )]
        .into_iter()
        .collect(),
        history_records: [(
            source,
            [(
                0u64,
                history::HistoryRecord {
                    position: native.source_position.clone(),
                    observation: native.observation_ref.clone(),
                    vendor_ordinal: Some(214),
                    header: Some(history::HistoryHeader {
                        history_mode: Some("paginated".into()),
                        agent: agent("root"),
                        history_base: Some(serde_json::json!({
                            "thread_id": "prior-physical-rollout",
                            "end_ordinal_exclusive": 214,
                            "end_byte_offset": 4200,
                        })),
                        inherited_before: None,
                        inherited_from: None,
                    }),
                },
            )]
            .into_iter()
            .collect::<imbl::OrdMap<u64, history::HistoryRecord>>(),
        )]
        .into_iter()
        .collect(),
        ..ArchiveContext::default()
    };
    let output = CodexMapper.map(MapInput {
        capture: &metadata,
        canonical_observation: &metadata.observation_ref,
        raw: &serde_json::to_vec(&snapshot).unwrap(),
        state: &MapperState::default(),
        archive: &archive,
    });
    assert!(
        output.claims.is_empty(),
        "a physical suffix is not the selected logical history"
    );
    assert!(output
        .diagnostics
        .iter()
        .any(|issue| issue.code == DiagnosticCode::MissingSource));
}

fn run(values: &[Value], live: bool) -> Vec<MapOutput> {
    let mut state = MapperState::default();
    let mut outputs = vec![];
    for (i, value) in values.iter().enumerate() {
        let capture = capture(i as u64 + 1, live);
        let raw = serde_json::to_vec(value).unwrap();
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        state = output.state.clone();
        outputs.push(output);
    }
    outputs
}

fn reduce(outputs: &[MapOutput]) -> crate::reducer::Reduction {
    let claims: Vec<_> = outputs
        .iter()
        .flat_map(|output| output.claims.clone())
        .collect();
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    let mut reducer = crate::reducer::Reducer::default();
    for chunk in claims.chunks(7) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    let expected = reducer.resolve();
    for chunk in claims.chunks(3) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    assert_eq!(reducer.resolve(), expected);
    expected
}
fn header() -> Value {
    serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"root","history_mode":"paginated"}})
}

#[test]
fn native_child_header_and_vendor_ordinal_keep_inherited_execution_outside_child() {
    let fixture=include_str!("../../../../../fixtures/native/codex/subagent-child-collab-namespace-newest-format-of-parent-019ff38d.jsonl");
    let values: Vec<Value> = fixture
        .lines()
        .take(10)
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let output = run(&values, false);
    let child = agent("019ff38e-2539-7bb1-8ae6-40fe6441c637");
    assert_eq!(
        output.last().unwrap().state.governing_agent,
        Some(child.clone())
    );
    let activations: Vec<_> = output
        .iter()
        .flat_map(|out| &out.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Activation(value),
            } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(activations.len(), 1);
    assert_eq!(
        activations[0].owner,
        OwnerRef::Agent {
            agent: child.clone()
        }
    );
    assert_eq!(
        activations[0].activation_ref.address(),
        &ObjectAddress::Native {
            scope: child,
            namespace: "codex.turn".into(),
            key: vec!["019ff38e-2584-7442-bb29-528e336a8773".into()]
        }
    );
    assert!(!output[8]
        .claims
        .iter()
        .any(|claim| matches!(claim.fact, Fact::HistoryPosition { .. })));
}

#[test]
fn parent_completion_keeps_later_child_output_and_own_child_activation() {
    let fixture =
        include_str!("../../../../../docs/measurements/codex/captures/s10_delegation_fork.jsonl");
    let values: Vec<Value> = fixture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["dir"] == "in")
        .map(|record| record["frame"].clone())
        .collect();
    let output = run(&values, true);
    let child = agent("01a0462b-cdb3-75e0-b609-0a218686b187");
    let parent_close = values
        .iter()
        .position(|frame| {
            frame["method"] == "turn/completed"
                && frame["params"]["threadId"] == "01a0462b-b919-7c33-9d9c-f8bec39c3cc8"
        })
        .unwrap();
    assert!(
        output[parent_close].state.values["codex"]["active_by_agent"]
            .as_object()
            .unwrap()
            .contains_key(&child.encode())
    );
    let later_child: Vec<_> = output[parent_close + 1..]
        .iter()
        .flat_map(|out| &out.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Message(value),
            } if value.message_ref.scope() == &child => Some(value),
            _ => None,
        })
        .collect();
    assert!(!later_child.is_empty());
    assert!(output.iter().flat_map(|out|&out.claims).any(|claim|matches!(&claim.fact,Fact::Execution{execution:ExecutionRef::Activation(reference),boundary:ExecutionBoundary::Outcome{outcome:TerminalOutcome::Succeeded,..}} if reference.scope()==&child)));
}

#[test]
fn native_error_and_seconds_do_not_turn_into_success_or_receipt_timing() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-a","started_at":100}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-a","completed_at":110,"duration_ms":17,"error":{"message":"budget"}}}),
        ],
        false,
    );
    assert!(output[2].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            boundary: ExecutionBoundary::Outcome {
                outcome: TerminalOutcome::Failed,
                ..
            },
            ..
        }
    )));
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            boundary: ExecutionBoundary::Started {
                time: Some(TimeEvidence {
                    value: 100,
                    unit: TimeUnit::Seconds,
                    basis: TimeBasis::SourceWall,
                    ..
                })
            },
            ..
        }
    )));
    assert!(output[2].claims.iter().any(|claim|matches!(&claim.fact,Fact::Declare{object:TraceObject::Measurement(value)} if value.metric==Metric::ReportedDuration&&value.value==17.into())));
}

#[test]
fn model_call_and_runtime_output_keep_distinct_invocations_and_exact_representations() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-a","name":"exec","input":"  tools.exec_command({cmd:'x'})\n"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":"root","item":{"type":"CommandExecution","id":"exec-a","command":"x","aggregated_output":"FULL OUTPUT\n","status":"completed","exit_code":0}}}),
            serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-a","output":"SHORT OUTPUT\n"}}),
        ],
        false,
    );
    assert_eq!(
        output[1].payloads[0].bytes,
        b"  tools.exec_command({cmd:'x'})\n"
    );
    let runtime = output[2]
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Result(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    let caller = output[3]
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Result(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    assert_ne!(runtime.operation, caller.operation);
    assert_eq!(runtime.representation, Representation::Runtime);
    assert_eq!(caller.representation, Representation::CallerResult);
    assert_eq!(output[3].payloads[0].bytes, b"SHORT OUTPUT\n");
    assert!(!output
        .iter()
        .flat_map(|out| &out.claims)
        .any(|claim| matches!(
            claim.fact,
            Fact::Relation {
                relation: Relation::Nesting { .. }
            }
        )));
}

#[test]
fn repeated_equal_message_occurrences_and_unknown_parts_are_retained() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"same"},{"type":"future-part","opaque":"kept"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"same"}]}}),
        ],
        false,
    );
    let messages: Vec<_> = output
        .iter()
        .flat_map(|out| &out.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Message(value),
            } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0].message_ref, messages[1].message_ref);
    assert_eq!(messages[0].author, Author::Unknown);
    assert_eq!(output[1].payloads[0].bytes, b"same");
    assert_eq!(
        serde_json::from_slice::<Value>(&output[1].payloads[1].bytes).unwrap()["opaque"],
        "kept"
    );
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::UnsupportedKind,
                ..
            }
        }
    )));
}

#[test]
fn stream_append_and_final_replacement_keep_separate_immutable_revisions() {
    let output = run(
        &[
            serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
            serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"turn-a","itemId":"message-a","delta":"A"}}),
            serde_json::json!({"method":"item/completed","params":{"threadId":"root","turnId":"turn-a","item":{"type":"agentMessage","id":"message-a","text":"BC","phase":"final_answer"}}}),
        ],
        true,
    );
    let revisions: Vec<_> = output
        .iter()
        .flat_map(|out| &out.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::MessageRevision(value),
            } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 3);
    assert_ne!(revisions[0].revision_ref, revisions[1].revision_ref);
    assert_eq!(output[0].payloads[0].bytes, b"A");
    assert_eq!(output[1].payloads[0].bytes, b"A");
    assert_eq!(output[2].payloads[0].bytes, b"BC");
    assert_eq!(
        revisions[1].form,
        RevisionForm::Append {
            predecessor: Some(revisions[0].revision_ref.clone())
        }
    );
    assert!(matches!(revisions[2].form, RevisionForm::Final { .. }));
}

#[test]
fn compaction_replacement_is_retained_without_claiming_exact_model_context() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"compacted","payload":{"replacement_history":[{"type":"message","content":[{"type":"input_text","text":"retained summary"}]}]}}),
        ],
        false,
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output[1].payloads[0].bytes).unwrap()[0]["content"][0]
            ["text"],
        "retained summary"
    );
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::ContextCheckpoint(ContextCheckpoint {
                content: ContextContent::OpaqueReplacement { .. },
                coverage: InclusionCoverage::Unknown { .. },
                ..
            })
        }
    )));
}

#[test]
fn recorded_live_message_refines_final_metadata_without_conflicting_declaration() {
    let fixture = include_str!("../../../../../docs/measurements/codex/captures/s02_turn.jsonl");
    let frames: Vec<_> = fixture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["dir"] == "in")
        .map(|record| record["frame"].clone())
        .collect();
    let output = run(&frames, true);
    let reduced = reduce(&output);
    let owner = agent("01a0460c-c359-7ed3-9e13-212c0a33d6c0");
    let reference = MessageRef::native(
        owner,
        "codex.message",
        vec!["msg_08ee54e1b830ae33016a90e8af266887d1b1a01d1a3d731110".into()],
    )
    .unwrap();
    let object = &reduced.objects[&AnyObjectRef::Message(reference.clone())];
    let Some(TraceObject::Message(message)) = object.declaration.known() else {
        panic!(
            "recorded start, delta and final metadata must resolve coherently: {:?}",
            object.declaration
        );
    };
    assert_eq!(message.purpose, MessagePurpose::Answer);
    assert_eq!(message.protocol_role, Some(ProtocolRole::Assistant));
    let history = &reduced.messages[&reference];
    assert!(history.sealed);
    let selected = history
        .selected
        .known()
        .expect("the recorded final item selects a coherent immutable body");
    let Some(TraceObject::MessageRevision(revision)) = reduced.objects
        [&AnyObjectRef::MessageRevision(selected.clone())]
        .declaration
        .known()
    else {
        panic!("selected body is declared");
    };
    let payload = output
        .iter()
        .flat_map(|out| &out.payloads)
        .find(|payload| payload.reference == revision.parts[0].payload)
        .unwrap();
    assert_eq!(payload.bytes, b"done");
}

#[test]
fn mapper_checkpoint_roundtrip_matches_cold_source_replay() {
    let fixture =
        include_str!("../../../../../fixtures/native/codex/ordinary-turn-item-completed-era.jsonl");
    let records: Vec<Value> = fixture
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let cold = run(&records, false);
    let mut state = MapperState::default();
    let mut resumed = vec![];
    for (i, record) in records.iter().enumerate() {
        if i % 4 == 0 {
            state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let capture = capture(i as u64 + 1, false);
        let raw = serde_json::to_vec(record).unwrap();
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        state = output.state.clone();
        resumed.push(output);
    }
    assert_eq!(resumed, cold);
    assert_eq!(reduce(&resumed), reduce(&cold));
    let operation=resumed.iter().flat_map(|output|&output.claims).find_map(|claim|match &claim.fact {
        Fact::Declare{object:TraceObject::Operation(operation)} if matches!(operation.operation_ref.address(),ObjectAddress::Native{namespace,key,..} if namespace=="codex.model-call"&&key==&["call_3YCIBrRaFbvxrv58lSNozTuL"])=>Some(operation),
        _=>None,
    }).expect("the recorded exec invocation must survive checkpointing");
    assert_eq!(
        operation.activation.as_ref().map(ActivationRef::address),
        Some(&ObjectAddress::Native {
            scope: agent("01a03c02-4d90-7063-abdb-ebe7f821b74b"),
            namespace: "codex.turn".into(),
            key: vec!["01a03c02-db15-7873-9baa-0941a6c713eb".into()]
        })
    );
}

#[test]
fn runtime_process_state_and_counter_uncertainty_survive_reduction() {
    let output = run(
        &[
            serde_json::json!({"method":"item/started","params":{"threadId":"root","turnId":"turn-a","item":{"type":"commandExecution","id":"exec-a","processId":"handle-1","command":"x","status":"inProgress"}}}),
            serde_json::json!({"method":"turn/completed","params":{"threadId":"root","turn":{"id":"turn-a","status":"completed"}}}),
            serde_json::json!({"method":"thread/tokenUsage/updated","params":{"threadId":"root","tokenUsage":{"total":{"inputTokens":100}}}}),
        ],
        true,
    );
    let reduced = reduce(&output);
    let resource = ResourceRef::native(
        agent("root"),
        "codex.process-creation",
        vec!["exec-a".into(), "handle-1".into()],
    )
    .unwrap();
    assert_eq!(
        reduced.resource_states[&resource]
            .iter()
            .map(|(state, _)| state)
            .collect::<Vec<_>>(),
        [&ResourceState::Running]
    );
    let measurement = output[2]
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Measurement(value),
            } => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(measurement.value, 100.into());
    assert!(matches!(
        measurement.mode,
        MeasurementMode::Cumulative {
            reset_epoch: None,
            ..
        }
    ));
    assert!(reduced
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::UnknownCounterBaseline));
}

#[test]
fn native_and_live_shared_item_id_retain_both_recorded_body_revisions() {
    // Controlled cross-surface pair using the recorded s02 final item identity and
    // bytes. This is an explicit test substitution, not a claim that its native
    // probe file survived with the live capture.
    let owner = "01a0460c-c359-7ed3-9e13-212c0a33d6c0";
    let id = "msg_08ee54e1b830ae33016a90e8af266887d1b1a01d1a3d731110";
    let native = run(
        &[
            serde_json::json!({"type":"session_meta","payload":{"id":owner}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":owner,"item":{"type":"AgentMessage","id":id,"content":[{"type":"Text","text":"done"}],"phase":"final_answer"}}}),
        ],
        false,
    );
    let live = run(
        &[
            serde_json::json!({"method":"item/completed","params":{"threadId":owner,"item":{"type":"agentMessage","id":id,"text":"done","phase":"final_answer"}}}),
        ],
        true,
    );
    let outputs: Vec<_> = native.into_iter().chain(live).collect();
    let reduced = reduce(&outputs);
    let reference = MessageRef::native(agent(owner), "codex.message", vec![id.into()]).unwrap();
    assert!(reduced.objects[&AnyObjectRef::Message(reference.clone())]
        .declaration
        .known()
        .is_some());
    let revisions: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::MessageRevision(revision)) if revision.message == reference => {
                Some(revision)
            }
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 2);
    assert_ne!(revisions[0].revision_ref, revisions[1].revision_ref);
    for revision in revisions {
        let body = outputs
            .iter()
            .flat_map(|out| &out.payloads)
            .find(|body| body.reference == revision.parts[0].payload)
            .unwrap();
        assert_eq!(body.bytes, b"done");
    }
}

#[test]
fn native_routing_keeps_unresolved_author_and_exact_admission_trigger_evidence() {
    let outputs = run(
        &[
            serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"child","session_id":"root","agent_path":"/root/worker"}}),
            serde_json::json!({"type":"event_msg","ordinal":1,"payload":{"type":"task_started","turn_id":"child-turn"}}),
            serde_json::json!({"type":"inter_agent_communication_metadata","ordinal":2,"payload":{"trigger_turn":true}}),
            serde_json::json!({"type":"response_item","ordinal":3,"payload":{"type":"agent_message","id":"assignment-a","author":"/root","recipient":"/root/worker","content":[{"type":"input_text","text":"task bytes"}]}}),
        ],
        false,
    );
    let output = &outputs[3];
    let reduced = reduce(&outputs);
    let message = output
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Message(message),
            } => Some(message),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        message.author,
        Author::Agent {
            agent: OwnerRef::Unresolved { .. }
        }
    ));
    assert_eq!(message.purpose, MessagePurpose::TaskInput);
    assert_eq!(output.payloads[0].bytes, b"task bytes");
    assert!(output.requests.iter().any(|request|matches!(request,ArchiveRequest::Correlation{namespace,key,..} if namespace=="codex.agent-path"&&key==&["/root"])));
    let trigger = output
        .claims
        .iter()
        .find(|claim| {
            matches!(
                claim.fact,
                Fact::Relation {
                    relation: Relation::Trigger { .. }
                }
            )
        })
        .unwrap();
    assert!(trigger
        .evidence
        .contains(&capture(3, false).observation_ref));
    assert_eq!(reduced.delivery_stages.len(), 1);
    assert!(reduced
        .delivery_stages
        .values()
        .flatten()
        .all(|(stage, _)| matches!(stage, DeliveryStage::Admitted { .. })));
}

#[test]
fn absent_header_and_unscoped_malformed_bytes_report_source_diagnostics() {
    let mut capture = capture(1, false);
    capture.declared_subject_refs.clear();
    for raw in [
        b"{bad".as_slice(),
        br#"{"type":"response_item","payload":{"type":"message","id":"a"}}"#.as_slice(),
    ] {
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw,
            state: &MapperState::default(),
            archive: &ArchiveContext::default(),
        });
        assert!(output.claims.is_empty());
        assert_eq!(output.diagnostics.len(), 1);
        assert_eq!(
            output.diagnostics[0].evidence,
            [capture.observation_ref.clone()]
        );
    }
}

#[test]
fn mapping_completeness_names_only_the_actual_record_coordinates() {
    let outputs = run(
        &[
            serde_json::json!({"type":"session_meta","ordinal":214,"payload":{"id":"root","history_base":{"end_ordinal_exclusive":214}}}),
            serde_json::json!({"type":"future_record","ordinal":215,"payload":{"opaque":true}}),
        ],
        false,
    );
    let extents: Vec<_> = outputs[0]
        .claims
        .iter()
        .filter_map(|claim| match &claim.fact {
            Fact::HistoryMappingComplete { extent } => Some(extent),
            _ => None,
        })
        .collect();
    assert_eq!(extents.len(), 2);
    assert!(extents.iter().any(|extent| matches!(
        extent.coordinate,
        HistoryCoordinate::LocalRecordIndex
    ) && extent.start_inclusive == 0
        && extent.end_exclusive == 1));
    assert!(extents.iter().any(|extent| matches!(
        extent.coordinate,
        HistoryCoordinate::VendorOrdinal { .. }
    ) && extent.start_inclusive == 214
        && extent.end_exclusive == 215));
    assert!(!outputs[0].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::HistoryEntry(_)
        }
    )));
    assert!(!outputs[1]
        .claims
        .iter()
        .any(|claim| matches!(claim.fact, Fact::HistoryMappingComplete { .. })));
}
mod accounting;
