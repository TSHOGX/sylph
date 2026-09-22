use super::*;

fn frames(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn recorded_retry_keeps_failed_inferences_and_one_episode_without_ending_the_outer_run() {
    let rows = frames(include_str!(
        "../../../../../../fixtures/unified-trace/pi/stdout-retry.jsonl"
    ));
    let output = run(&rows, true);
    let reduced = reduce(&output);
    let operations: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Operation(operation) => Some(operation),
            _ => None,
        })
        .collect();
    let inferences: Vec<_> = operations
        .iter()
        .filter(|operation| matches!(operation.category, OperationCategory::Inference { .. }))
        .collect();
    assert_eq!(inferences.len(), 3);
    let outcomes: Vec<_> = inferences
        .iter()
        .map(|operation| {
            *reduced
                .execution(&AnyObjectRef::Operation(operation.operation_ref.clone()))
                .unwrap()
                .outcome
                .known()
                .unwrap()
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            TerminalOutcome::Failed,
            TerminalOutcome::Failed,
            TerminalOutcome::Succeeded
        ]
    );
    let retry: Vec<_> = operations.iter().filter(|operation| matches!(&operation.category, OperationCategory::Extension { namespace, name } if namespace=="pi.runtime" && name=="retry")).collect();
    assert_eq!(retry.len(), 1);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(retry[0].operation_ref.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let attempts: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Result(result)
                if result.operation == retry[0].operation_ref
                    && result.disposition == ResultDisposition::Progress =>
            {
                Some(result)
            }
            _ => None,
        })
        .collect();
    assert_eq!(attempts.len(), 2);
    let mut attempts_values = vec![];
    for attempt in attempts {
        let body = output
            .iter()
            .flat_map(|out| &out.payloads)
            .find(|payload| Some(&payload.reference) == attempt.payload.as_ref())
            .unwrap();
        attempts_values.push(
            serde_json::from_slice::<Value>(&body.bytes).unwrap()["delayMs"]
                .as_u64()
                .unwrap(),
        );
    }
    assert_eq!(attempts_values, [10, 20]);
    for (index, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row["type"] == "agent_end" || row["type"] == "auto_retry_end")
    {
        assert!(
            output[index].state.values["pi"]["outer"].is_object(),
            "{} cannot settle the outer run",
            row["type"]
        );
        assert!(!output[index].claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::Execution {
                execution: ExecutionRef::Activation(_),
                boundary: ExecutionBoundary::Terminated { .. } | ExecutionBoundary::Outcome { .. }
            }
        )));
    }
    assert!(output
        .iter()
        .flat_map(|out| &out.payloads)
        .any(|payload| String::from_utf8_lossy(&payload.bytes)
            .contains("Controlled overloaded provider attempt")));
    let activations: Vec<_> = reduced
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .collect();
    assert_eq!(activations.len(), 1);
    assert_eq!(
        activations[0].1.outcome.known(),
        Some(&TerminalOutcome::Succeeded)
    );
}

#[test]
fn recorded_manual_compaction_has_its_own_lifecycle_and_an_unjoined_summary_checkpoint() {
    let rows = frames(include_str!(
        "../../../../../../fixtures/unified-trace/pi/stdout-compaction.jsonl"
    ));
    let output = run(&rows, true);
    let reduced = reduce(&output);
    let compact: Vec<_> = objects(&output).filter_map(|object|match object {
        TraceObject::Operation(operation) if matches!(&operation.category, OperationCategory::Extension { namespace,name } if namespace=="pi.runtime"&&name=="compaction")=>Some(operation),_=>None,
    }).collect();
    assert_eq!(compact.len(), 1);
    assert!(
        compact[0].activation.is_none(),
        "manual compaction after settlement must not reopen an activation"
    );
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(compact[0].operation_ref.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let checkpoints: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::ContextCheckpoint(checkpoint) => Some(checkpoint),
            _ => None,
        })
        .collect();
    assert_eq!(
        checkpoints.len(),
        2,
        "the live result and entry snapshot are independently identified"
    );
    let live = checkpoints
        .iter()
        .find(|checkpoint| {
            matches!(
                checkpoint.checkpoint_ref.address(),
                ObjectAddress::Occurrence { .. }
            )
        })
        .unwrap();
    assert_eq!(live.basis, ContextBasis::Summary);
    assert!(matches!(live.coverage, InclusionCoverage::Unknown { .. }));
    assert!(checkpoints.iter().any(|checkpoint| matches!(checkpoint.checkpoint_ref.address(), ObjectAddress::Native { namespace, key, .. } if namespace=="pi.checkpoint" && key==&["a7cd56da"])));
    assert!(output
        .iter()
        .flat_map(|out| &out.payloads)
        .any(|payload| String::from_utf8_lossy(&payload.bytes)
            .contains("SYLPH_PI_COMPACTION_SUMMARY")));
    assert_eq!(
        objects(&output)
            .filter(|object| matches!(object, TraceObject::Activation(_)))
            .count(),
        1
    );
}

#[test]
fn missing_retry_completion_does_not_reuse_the_previous_episode_or_invent_an_outcome() {
    let rows = [
        state(),
        serde_json::json!({"type":"agent_start"}),
        serde_json::json!({"type":"auto_retry_start","attempt":1,"delayMs":10}),
        serde_json::json!({"type":"agent_settled"}),
        serde_json::json!({"type":"agent_start"}),
        serde_json::json!({"type":"auto_retry_start","attempt":1,"delayMs":10}),
        serde_json::json!({"type":"auto_retry_end","attempt":1,"success":false,"finalError":"retry exhausted"}),
        serde_json::json!({"type":"agent_settled"}),
    ];
    let output = run(&rows, true);
    let operations: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Operation(operation) => Some(operation),
            _ => None,
        })
        .collect();
    assert_eq!(operations.len(), 2);
    assert_ne!(operations[0].operation_ref, operations[1].operation_ref);
    let reduced = reduce(&output);
    assert!(reduced
        .execution(&AnyObjectRef::Operation(
            operations[0].operation_ref.clone()
        ))
        .unwrap()
        .outcome
        .known()
        .is_none());
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
    assert!(output[3].claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic} if diagnostic.code==DiagnosticCode::CaptureGap)));
}

#[test]
fn compaction_failure_and_abort_do_not_become_success_from_will_retry() {
    for (end, expected) in [
        (
            serde_json::json!({"type":"compaction_end","aborted":false,"willRetry":true,"errorMessage":"summary failed"}),
            TerminalOutcome::Failed,
        ),
        (
            serde_json::json!({"type":"compaction_end","aborted":true,"willRetry":false}),
            TerminalOutcome::Interrupted,
        ),
        (
            serde_json::json!({"type":"compaction_end","aborted":false,"willRetry":true}),
            TerminalOutcome::Unknown,
        ),
        (
            serde_json::json!({"type":"compaction_end","aborted":false,"willRetry":false,"result":{}}),
            TerminalOutcome::Unknown,
        ),
    ] {
        let output = run(
            &[
                state(),
                serde_json::json!({"type":"compaction_start","reason":"overflow"}),
                end,
            ],
            true,
        );
        let reduced = reduce(&output);
        assert_eq!(
            reduced.executions.values().next().unwrap().outcome.known(),
            Some(&expected)
        );
        assert!(!objects(&output).any(|object| matches!(
            object,
            TraceObject::Activation(_) | TraceObject::ContextCheckpoint(_)
        )));
    }
}

#[test]
fn native_failed_response_preserves_its_reported_error_without_inventing_execution() {
    let rows = frames(include_str!(
        "../../../../../../fixtures/unified-trace/pi/native-retry.jsonl"
    ));
    let output = run(&rows, false);
    let reduced = reduce(&output);
    assert!(reduced.executions.is_empty());
    let failures: Vec<_> = output
        .iter()
        .flat_map(|out| &out.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Diagnostic { diagnostic }
                if diagnostic.code == DiagnosticCode::ReportedFailure =>
            {
                Some(diagnostic)
            }
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 2);
    assert!(failures.iter().all(|diagnostic| diagnostic
        .message
        .contains("Controlled overloaded provider attempt")
        && matches!(diagnostic.subject, Some(AnyObjectRef::MessageRevision(_)))));
}
