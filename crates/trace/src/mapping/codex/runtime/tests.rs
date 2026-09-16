use super::*;
use crate::mapping::{
    captured_payload_key, ArchiveContext, CapturedPayload, CapturedPayloadBinding, MapperState,
};
use serde_json::json;
use sylph_protocol::EnvironmentId;

fn agent(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env:codex:{id}")).unwrap())
}
fn capture(seq: u64) -> Capture {
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
                instance: "runtime".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: seq },
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
        transport_kind: TransportKind::LiveStdout,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}
fn map(
    seq: u64,
    value: Value,
    state: &MapperState,
    attachment: Option<(&str, Value)>,
) -> MapOutput {
    let capture = capture(seq);
    let mut archive = ArchiveContext::default();
    if let Some((field, value)) = attachment {
        let slot = format!("/payload/{field}");
        archive.captured_payloads.insert(
            captured_payload_key(&capture.observation_ref, &slot),
            CapturedPayload {
                bindings: vec![CapturedPayloadBinding {
                    parent: capture.observation_ref.clone(),
                    slot,
                    attachment: CaptureAttachment {
                        locator: "payload.json".into(),
                        source_position: None,
                        observed_version: None,
                        payload_ref: Some(PayloadRef::bytes("b".repeat(64)).unwrap()),
                        availability: AttachmentAvailability::Captured,
                        relation: "runtime".into(),
                    },
                    evidence: vec![capture.observation_ref.clone()],
                }],
                bytes: Some(serde_json::to_vec(&value).unwrap()),
                limited: false,
            },
        );
    }
    CodexMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(&value).unwrap(),
        state,
        archive: &archive,
    })
}
fn runtime(kind: &str, field: &str) -> Value {
    json!({"schema_version":1,"seq":1,"rollout_id":"rollout-a","thread_id":"root","codex_turn_id":"turn-a","wall_time_unix_ms":1000,"payload":{"type":kind,"tool_call_id":"exec-a","status":"completed",field:{"raw_payload_id":"payload-a","kind":{"type":if field == "runtime_payload" {"tool_runtime_event"} else {"tool_result"}},"path":"payload.json"}}})
}
fn operation(output: &MapOutput) -> &Operation {
    output
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Operation(operation),
            } => Some(operation),
            _ => None,
        })
        .unwrap()
}

#[test]
fn bare_runtime_exec_end_without_process_handle_preserves_its_own_output_and_outcome() {
    // ToolRuntimePayload serializes the struct itself; ExecCommandEnd retains command
    // and omits process_id when this was a synchronous shell invocation.
    let output = map(
        1,
        runtime("tool_call_runtime_ended", "runtime_payload"),
        &MapperState::default(),
        Some((
            "runtime_payload",
            json!({"call_id":"exec-a","turn_id":"turn-a","completed_at_ms":1200,"command":["sh","-c","exit 2"],"cwd":"/workspace","parsed_cmd":[],"source":"agent","stdout":"exact stdout\n","stderr":"exact stderr\n","aggregated_output":"combined\n","formatted_output":"caller formatting\n","exit_code":2,"duration":{"secs":0,"nanos":200000000},"status":"failed"}),
        )),
    );
    let operation = operation(&output);
    assert!(matches!(
        operation.category,
        OperationCategory::TerminalIo {
            action: TerminalAction::Launch
        }
    ));
    assert!(output.claims.iter().any(|claim| matches!(&claim.fact, Fact::Execution {execution: ExecutionRef::Operation(reference), boundary: ExecutionBoundary::Outcome {outcome: TerminalOutcome::Failed,..}} if reference == &operation.operation_ref)));
    assert!(output
        .payloads
        .iter()
        .any(|payload| payload.bytes == b"exact stderr\n"));
}

#[test]
fn bare_runtime_spawn_payload_establishes_topology_without_closing_a_child_activation() {
    let output = map(
        1,
        runtime("tool_call_runtime_ended", "runtime_payload"),
        &MapperState::default(),
        Some((
            "runtime_payload",
            json!({"call_id":"exec-a","completed_at_ms":1200,"sender_thread_id":"root","new_thread_id":"child","prompt":"inspect","model":"gpt-test","reasoning_effort":"high","status":{"errored":"child failed later"}}),
        )),
    );
    assert!(output.claims.iter().any(|claim| matches!(&claim.fact, Fact::Relation {relation: Relation::Spawn {parent,child,..}} if parent == &agent("root") && child == &agent("child"))));
    assert!(!output.claims.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Execution {
            execution: ExecutionRef::Activation(_),
            ..
        }
    )));
}

#[test]
fn terminal_write_after_launch_keeps_its_own_input_category_and_existing_resource() {
    let native = |seq, value: Value, state: &MapperState| {
        let mut capture = capture(seq);
        capture.transport_kind = TransportKind::NativeJsonl;
        capture.source_position.locator = SourceLocator::Jsonl {
            byte_start: seq * 100,
            byte_end: seq * 100 + 100,
            record_ordinal: seq - 1,
        };
        CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&value).unwrap(),
            state,
            archive: &ArchiveContext::default(),
        })
    };
    let start = native(
        1,
        json!({"type":"event_msg","payload":{"type":"exec_command_begin","call_id":"exec-a","turn_id":"turn-a","process_id":"7","source":"unified_exec_startup","command":["cat"],"cwd":"/workspace","parsed_cmd":[],"started_at_ms":1000}}),
        &MapperState {
            governing_agent: Some(agent("root")),
            ..MapperState::default()
        },
    );
    let write = native(
        2,
        json!({"type":"event_msg","payload":{"type":"terminal_interaction","call_id":"exec-a","process_id":"7","stdin":"new input\n"}}),
        &start.state,
    );
    let launch = operation(&start);
    let interaction = operation(&write);
    assert!(matches!(
        interaction.category,
        OperationCategory::TerminalIo {
            action: TerminalAction::Write
        }
    ));
    assert_ne!(interaction.operation_ref, launch.operation_ref);
    assert_ne!(interaction.input_revision, launch.input_revision);
    assert!(write
        .payloads
        .iter()
        .any(|payload| String::from_utf8_lossy(&payload.bytes).contains("new input")));
    assert!(write.claims.iter().any(|claim|matches!(&claim.fact,Fact::Relation {relation:Relation::UsesResource {operation,..}} if operation == &interaction.operation_ref)));
    assert!(!write.claims.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Declare {
            object: TraceObject::Resource(_)
        }
    )));
}

#[test]
fn immediate_tool_return_is_a_caller_representation_without_conversation_admission() {
    let output = map(
        1,
        runtime("tool_call_ended", "result_payload"),
        &MapperState::default(),
        Some((
            "result_payload",
            json!({"type":"code_mode_response","value":{"content":[{"type":"text","text":"selected result"}]}}),
        )),
    );
    let result = output
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Result(result),
            } => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.representation, Representation::CallerResult);
    assert!(result.delivery.is_none());
    assert!(output
        .payloads
        .iter()
        .any(|payload| String::from_utf8_lossy(&payload.bytes).contains("selected result")));
}

#[test]
fn live_collaboration_tool_enums_map_spawn_and_followup_without_new_child_runs() {
    for (seq, tool, category) in [
        (1, "spawnAgent", OperationCategory::Delegation),
        (2, "followupTask", OperationCategory::Communication),
    ] {
        let output = map(
            seq,
            json!({"method":"item/completed","params":{"threadId":"root","turnId":"turn-a","item":{"type":"collabAgentToolCall","id":format!("tool-{seq}"),"tool":tool,"status":"completed","senderThreadId":"root","receiverThreadIds":["child"],"prompt":"work","agentsStates":{}}}}),
            &MapperState::default(),
            None,
        );
        assert_eq!(operation(&output).category, category);
        assert_eq!(output.claims.iter().filter(|claim|matches!(&claim.fact,Fact::Relation {relation:Relation::Spawn {child,..}} if child == &agent("child"))).count(),usize::from(seq==1));
        assert!(!output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Declare {object:TraceObject::Activation(activation)} if activation.owner==OwnerRef::Agent {agent:agent("child")})));
    }
}

#[test]
fn collaboration_tool_spelling_is_bound_to_its_vendor_transport() {
    for (seq, tool, delegated) in [(1, "spawn_agent", true), (2, "spawnAgent", false)] {
        let mut capture = capture(seq);
        capture.transport_kind = TransportKind::NativeJsonl;
        capture.source_position.locator = SourceLocator::Jsonl {
            byte_start: seq * 100,
            byte_end: seq * 100 + 100,
            record_ordinal: seq - 1,
        };
        let value = json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CollabAgentToolCall","id":"tool-a","tool":tool,"status":"completed","sender_thread_id":"root","receiver_thread_ids":["child"]}}});
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&value).unwrap(),
            state: &MapperState {
                governing_agent: Some(agent("root")),
                ..MapperState::default()
            },
            archive: &ArchiveContext::default(),
        });
        assert_eq!(
            matches!(operation(&output).category, OperationCategory::Delegation),
            delegated
        );
    }
    let output = map(
        3,
        json!({"method":"item/completed","params":{"threadId":"root","item":{"type":"collabAgentToolCall","id":"tool-a","tool":"spawn_agent","status":"completed","senderThreadId":"root","receiverThreadIds":["child"]}}}),
        &MapperState::default(),
        None,
    );
    assert!(matches!(
        operation(&output).category,
        OperationCategory::Extension { .. }
    ));
}

#[test]
fn bare_runtime_patch_and_mcp_boundaries_keep_typed_work_and_observed_failure() {
    for (value, expected) in [
        (
            json!({"call_id":"exec-a","turn_id":"turn-a","stdout":"applied nothing","stderr":"rejected","success":false,"changes":{"src/main.rs":{"type":"add","content":"new content"}},"status":"failed"}),
            OperationCategory::Edit,
        ),
        (
            json!({"call_id":"exec-a","invocation":{"server":"tools","tool":"lookup","arguments":{"query":"exact"}},"duration":{"secs":1,"nanos":0},"result":{"Err":"offline"}}),
            OperationCategory::Tool {
                namespace: Some("tools".into()),
                name: "lookup".into(),
            },
        ),
    ] {
        let output = map(
            1,
            runtime("tool_call_runtime_ended", "runtime_payload"),
            &MapperState::default(),
            Some(("runtime_payload", value)),
        );
        let mapped = operation(&output);
        assert_eq!(mapped.category, expected);
        assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Execution {execution:ExecutionRef::Operation(reference),boundary:ExecutionBoundary::Outcome {outcome:TerminalOutcome::Failed,..}} if reference == &mapped.operation_ref)));
    }
}

#[test]
fn mismatched_runtime_payload_identity_cannot_create_another_operation() {
    let output = map(
        1,
        runtime("tool_call_runtime_ended", "runtime_payload"),
        &MapperState::default(),
        Some((
            "runtime_payload",
            json!({"call_id":"different-call","turn_id":"turn-a","command":["false"],"status":"failed"}),
        )),
    );
    assert!(!output.claims.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Declare {
            object: TraceObject::Operation(_)
        }
    )));
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic {diagnostic} if diagnostic.code==DiagnosticCode::ConflictingOwner)));
}

#[test]
fn bare_subagent_activity_identifies_a_peer_without_inventing_its_spawn_or_run() {
    let output = map(
        1,
        runtime("tool_call_runtime_ended", "runtime_payload"),
        &MapperState::default(),
        Some((
            "runtime_payload",
            json!({"event_id":"exec-a","occurred_at_ms":1000,"agent_thread_id":"child","agent_path":"/root/worker","kind":"interacted"}),
        )),
    );
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Declare {object:TraceObject::Agent(child)} if child.agent_ref==agent("child"))));
    assert!(!output.claims.iter().any(|claim| matches!(
        &claim.fact,
        Fact::Relation {
            relation: Relation::Spawn { .. }
        } | Fact::Declare {
            object: TraceObject::Activation(_)
        } | Fact::Execution {
            execution: ExecutionRef::Activation(_),
            ..
        }
    )));
}
