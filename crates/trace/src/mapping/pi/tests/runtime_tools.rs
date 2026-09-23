use super::*;

const LIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/pi/tool-sidecar/stdout.jsonl");
fn frames(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn recorded_reused_tool_id_keeps_two_live_invocations_and_their_own_results() {
    let rows = frames(LIVE);
    let output = run(&rows, true);
    let reduced = reduce(&output);
    let tools:Vec<_>=reduced.objects.values().filter_map(|object|match object.declaration.known(){Some(TraceObject::Operation(operation))if matches!(&operation.category,OperationCategory::Tool{namespace:Some(namespace),name}if namespace=="pi"&&name=="bash")=>Some(operation),_=>None}).collect();
    let executed: Vec<_> = tools
        .iter()
        .filter(|op| {
            reduced
                .execution(&AnyObjectRef::Operation(op.operation_ref.clone()))
                .is_some_and(|execution| !execution.started.is_empty())
        })
        .collect();
    assert_eq!(
        executed.len(),
        2,
        "one reused toolCallId is not one invocation for the producer epoch"
    );
    assert_ne!(executed[0].operation_ref, executed[1].operation_ref);
    for operation in executed {
        assert_eq!(
            reduced
                .execution(&AnyObjectRef::Operation(operation.operation_ref.clone()))
                .unwrap()
                .outcome
                .known(),
            Some(&TerminalOutcome::Succeeded)
        );
        let results: Vec<_> = objects(&output)
            .filter_map(|object| match object {
                TraceObject::Result(result) if result.operation == operation.operation_ref => {
                    Some(result)
                }
                _ => None,
            })
            .collect();
        assert!(results.iter().any(|result| result.representation
            == ResultRepresentation::Runtime
            && result.disposition == ResultDisposition::FinalReturn));
        assert!(
            results
                .iter()
                .any(|result| result.representation == ResultRepresentation::CallerResult)
        );
    }
}

fn start() -> Value {
    serde_json::json!({"type":"tool_execution_start","toolCallId":"reused","toolName":"bash","args":{"command":"fixture"}})
}
fn end(failed: bool) -> Value {
    serde_json::json!({"type":"tool_execution_end","toolCallId":"reused","toolName":"bash","result":{"content":[{"type":"text","text":"result"}]},"isError":failed})
}

#[test]
fn runtime_starts_without_model_messages_never_reuse_a_finished_or_unfinished_invocation() {
    for gap in [false, true] {
        let mut rows = vec![state(), serde_json::json!({"type":"agent_start"}), start()];
        if !gap {
            rows.push(end(false));
        }
        rows.push(start());
        rows.push(end(true));
        rows.push(serde_json::json!({"type":"agent_settled"}));
        let output = run(&rows, true);
        let reduced = reduce(&output);
        let operations: Vec<_> = objects(&output)
            .filter_map(|object| match object {
                TraceObject::Operation(op) => Some(op),
                _ => None,
            })
            .collect();
        assert_eq!(operations.len(), 2);
        assert_ne!(operations[0].operation_ref, operations[1].operation_ref);
        let first = reduced
            .execution(&AnyObjectRef::Operation(
                operations[0].operation_ref.clone(),
            ))
            .unwrap();
        assert_eq!(
            first.outcome.known(),
            if gap {
                None
            } else {
                Some(&TerminalOutcome::Succeeded)
            }
        );
        if gap {
            assert!(first.terminated.is_empty());
        }
        assert_eq!(
            reduced
                .execution(&AnyObjectRef::Operation(
                    operations[1].operation_ref.clone()
                ))
                .unwrap()
                .outcome
                .known(),
            Some(&TerminalOutcome::Failed)
        );
    }
    let output = run(
        &[
            state(),
            serde_json::json!({"type":"agent_start"}),
            start(),
            serde_json::json!({"type":"agent_settled"}),
            serde_json::json!({"type":"agent_start"}),
            end(false),
        ],
        true,
    );
    let reduced = reduce(&output);
    let begun = reduced
        .executions
        .values()
        .find(|execution| !execution.started.is_empty() && execution.outcome.known().is_none())
        .unwrap();
    assert!(begun.terminated.is_empty());
    assert!(output[3].state.values["pi"]["runtime_tools"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[test]
fn a_native_entry_snapshot_between_runtime_end_and_message_admission_preserves_live_binding() {
    let mut rows = frames(LIVE);
    let index = rows
        .iter()
        .position(|row| row["type"] == "tool_execution_end")
        .unwrap();
    let entries: Vec<_> = frames(include_str!(
        "../../../../../../fixtures/unified-trace/pi/tool-sidecar/native.jsonl"
    ))
    .into_iter()
    .skip(1)
    .take_while(|row| row["message"]["role"] != "toolResult")
    .collect();
    let leaf = entries.last().unwrap()["id"].clone();
    rows.insert(index+1,serde_json::json!({"type":"response","command":"get_entries","success":true,"data":{"entries":entries,"leafId":leaf}}));
    let output = run(&rows, true);
    let reduced = reduce(&output);
    let first = output[index]
        .claims
        .iter()
        .find_map(|claim| match &claim.fact {
            Fact::Execution {
                execution: ExecutionRef::Operation(op),
                boundary: ExecutionBoundary::Outcome { .. },
            } => Some(op.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(first.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert!(objects(&output[index+2..]).any(|object|matches!(object,TraceObject::Result(result)if result.operation==first&&result.representation==ResultRepresentation::CallerResult)));
    let message_start = index + 2;
    let key = crate::mapping::attachment_key(
        &capture(message_start as u64, true).observation_ref,
        "/message",
        "/workspace/pi-capture/sidecar-1.txt",
    );
    assert_eq!(
        output[message_start].correlations.get(&key),
        Some(&AnyObjectRef::Operation(first))
    );
}

#[test]
fn runtime_progress_keeps_bounded_checkpoint_proof_and_every_result_occurrence() {
    let mut rows = vec![state(), serde_json::json!({"type":"agent_start"}), start()];
    for _ in 0..200 {
        rows.push(serde_json::json!({"type":"tool_execution_update","toolCallId":"reused","toolName":"bash","partialResult":{"content":[{"type":"text","text":"same progress"}]}}));
    }
    rows.push(end(false));
    let output = run(&rows, true);
    assert_eq!(
        output[2].state.values["pi"]["runtime_tools"],
        output[202].state.values["pi"]["runtime_tools"]
    );
    assert_eq!(objects(&output).filter(|object|matches!(object,TraceObject::Result(result)if result.disposition==ResultDisposition::Progress)).count(),200);
    assert!(output.last().unwrap().state.values["pi"]["runtime_tools"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[test]
fn every_recorded_output_reference_has_its_exact_native_or_rpc_source_slot() {
    for (raw, live) in [
        (LIVE, true),
        (
            include_str!("../../../../../../fixtures/unified-trace/pi/tool-sidecar/native.jsonl"),
            false,
        ),
    ] {
        let rows = frames(raw);
        let output = run(&rows, live);
        let mut observed = 0;
        for (index, row) in rows.iter().enumerate() {
            let mut found = vec![];
            paths(row, "", &mut found);
            for (slot, path) in found {
                observed += 1;
                let key = crate::mapping::attachment_key(
                    &capture(index as u64, live).observation_ref,
                    &slot,
                    &path,
                );
                assert!(
                    matches!(
                        output[index].correlations.get(&key),
                        Some(AnyObjectRef::Operation(_))
                    ),
                    "missing binding at record {index}: {slot}"
                );
            }
        }
        assert!(observed >= 2);
    }
}

#[test]
fn outbound_rpc_values_cannot_become_vendor_identity_or_execution_evidence() {
    let mut capture = capture(0, true);
    capture.transport_kind = TransportKind::LiveStdin;
    let output = PiMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(&state()).unwrap(),
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert!(output.claims.is_empty());
    assert!(output.source_owners.is_empty());
    assert!(output.correlations.is_empty());
}

#[test]
fn echoed_sidecars_require_a_contiguous_prefix_and_exact_message_count() {
    let mut rows = frames(LIVE);
    let index = rows
        .iter()
        .position(|row| row["type"] == "agent_end")
        .unwrap();
    rows[index]["messages"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"role":"user","content":"not observed in the prefix"}));
    let output = run(&rows, true);
    assert!(output[index]
        .correlations
        .keys()
        .all(|key| !key.contains("sidecar-")));
    assert!(output[index].claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::CaptureGap)));

    let rows = frames(LIVE);
    let prefix = run(&rows[..index], true);
    let mut archive = ArchiveContext::default();
    for (i, out) in prefix.iter().enumerate() {
        archive
            .observations
            .insert(capture(i as u64, true).observation_ref);
        archive.correlations.extend(out.correlations.clone());
        archive.extend_correlation_evidence(out.correlation_evidence.clone());
    }
    let mut captured = capture(index as u64, true);
    captured.source_position.locator = SourceLocator::Frame {
        ordinal: index as u64 + 1,
    };
    let output = PiMapper.map(MapInput {
        capture: &captured,
        canonical_observation: &captured.observation_ref,
        raw: &serde_json::to_vec(&rows[index]).unwrap(),
        state: &prefix.last().unwrap().state,
        archive: &archive,
    });
    assert!(output
        .correlations
        .keys()
        .all(|key| !key.contains("sidecar-")));
    assert_eq!(output.state.values["pi"]["inner"]["coherent"], false);
}

fn paths(value: &Value, path: &str, found: &mut Vec<(String, String)>) {
    if let Some(object) = value.as_object() {
        if value["role"] == "toolResult" {
            if let Some(file) = value["details"]["fullOutputPath"].as_str() {
                found.push((path.into(), file.into()));
            }
        }
        for field in ["result", "partialResult"] {
            if let Some(file) = value[field]["details"]["fullOutputPath"].as_str() {
                found.push((format!("{path}/{field}"), file.into()));
            }
        }
        for (key, child) in object {
            paths(child, &format!("{path}/{key}"), found);
        }
    } else if let Some(array) = value.as_array() {
        for (index, child) in array.iter().enumerate() {
            paths(child, &format!("{path}/{index}"), found);
        }
    }
}

#[test]
fn nested_tree_sidecars_use_the_recorded_entry_pointer_instead_of_a_semantic_label() {
    let entries: Vec<_> = frames(include_str!(
        "../../../../../../fixtures/unified-trace/pi/tool-sidecar/native.jsonl"
    ))
    .into_iter()
    .skip(1)
    .collect();
    let leaf = entries.last().unwrap()["id"].clone();
    let mut children = vec![];
    for entry in entries.into_iter().rev() {
        children = vec![serde_json::json!({"entry":entry,"children":children})];
    }
    let response = serde_json::json!({"type":"response","command":"get_tree","success":true,"data":{"tree":children,"leafId":leaf}});
    let output = run(&[state(), response.clone()], true);
    let mut found = vec![];
    paths(&response, "", &mut found);
    assert_eq!(found.len(), 2);
    for (slot, path) in found {
        assert!(slot.contains("/children/0/"));
        let key = crate::mapping::attachment_key(&capture(1, true).observation_ref, &slot, &path);
        assert!(
            matches!(
                output[1].correlations.get(&key),
                Some(AnyObjectRef::Operation(_))
            ),
            "{slot}"
        );
    }
}
