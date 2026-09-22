use super::*;

fn recorded() -> Vec<Value> {
    include_str!("../../../../../../fixtures/unified-trace/claude/delegation/stdout.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn recorded_task_reuse_keeps_both_activity_creators_and_their_own_states() {
    let outputs = run(&recorded(), true);
    let reduced = reduce(&outputs);
    let tasks: Vec<_> = reduced.objects.values().filter_map(|object| match object.declaration.known() {
        Some(TraceObject::Resource(resource)) if matches!(&resource.kind, ResourceKind::Extension { namespace, name } if namespace == "claude.task" && name == "local_agent") => Some(resource),
        _ => None,
    }).collect();
    assert_eq!(
        tasks.len(),
        2,
        "a reused task handle must not collide its activity declarations"
    );
    let root = owner("e9b9839b-607e-4acd-b5a8-f4b1d8c3b38f");
    assert_eq!(
        reduced
            .relations
            .iter()
            .filter(|value| matches!(value.relation, Relation::Spawn { .. }))
            .count(),
        1
    );
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(operation(
                &root,
                "toolu_delegate_1"
            )))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert_eq!(
        tasks
            .iter()
            .map(|resource| resource.creator.clone().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        [
            operation(&root, "toolu_delegate_1"),
            operation(&root, "toolu_delegate_2")
        ]
        .into_iter()
        .collect()
    );
    for task in tasks {
        let states = &reduced.resource_states[&task.resource_ref];
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| *state == ResourceState::Running)
                .count(),
            1
        );
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| matches!(
                    state,
                    ResourceState::Closed {
                        outcome: TerminalOutcome::Succeeded,
                        ..
                    }
                ))
                .count(),
            2
        );
    }
}
#[test]
fn explicit_sdk_origin_and_forwarded_input_keep_their_documented_authorship() {
    let values = [
        serde_json::json!({"type":"user","session_id":"root","uuid":"human","origin":{"kind":"human"},"message":{"role":"user","content":"human source"}}),
        serde_json::json!({"type":"user","session_id":"root","uuid":"channel","origin":{"kind":"channel","server":"observed-channel"},"message":{"role":"user","content":"channel source"}}),
    ];
    let outputs = run(&values, true);
    let authors: Vec<_> = objects(&outputs)
        .filter_map(|object| match object {
            TraceObject::Message(value) => Some(value.author.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        authors,
        [
            Author::Human { subject: None },
            Author::External {
                identity: Some("observed-channel".into())
            }
        ]
    );
    let mut capture = capture(0, true);
    capture.declared_subject_refs.clear();
    let child = Context::child(&owner("root"), "observed-child");
    let mut archive = ArchiveContext::default();
    archive.correlations.insert(
        correlation_key(&owner("root"), "claude.parent-tool-use", &["host".into()]),
        AnyObjectRef::Agent(child),
    );
    let output = ClaudeMapper.map(MapInput { capture: &capture, canonical_observation: &capture.observation_ref, raw: br#"{"type":"user","session_id":"root","uuid":"forwarded","parent_tool_use_id":"host","message":{"role":"user","content":"delegated prompt"}}"#, state: &MapperState::default(), archive: &archive });
    let outputs = [output];
    let forwarded: Vec<_> = objects(&outputs)
        .filter_map(|object| match object {
            TraceObject::Message(value) => Some(value.author.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, [Author::Unknown]);
}
#[test]
fn an_unrelated_tool_result_cannot_mint_a_child_from_an_agent_id_field() {
    let values = [
        serde_json::json!({"type":"assistant","sessionId":"root","uuid":"call","message":{"role":"assistant","content":[{"type":"tool_use","id":"inspect","name":"Read","input":{"file_path":"fixture"}}]}}),
        serde_json::json!({"type":"user","sessionId":"root","uuid":"reply","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"inspect","content":"application data"}]},"toolUseResult":{"agentId":"application-row"}}),
    ];
    let outputs = run(&values, false);
    assert!(!objects(&outputs).any(|object| matches!(
        object,
        TraceObject::Agent(AgentThread {
            agent_ref: AgentRef::Embedded { .. },
            ..
        })
    )));
}

fn task(kind: &str, id: &str, call: Option<&str>, status: Option<&str>, uuid: &str) -> Value {
    let mut value = serde_json::json!({"type":"system","subtype":kind,"session_id":"root","task_id":id,"uuid":uuid});
    if kind == "task_started" {
        value["task_type"] = "local_agent".into();
    }
    if let Some(call) = call {
        value["tool_use_id"] = call.into();
    }
    if let Some(status) = status {
        value["status"] = status.into();
    }
    value
}
#[test]
fn an_old_task_notification_cannot_finish_a_new_episode() {
    let outputs = run(
        &[
            task("task_started", "reused", Some("first"), None, "start-first"),
            task(
                "task_updated",
                "reused",
                None,
                Some("completed"),
                "first-completed",
            ),
            task(
                "task_started",
                "reused",
                Some("second"),
                None,
                "start-second",
            ),
            task(
                "task_notification",
                "reused",
                Some("first"),
                Some("failed"),
                "old-notification",
            ),
        ],
        true,
    );
    let reduced = reduce(&outputs);
    let current = objects(&outputs)
        .find_map(|object| match object {
            TraceObject::Resource(resource)
                if resource.creator == Some(operation(&owner("root"), "second")) =>
            {
                Some(resource)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        reduced.resource_states[&current.resource_ref]
            .iter()
            .map(|(state, _)| state)
            .collect::<Vec<_>>(),
        [&ResourceState::Running]
    );
    assert_eq!(
        reduced
            .resource_states
            .values()
            .flatten()
            .filter(|(state, _)| matches!(
                state,
                ResourceState::Closed {
                    outcome: TerminalOutcome::Failed,
                    ..
                }
            ))
            .count(),
        1
    );
}
#[test]
fn overlapping_task_starts_leave_unqualified_updates_unassigned() {
    let outputs = run(
        &[
            task("task_started", "reused", Some("first"), None, "start-first"),
            task(
                "task_started",
                "reused",
                Some("second"),
                None,
                "start-second",
            ),
            task(
                "task_updated",
                "reused",
                None,
                Some("failed"),
                "ambiguous-update",
            ),
        ],
        true,
    );
    let reduced = reduce(&outputs);
    assert_eq!(reduced.resource_states.len(), 2);
    assert!(
        reduced
            .resource_states
            .values()
            .flatten()
            .all(|(state, _)| *state == ResourceState::Running)
    );
    assert!(outputs[2].claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingSource)));
}

#[test]
fn a_failed_send_message_result_is_not_success_or_a_resumed_child() {
    let output = run(
        &[
            serde_json::json!({"type":"assistant","sessionId":"root","uuid":"send","message":{"role":"assistant","content":[{"type":"tool_use","id":"send","name":"SendMessage","input":{"to":"unavailable","message":"followup"}}]}}),
            serde_json::json!({"type":"user","sessionId":"root","uuid":"returned","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"send","content":"no transcript","is_error":false}]},"toolUseResult":{"success":false,"message":"No transcript found","resumedAgentId":"unavailable"}}),
        ],
        false,
    );
    let reduced = reduce(&output);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(operation(&owner("root"), "send")))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    assert!(!objects(&output).any(|object| matches!(
        object,
        TraceObject::Agent(AgentThread {
            agent_ref: AgentRef::Embedded { .. },
            ..
        })
    )));
}
#[test]
fn settled_task_episodes_do_not_accumulate_in_the_runtime_checkpoint() {
    let checkpoint = |count| {
        let mut rows = vec![];
        for index in 0..count {
            let id = format!("task-{index}");
            let call = format!("call-{index}");
            rows.push(task(
                "task_started",
                &id,
                Some(&call),
                None,
                &format!("start-{index}"),
            ));
            rows.push(task(
                "task_notification",
                &id,
                Some(&call),
                Some("completed"),
                &format!("end-{index}"),
            ));
        }
        let output = run(&rows, true);
        serde_json::to_vec(&output.last().unwrap().state)
            .unwrap()
            .len()
    };
    assert!(
        checkpoint(200) <= checkpoint(20) + 32,
        "completed activity history belongs in the archive, not the source checkpoint"
    );
}

#[test]
fn a_result_waiting_for_its_invocation_never_publishes_an_outcome_it_will_reverse() {
    let raw = br#"{"type":"user","sessionId":"root","uuid":"reply","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"send","content":"not delivered","is_error":false}]},"toolUseResult":{"success":false}}"#;
    let capture = capture(1, false);
    let state = MapperState::default();
    let mut archive = ArchiveContext::default();
    let map = |archive: &ArchiveContext| {
        ClaudeMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw,
            state: &state,
            archive,
        })
    };
    let waiting = map(&archive);
    assert!(!waiting.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            boundary: ExecutionBoundary::Outcome { .. },
            ..
        }
    )));
    let call = serde_json::json!({"type":"assistant","sessionId":"root","uuid":"send","message":{"role":"assistant","content":[{"type":"tool_use","id":"send","name":"SendMessage","input":{"to":"child","message":"fixture"}}]}});
    let call = run(&[call], false).remove(0);
    for claim in &call.claims {
        if let Fact::Declare {
            object: TraceObject::Operation(value),
        } = &claim.fact
        {
            archive
                .semantic_mut()
                .operations
                .insert(value.operation_ref.clone(), value.clone());
            archive.semantic_mut().object_evidence.insert(
                AnyObjectRef::Operation(value.operation_ref.clone()),
                claim.evidence.clone().into(),
            );
        }
    }
    let known = map(&archive);
    let reduced = reduce(&[waiting, known]);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(operation(&owner("root"), "send")))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
}
