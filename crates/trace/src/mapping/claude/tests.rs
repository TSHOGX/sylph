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
        vendor_build: Some("2.1.272".into()),
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
        let output = ClaudeMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(value).unwrap(),
            state: &state,
            archive: &archive,
        });
        state = serde_json::from_slice(&serde_json::to_vec(&output.state).unwrap()).unwrap();
        for claim in &output.claims {
            if let Fact::Declare { object: TraceObject::Operation(operation) } = &claim.fact {
                archive.semantic_mut().operations.insert(operation.operation_ref.clone(), operation.clone());
            }
        }
        archive.correlations.extend(output.correlations.clone());
        archive
            .correlation_evidence
            .extend(output.correlation_evidence.clone());
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
    for chunk in claims.chunks(7) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    let result = reducer.resolve();
    for chunk in claims.chunks(3) {
        reducer.apply(chunk, &evidence, 100).unwrap();
    }
    assert_eq!(result, reducer.resolve());
    result
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
fn owner(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env:claude:{id}")).unwrap())
}

#[test]
fn shared_model_message_id_preserves_four_record_uuids_and_four_part_zeroes() {
    let records: Vec<Value> =
        include_str!("../../../../../fixtures/native/claude/ordinary-turn-session-start.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&records, false);
    let expected = [
        "1398c0ab-b7b2-4b5a-ba14-b4d572235007",
        "1ac71385-50c0-4f6b-a73d-073dff015f51",
        "dc476707-e551-45a9-9e8c-fd265e475347",
        "10785cad-7f5c-467d-b1a0-32a09f39ef1b",
    ];
    for uuid in expected {
        assert!(objects(&output).any(|object|matches!(object,TraceObject::Message(message)if matches!(message.message_ref.address(),ObjectAddress::Native{key,..}if key==&[uuid]))));
        assert!(objects(&output).any(|object|matches!(object,TraceObject::MessageRevision(revision)if revision.parts.iter().any(|part|matches!(part.part_ref.address(),ObjectAddress::Native{key,..}if key==&[uuid,"0"])))));
    }
    assert!(!objects(&output).any(|object| matches!(object, TraceObject::Activation(_))));
    assert!(reduce(&output)
        .histories
        .values()
        .all(|history| history.selected_entries.is_none()));
}

#[test]
fn native_meta_and_sdk_input_do_not_borrow_human_authorship() {
    let values: Vec<Value> = include_str!(
        "../../../../../fixtures/native/claude/injected-boilerplate-slash-command.jsonl"
    )
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
    let output = run(&values, false);
    let by_id = |id: &str| {
        objects(&output).find_map(|object|match object{TraceObject::Message(message)if matches!(message.message_ref.address(),ObjectAddress::Native{key,..}if key==&[id])=>Some(message),_=>None}).unwrap()
    };
    assert!(matches!(
        by_id("e7613dc0-1032-4c99-9998-0b3b73c2b686").author,
        Author::Harness { .. }
    ));
    assert_eq!(
        by_id("e7613dc0-1032-4c99-9998-0b3b73c2b686").purpose,
        MessagePurpose::Context
    );
    assert_eq!(
        by_id("050fe50f-41ad-4257-81e4-e0bf1fdf79b1").author,
        Author::Unknown
    );
    assert!(!objects(&output).any(|object| matches!(object, TraceObject::Activation(_))));
}

#[test]
fn actual_sdk_and_native_pair_share_record_identity_and_exact_marker_bytes() {
    let native: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/claude/native-basic.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let live: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/claude/live-basic.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output: Vec<_> = run(&native, false)
        .into_iter()
        .chain(run(&live, true))
        .collect();
    let reference = MessageRef::native(
        owner("2ea06d27-a87f-458a-9724-9bafa7d64ba6"),
        "claude.record",
        vec!["1150bf8e-6198-4ecb-91f4-32a22bca04cd".into()],
    )
    .unwrap();
    let reduced = reduce(&output);
    assert!(reduced.objects[&AnyObjectRef::Message(reference.clone())]
        .declaration
        .known()
        .is_some());
    let revisions: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::MessageRevision(revision) if revision.message == reference => {
                Some(revision)
            }
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 2);
    for revision in revisions {
        let payload = output
            .iter()
            .flat_map(|out| &out.payloads)
            .find(|body| body.reference == revision.parts[0].payload)
            .unwrap();
        assert_eq!(payload.bytes, b"SYLPH_CLAUDE_TRACE_OK");
    }
    assert_eq!(
        reduced
            .executions
            .values()
            .filter(|execution| execution.outcome.known() == Some(&TerminalOutcome::Succeeded))
            .count(),
        1
    );
}

#[test]
fn explicit_agent_result_creates_child_while_argument_sealing_does_not_close_it() {
    let values: Vec<Value> =
        include_str!("../../../../../fixtures/native/claude/delegation-task-fanout-nested.jsonl")
            .lines()
            .take(2)
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&values, false);
    assert!(!objects(&output[..1]).any(|object| matches!(
        object,
        TraceObject::Agent(AgentThread {
            agent_ref: AgentRef::Embedded { .. },
            ..
        })
    )));
    let child = objects(&output[1..])
        .find_map(|object| match object {
            TraceObject::Agent(AgentThread {
                agent_ref:
                    AgentRef::Embedded {
                        native_child_id, ..
                    },
                ..
            }) => Some(native_child_id),
            _ => None,
        })
        .unwrap();
    assert_eq!(child, "af9bb3b1475445e7d");
    assert!(output[1].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Relation {
            relation: Relation::Spawn { .. }
        }
    )));
    assert!(!objects(&output).any(|object| matches!(object, TraceObject::Activation(_))));
}

#[test]
fn task_failure_and_parent_success_remain_independent_scopes() {
    let output = run(
        &[
            serde_json::json!({"type":"stream_event","session_id":"root","event":{"type":"message_start","message":{"id":"model"}}}),
            serde_json::json!({"type":"system","subtype":"task_started","session_id":"root","task_id":"task-a","task_type":"local_agent"}),
            serde_json::json!({"type":"system","subtype":"task_notification","session_id":"root","task_id":"task-a","status":"failed"}),
            serde_json::json!({"type":"result","session_id":"root","subtype":"success","is_error":false,"terminal_reason":"completed"}),
        ],
        true,
    );
    let reduced = reduce(&output);
    assert_eq!(
        reduced.executions.values().next().unwrap().outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert!(reduced
        .resource_states
        .values()
        .flatten()
        .any(|(state, _)| matches!(
            state,
            ResourceState::Closed {
                outcome: TerminalOutcome::Failed,
                ..
            }
        )));
    assert!(!objects(&output).any(|object| matches!(
        object,
        TraceObject::Agent(AgentThread {
            agent_ref: AgentRef::Embedded { .. },
            ..
        })
    )));
}

#[test]
fn sidecar_ranges_remain_distinct_from_the_caller_result() {
    let value = serde_json::json!({"type":"user","sessionId":"root","uuid":"result","parentUuid":"call","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-a","content":"short caller bytes"}]},"toolUseResult":{"persistedOutputPath":"/captured/output","persistedOutputSize":9000}});
    let mut capture = capture(0, false);
    let payload = PayloadRef::manifest("b".repeat(64)).unwrap();
    capture.attachments.push(CaptureAttachment {
        locator: "/captured/output".into(),
        source_position: None,
        observed_version: Some("revision-a".into()),
        payload_ref: Some(payload.clone()),
        availability: AttachmentAvailability::Partial {
            reason: "retained byte range 0..256".into(),
        },
        relation: "persisted_output".into(),
    });
    let output = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(&value).unwrap(),
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert!(objects(std::slice::from_ref(&output)).any(|object|matches!(object,TraceObject::Result(ResultOccurrence{representation:Representation::Runtime,payload:Some(reference),..})if reference==&payload)));
    assert!(output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Coverage {
            coverage: Coverage {
                dimension: CoverageDimension::PayloadRanges,
                state: CoverageState::Gap { .. },
                ..
            }
        }
    )));
    assert!(output
        .payloads
        .iter()
        .any(|body| serde_json::from_slice::<Value>(&body.bytes)
            .ok()
            .is_some_and(|value| value["content"] == "short caller bytes")));
}

#[test]
fn copied_fork_records_keep_each_original_author_without_inventing_a_cutoff() {
    let values: Vec<Value> = include_str!(
        "../../../../../fixtures/native/claude/fork-replay-messageuuid-mirrors-own-uuid.jsonl"
    )
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
    let output = run(&values, false);
    let message=objects(&output).find_map(|object|match object{TraceObject::Message(message)if matches!(message.message_ref.address(),ObjectAddress::Native{key,..}if key==&["88306788-b6ec-4093-ab79-bef41e20b995"])=>Some(message),_=>None}).unwrap();
    assert_eq!(
        message.message_ref.scope(),
        &owner("bba7f74d-801e-4a61-b814-3091cafeb904")
    );
    assert!(objects(&output).any(|object|matches!(object,TraceObject::HistoryEntry(HistoryEntry{original_owner:Some(OwnerRef::Agent{agent}),..})if agent==&owner("bba7f74d-801e-4a61-b814-3091cafeb904"))));
    assert!(!output
        .iter()
        .flat_map(|out| &out.claims)
        .any(|claim| matches!(
            claim.fact,
            Fact::SelectHistory { .. }
                | Fact::Relation {
                    relation: Relation::Spawn { .. }
                }
        )));
}

#[test]
fn decoded_image_retains_bytes_mime_and_binary_encoding() {
    let output = run(
        &[
            serde_json::json!({"type":"user","sessionId":"root","uuid":"image-a","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}]}}),
        ],
        false,
    );
    let revision = objects(&output)
        .find_map(|object| match object {
            TraceObject::MessageRevision(revision) => Some(revision),
            _ => None,
        })
        .unwrap();
    assert_eq!(revision.parts[0].kind, ContentKind::Image);
    assert_eq!(revision.parts[0].media_type.as_deref(), Some("image/png"));
    assert_eq!(revision.parts[0].encoding, PayloadEncoding::Binary);
    assert_eq!(output[0].payloads[0].bytes, [1, 2, 3]);
    assert_eq!(output[0].payloads[0].media_type, "image/png");
}

#[test]
fn unresolved_forwarded_host_never_becomes_a_root_or_phantom_child_message() {
    let output = run(
        &[
            serde_json::json!({"type":"assistant","session_id":"root","parent_tool_use_id":"host-tool","uuid":"child-record","message":{"role":"assistant","content":[{"type":"text","text":"child bytes"}]}}),
        ],
        true,
    );
    assert!(objects(&output).next().is_none());
    assert_eq!(
        output[0].requests,
        [ArchiveRequest::Correlation {
            agent: owner("root"),
            namespace: "claude.parent-tool-use".into(),
            key: vec!["host-tool".into()]
        }]
    );
}

#[test]
fn large_payload_descriptor_is_fixed_before_manifest_identity() {
    let capture = capture(0, true);
    let state = MapperState::default();
    let archive = ArchiveContext::default();
    let input = MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: b"{}",
        state: &state,
        archive: &archive,
    };
    let bytes = vec![b'A'; crate::mapping::DERIVED_PAYLOAD_CHUNK_BYTES + 1];
    let binary = derived_payload_with_media(
        &input,
        bytes.clone(),
        "/data",
        "fixture/declared-media",
        "application/octet-stream",
        PayloadEncoding::Binary,
    );
    let text = derived_payload_with_media(
        &input,
        bytes.clone(),
        "/data",
        "fixture/declared-media",
        "text/plain",
        PayloadEncoding::Utf8,
    );
    assert_ne!(binary.reference, text.reference);
    assert_eq!(
        binary.manifest.as_ref().unwrap().media_type,
        "application/octet-stream"
    );
    assert_eq!(
        binary.manifest.as_ref().unwrap().encoding,
        PayloadEncoding::Binary
    );
    assert_eq!(text.manifest.as_ref().unwrap().media_type, "text/plain");
    assert_eq!(
        text.manifest.as_ref().unwrap().encoding,
        PayloadEncoding::Utf8
    );
    assert_eq!(binary.bytes, bytes);
    assert_eq!(text.bytes, bytes);
}

#[test]
fn nonquerying_synthetic_sdk_input_is_admitted_without_human_or_execution_claims() {
    let output = run(
        &[
            serde_json::json!({"type":"user","session_id":"root","uuid":"synthetic-a","isSynthetic":true,"shouldQuery":false,"message":{"role":"user","content":[{"type":"text","text":"automatic context"}]}}),
        ],
        true,
    );
    let message = objects(&output)
        .find_map(|object| match object {
            TraceObject::Message(message) => Some(message),
            _ => None,
        })
        .unwrap();
    assert!(matches!(message.author, Author::Harness { .. }));
    assert_eq!(output[0].payloads[0].bytes, b"automatic context");
    assert!(!objects(&output).any(|object| matches!(object, TraceObject::Activation(_))));
    assert!(output[0].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Admitted { .. },
            ..
        }
    )));
    assert!(!output[0].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Included { .. },
            ..
        }
    )));
}

#[test]
fn compaction_retains_membership_metadata_without_admitting_transcript_only_summary() {
    let values: Vec<Value> =
        include_str!("../../../../../fixtures/native/claude/compaction-boundary.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&values, false);
    let checkpoint = objects(&output)
        .find_map(|object| match object {
            TraceObject::ContextCheckpoint(checkpoint) => Some(checkpoint),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        checkpoint.coverage,
        InclusionCoverage::Unknown { .. }
    ));
    let ContextContent::OpaqueReplacement { payload } = &checkpoint.content else {
        panic!("compaction retains its structured source metadata");
    };
    let body = output
        .iter()
        .flat_map(|out| &out.payloads)
        .find(|body| &body.reference == payload)
        .unwrap();
    let value: Value = serde_json::from_slice(&body.bytes).unwrap();
    assert_eq!(value["compactMetadata"]["preTokens"], 178564);
    assert_eq!(
        value["logicalParentUuid"],
        "e117dff3-3ce2-490c-b927-f02cd2c7f7d8"
    );
    assert!(!output[4]
        .claims
        .iter()
        .any(|claim| matches!(claim.fact, Fact::DeliveryStage { .. })));
    assert!(objects(&output[4..5]).any(|object| matches!(
        object,
        TraceObject::Message(Message {
            author: Author::Harness { .. },
            purpose: MessagePurpose::Context,
            ..
        })
    )));
}

#[test]
fn delegate_metadata_uses_proven_child_identity_and_explicit_parent_agent() {
    let value:Value=serde_json::from_str(include_str!("../../../../../fixtures/native/claude/delegation-task-fanout-nested.agent-afcf7613795f5a25b.meta.json")).unwrap();
    let root = owner("2d9f8dce-716c-40d8-8e38-86de871caa1e");
    let child = Context::child(&root, "afcf7613795f5a25b");
    let mut capture = capture(0, false);
    capture.transport_kind = TransportKind::NativeSnapshot;
    capture.declared_subject_refs = vec![child.clone()];
    let raw = serde_json::to_vec(&value).unwrap();
    let state = MapperState::default();
    let mut archive = ArchiveContext::default();
    let missing = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &raw,
        state: &state,
        archive: &archive,
    });
    assert!(objects(std::slice::from_ref(&missing)).next().is_none());
    assert_eq!(
        missing.requests,
        [ArchiveRequest::Source {
            source: capture.source_position.source.clone(),
            generation: 0
        }]
    );
    let evidence = ObservationRef {
        producer_seq: 99,
        ..capture.observation_ref.clone()
    };
    archive.observations.insert(evidence.clone());
    archive.source_owners.insert(
        (
            capture.source_position.source.clone(),
            capture.source_position.generation,
        ),
        crate::mapping::SourceOwner {
            agent: child.clone(),
            evidence: vec![evidence.clone()],
            rule: "fixture/retained-native-child-source".into(),
        },
    );
    let output = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &raw,
        state: &state,
        archive: &archive,
    });
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Relation{relation:Relation::Spawn{parent,child:actual,..}} if parent==&Context::child(&root,"af9bb3b1475445e7d")&&actual==&child)));
    assert!(output
        .claims
        .iter()
        .all(|claim| claim.evidence.contains(&evidence)));
    assert!(objects(std::slice::from_ref(&output)).any(|object|matches!(object,TraceObject::Agent(AgentThread{name:Some(name),..})if name=="sock-probe")));
}

#[test]
fn compaction_membership_uses_the_explicit_preserved_uuid_set() {
    let values: Vec<Value> =
        include_str!("../../../../../fixtures/native/claude/compaction-boundary.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&values, false);
    let reduced = reduce(&output);
    let selection = objects(&output)
        .find_map(|object| match object {
            TraceObject::HistorySelection(selection) => Some(selection),
            _ => None,
        })
        .unwrap();
    let SelectionMembership::EntrySet { entries } = &selection.membership else {
        panic!("explicit UUID list defines exact membership");
    };
    let expected = [
        "979e6a94-a536-470f-b05f-a915143df1cf",
        "e117dff3-3ce2-490c-b927-f02cd2c7f7d8",
    ];
    assert_eq!(entries.len(), expected.len());
    for (entry, id) in entries.iter().zip(expected) {
        assert!(matches!(entry.address(),ObjectAddress::Native{key,..}if key==&[id]));
    }
    assert!(reduced.relations.iter().any(|relation| matches!(
        relation.relation,
        Relation::ContextMembership {
            disposition: ContextMembershipKind::Preserved,
            ..
        }
    )));
    assert!(reduced
        .histories
        .values()
        .all(|history| history.selected_entries.is_none()));
}

#[test]
fn absent_text_field_is_opaque_while_explicit_empty_text_remains_empty() {
    let output = run(
        &[
            serde_json::json!({"type":"user","sessionId":"root","uuid":"missing","message":{"role":"user","content":[{"type":"text"}]}}),
            serde_json::json!({"type":"user","sessionId":"root","uuid":"empty","message":{"role":"user","content":[{"type":"text","text":""}]}}),
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
    assert_eq!(output[1].payloads[0].bytes, b"");
    assert_eq!(
        serde_json::from_slice::<Value>(&output[0].payloads[0].bytes).unwrap()["type"],
        "text"
    );
    assert!(output[0].claims.iter().any(|claim| matches!(
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
fn sdk_reports_usd_cost_and_reasoning_without_turning_tokens_into_money() {
    let output = run(
        &[
            serde_json::json!({"type":"stream_event","session_id":"root","event":{"type":"message_start","message":{"id":"model"}}}),
            serde_json::json!({"type":"result","session_id":"root","subtype":"success","is_error":false,"total_cost_usd":3,"usage":{"input_tokens":100,"output_tokens":11,"cache_creation_input_tokens":7,"output_tokens_details":{"thinking_tokens":5}}}),
        ],
        true,
    );
    let cost = objects(&output)
        .find_map(|object| match object {
            TraceObject::Measurement(measurement)
                if matches!(measurement.metric, Metric::Cost { .. }) =>
            {
                Some(measurement)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(cost.value, 3.into());
    assert_eq!(cost.unit, MeasurementUnit::Currency { code: "USD".into() });
    assert!(objects(&output).any(|object|matches!(object,TraceObject::Measurement(measurement)if measurement.metric==Metric::ReasoningTokens&&measurement.value==5.into())));
    assert!(objects(&output).any(|object|matches!(object,TraceObject::Measurement(measurement)if matches!(&measurement.metric,Metric::Extension{name,..}if name=="cache_creation_input_tokens")&&measurement.value==7.into())));
}

#[test]
fn intrinsic_source_owner_comes_from_initial_native_fields_not_routing_hints() {
    let value = serde_json::json!({"type":"assistant","sessionId":"root","agentId":"child-a","uuid":"record","message":{"role":"assistant","content":"child bytes"}});
    let output = run(
        &[
            value.clone(),
            serde_json::json!({"type":"assistant","sessionId":"root","agentId":"child-b","uuid":"later","message":{"role":"assistant","content":"other child"}}),
        ],
        false,
    );
    assert_eq!(output[0].source_owners.len(), 1);
    assert_eq!(
        output[0].source_owners[0].owner.agent,
        Context::child(&owner("root"), "child-a")
    );
    assert!(output[1].source_owners.is_empty());
    assert_eq!(
        output[0].source_owners[0].owner.evidence,
        [capture(0, false).observation_ref]
    );
    let live = run(
        &[
            serde_json::json!({"type":"assistant","session_id":"root","uuid":"record","message":{"role":"assistant","content":"bytes"}}),
        ],
        true,
    );
    assert!(live[0].source_owners.is_empty());
}

fn stream_event(event: Value) -> Value {
    serde_json::json!({"type":"stream_event","session_id":"root","parent_tool_use_id":null,"event":event})
}
fn part_bodies(parts: &[PartSummary]) -> Vec<(&MessagePartRef, ContentKind, &str)> {
    parts
        .iter()
        .map(|part| (&part.part_ref, part.kind, part.preview.as_str()))
        .collect()
}
fn stream_reading(outputs: &[MapOutput]) -> Vec<PartSummary> {
    let mut previews = crate::reading::PayloadPreviews::new();
    for payload in outputs.iter().flat_map(|output| &output.payloads) {
        previews.insert(
            payload.reference.clone(),
            std::sync::Arc::new(crate::reading::PayloadPreview {
                chunks: payload
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.chunks.clone()),
                preview_limit: 4096,
                preview: String::from_utf8_lossy(&payload.bytes).into_owned(),
                total_bytes: Some(payload.bytes.len() as u64),
                availability: Some(ReadAvailability::Available),
                ..Default::default()
            }),
        );
    }
    let projection = crate::reading::project(&reduce(outputs), &previews, 32);
    let messages: Vec<_> = projection
        .windows
        .values()
        .flatten()
        .filter_map(|unit| match &unit.content {
            ReadingContent::Message { parts, .. } => Some(parts.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "one unfinished model message must be readable"
    );
    messages.into_iter().next().unwrap()
}

#[test]
fn captured_claude_deltas_are_readable_before_the_sdk_assistant_record_arrives() {
    let values: Vec<Value> =
        include_str!("../../../../../fixtures/unified-trace/claude/live-basic.jsonl")
            .lines()
            .take(8)
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let outputs = run(&values, true);
    let parts = stream_reading(&outputs);
    assert_eq!(parts[0].preview, "SYLPH_CLAUDE_TRACE_OK");
    assert!(!outputs
        .iter()
        .flat_map(|output| &output.claims)
        .any(|claim| matches!(claim.fact, Fact::DeliveryStage { .. })));
}

#[test]
fn live_initial_text_repeated_deltas_and_multiple_parts_survive_the_final_snapshot() {
    let mut events = vec![
        stream_event(
            serde_json::json!({"type":"message_start","message":{"id":"msg-a","role":"assistant","content":[]}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"seed "}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"A"}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"A"}}),
        ),
        stream_event(serde_json::json!({"type":"content_block_stop","index":0})),
        stream_event(
            serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"second"}}),
        ),
    ];
    let partial = stream_reading(&run(&events, true));
    assert_eq!(
        partial
            .iter()
            .map(|part| part.preview.as_str())
            .collect::<Vec<_>>(),
        ["seed AA", "second"]
    );
    assert_ne!(partial[0].part_ref, partial[1].part_ref);
    events.push(stream_event(serde_json::json!({"type":"message_stop"})));
    let complete = stream_reading(&run(&events, true));
    assert_eq!(part_bodies(&complete), part_bodies(&partial));
}

#[test]
fn thinking_signatures_replace_opaque_metadata_without_appending_to_reasoning() {
    let events = vec![
        stream_event(
            serde_json::json!({"type":"message_start","message":{"id":"msg-a","content":[]}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"initial ","signature":""}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thought"}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"first-signature"}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"replacement-signature"}}),
        ),
    ];
    let outputs = run(&events, true);
    let parts = stream_reading(&outputs);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].kind, ContentKind::DisclosedReasoning);
    assert_eq!(parts[0].preview, "initial thought");
    assert_eq!(parts[1].kind, ContentKind::Opaque);
    assert_eq!(parts[1].preview, "replacement-signature");
    assert_ne!(parts[0].part_ref, parts[1].part_ref);
    let mut final_events = events;
    final_events.push(stream_event(serde_json::json!({"type":"message_stop"})));
    assert_eq!(
        part_bodies(&stream_reading(&run(&final_events, true))),
        part_bodies(&parts)
    );
    assert!(outputs
        .iter()
        .flat_map(|output| &output.payloads)
        .any(|payload| payload.bytes == b"first-signature"));
}

#[test]
fn streamed_tool_json_is_sealed_as_input_without_claiming_execution_or_admission() {
    let mut events = vec![
        stream_event(
            serde_json::json!({"type":"message_start","message":{"id":"msg-a","content":[]}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-a","name":"Read","input":{}}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\""}}),
        ),
        stream_event(
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"file.txt\"}"}}),
        ),
        stream_event(serde_json::json!({"type":"content_block_stop","index":0})),
    ];
    let sealed = stream_reading(&run(&events, true));
    let value: Value = serde_json::from_str(&sealed[0].preview).unwrap();
    assert_eq!(value["input"], serde_json::json!({"path":"file.txt"}));
    events.push(stream_event(serde_json::json!({"type":"message_stop"})));
    let outputs = run(&events, true);
    assert_eq!(part_bodies(&stream_reading(&outputs)), part_bodies(&sealed));
    assert!(!outputs
        .iter()
        .skip(1)
        .flat_map(|output| &output.claims)
        .any(|claim| matches!(
            claim.fact,
            Fact::Execution { .. } | Fact::DeliveryStage { .. }
        )));
}

#[test]
fn invalid_final_tool_json_is_opaque_instead_of_a_plausible_empty_input() {
    let outputs = run(
        &[
            stream_event(
                serde_json::json!({"type":"message_start","message":{"id":"msg-a","content":[]}}),
            ),
            stream_event(
                serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-a","name":"Read","input":{}}}),
            ),
            stream_event(
                serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            ),
            stream_event(serde_json::json!({"type":"content_block_stop","index":0})),
            stream_event(serde_json::json!({"type":"message_stop"})),
        ],
        true,
    );
    let parts = stream_reading(&outputs);
    assert_eq!(parts[0].kind, ContentKind::Opaque);
    let value: Value = serde_json::from_str(&parts[0].preview).unwrap();
    assert!(value.get("input").is_none());
    assert_eq!(value["partial_json"], "{\"path\":");
    assert!(outputs.iter().flat_map(|output|&output.claims).any(|claim|matches!(&claim.fact,Fact::Diagnostic {diagnostic} if diagnostic.code==DiagnosticCode::DecodeFailure)));
}

#[test]
fn interleaved_live_messages_keep_their_owners_and_same_index_parts_distinct() {
    let envelope = |session: &str, event: Value| serde_json::json!({"type":"stream_event","session_id":session,"event":event});
    let outputs = run(
        &[
            envelope(
                "root-a",
                serde_json::json!({"type":"message_start","message":{"id":"same-model-id","content":[]}}),
            ),
            envelope(
                "root-b",
                serde_json::json!({"type":"message_start","message":{"id":"same-model-id","content":[]}}),
            ),
            envelope(
                "root-a",
                serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"owner A"}}),
            ),
            envelope(
                "root-b",
                serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"owner B"}}),
            ),
            envelope("root-a", serde_json::json!({"type":"message_stop"})),
            envelope("root-b", serde_json::json!({"type":"message_stop"})),
        ],
        true,
    );
    let messages: Vec<_> = objects(&outputs)
        .filter_map(|object| match object {
            TraceObject::Message(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0].message_ref, messages[1].message_ref);
    for (message, expected) in messages.iter().zip([owner("root-a"), owner("root-b")]) {
        assert_eq!(
            message.author,
            Author::Agent {
                agent: OwnerRef::Agent { agent: expected }
            }
        );
    }
    let final_parts: Vec<_> = objects(&outputs)
        .filter_map(|object| match object {
            TraceObject::MessageRevision(revision)
                if matches!(revision.form, RevisionForm::Final { .. }) =>
            {
                Some(revision.parts[0].part_ref.clone())
            }
            _ => None,
        })
        .collect();
    assert_ne!(final_parts[0], final_parts[1]);
    let final_payload = &outputs[4].payloads[0];
    assert!(final_payload
        .evidence
        .contains(&capture(0, true).observation_ref));
    assert!(final_payload
        .evidence
        .contains(&capture(2, true).observation_ref));
    assert!(!final_payload
        .evidence
        .contains(&capture(3, true).observation_ref));
    assert_eq!(reduce(&outputs).histories.len(), 2);
}

mod workflow;
mod tool_results;

mod schedule;
