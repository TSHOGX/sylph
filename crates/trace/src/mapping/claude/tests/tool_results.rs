use super::*;

const LIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/tool-read/stdout.jsonl");
const NATIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/tool-read/native.jsonl");
fn frames(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn runtime_results(output: &[MapOutput]) -> Vec<&ResultOccurrence> {
    objects(output)
        .filter_map(|object| match object {
            TraceObject::Result(result)
                if result.representation == ResultRepresentation::Runtime =>
            {
                Some(result)
            }
            _ => None,
        })
        .collect()
}
fn body<'a>(output: &'a [MapOutput], result: &ResultOccurrence) -> &'a [u8] {
    &output
        .iter()
        .flat_map(|out| &out.payloads)
        .find(|payload| {
            Some(&payload.reference) == result.payload.as_ref().map(|payload| &payload.reference)
        })
        .unwrap()
        .bytes
}

#[test]
fn recorded_sdk_and_native_tool_results_keep_plain_runtime_bytes_separate_from_numbered_caller_text(
) {
    for (raw, live) in [(LIVE, true), (NATIVE, false)] {
        let output = run(&frames(raw), live);
        let runtime = runtime_results(&output);
        assert_eq!(runtime.len(), 1);
        let structured: Value = serde_json::from_slice(body(&output, runtime[0])).unwrap();
        assert_eq!(structured["type"], "text");
        assert_eq!(structured["file"]["content"], "SYLPH_CLAUDE_TOOL_BYTES\n");
        assert_eq!(structured["file"]["numLines"], 2);
        let caller = objects(&output)
            .find_map(|object| match object {
                TraceObject::Result(result)
                    if result.representation == ResultRepresentation::CallerResult =>
                {
                    Some(result)
                }
                _ => None,
            })
            .unwrap();
        let returned: Value = serde_json::from_slice(body(&output, caller)).unwrap();
        assert_eq!(returned["tool_use_id"], "toolu_controlled");
        assert_eq!(returned["content"], "1\tSYLPH_CLAUDE_TOOL_BYTES\n2\t");
        assert_eq!(runtime[0].operation, caller.operation);
        assert_ne!(runtime[0].payload, caller.payload);
    }
}

fn envelope(live: bool, metadata: Option<Value>) -> Value {
    let mut value = serde_json::json!({"type":"user","uuid":"result-a","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-a","content":"caller bytes"}]}});
    value[if live { "session_id" } else { "sessionId" }] = "root".into();
    if let Some(metadata) = metadata {
        value[if live {
            "tool_use_result"
        } else {
            "toolUseResult"
        }] = metadata;
    }
    // This other surface's spelling must never become a fallback source.
    value[if live {
        "toolUseResult"
    } else {
        "tool_use_result"
    }] = serde_json::json!({"agentId":"wrong-child","persistedOutputPath":"/wrong/source"});
    value
}

#[test]
fn explicit_runtime_null_and_empty_values_remain_observed_while_missing_metadata_stays_missing() {
    for live in [false, true] {
        for value in [
            Value::Null,
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(""),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let output = run(&[envelope(live, Some(value.clone()))], live);
            let runtime = runtime_results(&output);
            assert_eq!(runtime.len(), 1);
            assert_eq!(
                serde_json::from_slice::<Value>(body(&output, runtime[0])).unwrap(),
                value
            );
            assert!(!objects(&output).any(|object| matches!(
                object,
                TraceObject::Agent(AgentThread {
                    agent_ref: AgentRef::Embedded { .. },
                    ..
                })
            )));
            assert!(output[0]
                .correlations
                .keys()
                .all(|key| !key.contains("/wrong/source")));
        }
        let output = run(&[envelope(live, None)], live);
        assert!(runtime_results(&output).is_empty());
        assert!(!objects(&output).any(|object| matches!(
            object,
            TraceObject::Agent(AgentThread {
                agent_ref: AgentRef::Embedded { .. },
                ..
            })
        )));
    }
}

#[test]
fn ambiguous_envelope_metadata_cannot_assign_runtime_results_or_children_to_each_call() {
    for live in [false, true] {
        let mut value = envelope(
            live,
            Some(
                serde_json::json!({"agentId":"unassigned-child","persistedOutputPath":"/unpaired/output"}),
            ),
        );
        value["message"]["content"].as_array_mut().unwrap().push(serde_json::json!({"type":"tool_result","tool_use_id":"call-b","content":"other caller"}));
        let output = run(&[value], live);
        assert_eq!(
            objects(&output)
                .filter(|object| matches!(
                    object,
                    TraceObject::Result(ResultOccurrence {
                        representation: ResultRepresentation::CallerResult,
                        ..
                    })
                ))
                .count(),
            2
        );
        assert!(runtime_results(&output).is_empty());
        assert!(!objects(&output).any(|object| matches!(
            object,
            TraceObject::Agent(AgentThread {
                agent_ref: AgentRef::Embedded { .. },
                ..
            })
        )));
        assert!(output[0].claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingOwner)));
        assert!(output[0]
            .correlations
            .keys()
            .all(|key| !key.contains("/unpaired/output")));
        let mut invalid = envelope(
            live,
            Some(
                serde_json::json!({"agentId":"unassigned-child","persistedOutputPath":"/unpaired/output"}),
            ),
        );
        invalid["message"]["content"][0]["tool_use_id"] = "".into();
        let invalid = run(&[invalid], live);
        assert!(!objects(&invalid).any(|object| matches!(object, TraceObject::Result(_))));
        assert!(invalid[0]
            .correlations
            .keys()
            .all(|key| !key.contains("/unpaired/output")));
    }
}

#[test]
fn recorded_live_sidecar_metadata_binds_the_actual_invocation_without_borrowing_native_fields() {
    let rows = frames(include_str!(
        "../../../../../../fixtures/unified-trace/claude/tool-sidecar/stdout.jsonl"
    ));
    let index = rows
        .iter()
        .position(|row| row.get("tool_use_result").is_some())
        .unwrap();
    let output = run(&rows, true);
    let captured = capture(index as u64, true);
    let key = crate::mapping::attachment_key(
        &captured.observation_ref,
        "/message/content/0",
        "/workspace/claude-capture/sidecar.txt",
    );
    let Some(AnyObjectRef::Operation(operation)) = output[index].correlations.get(&key) else {
        panic!("the observed SDK sidecar has an invocation binding");
    };
    assert!(
        matches!(operation.address(),ObjectAddress::Native{namespace,key,..}if namespace=="claude.tool-call"&&key==&["toolu_controlled"])
    );
    let runtime = runtime_results(&output);
    assert_eq!(runtime.len(), 1);
    let value: Value = serde_json::from_slice(body(&output, runtime[0])).unwrap();
    assert_eq!(value["persistedOutputSize"], 172032);
    assert_eq!(
        value["persistedOutputPath"],
        "/workspace/claude-capture/sidecar.txt"
    );
}

#[test]
fn outbound_sdk_metadata_cannot_fabricate_vendor_results_admission_or_child_identity() {
    let value = envelope(
        true,
        Some(serde_json::json!({"agentId":"not-observed","status":"async_launched"})),
    );
    let mut capture = capture(0, true);
    capture.transport_kind = TransportKind::LiveStdin;
    let output = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(&value).unwrap(),
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert!(output.claims.is_empty());
    assert!(output.correlations.is_empty());
    assert!(output.requests.is_empty());
    assert!(output.diagnostics.is_empty());
}

#[test]
fn an_unmatched_tool_result_stays_a_caller_result_without_schedule_or_outcome() {
    let output = run(&[envelope(false, None)], false);
    let results: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Result(result) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].representation,
        ResultRepresentation::CallerResult
    );
    assert!(output[0].claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::MissingOwner,
                ..
            }
        }
    )));
    assert!(!output
        .iter()
        .flat_map(|out| &out.claims)
        .any(|claim| matches!(
            claim.fact,
            Fact::Declare {
                object: TraceObject::Resource(_)
            } | Fact::Execution { .. }
        )));
}
