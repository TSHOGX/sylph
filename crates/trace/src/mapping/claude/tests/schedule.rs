use super::*;

const LIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/schedule/stdout.jsonl");
const NATIVE: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/schedule/native.jsonl");
fn frames(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn call(id: &str, name: &str, input: Value) -> Value {
    serde_json::json!({"type":"assistant","sessionId":"root","uuid":format!("call-{id}"),"message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
}
fn reply(id: &str, value: Value, error: bool) -> Value {
    serde_json::json!({"type":"user","sessionId":"root","uuid":format!("result-{id}"),"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"caller result","is_error":error}]},"toolUseResult":value})
}
fn created() -> Vec<Value> {
    vec![
        call(
            "create",
            "CronCreate",
            serde_json::json!({"cron":"0 0 1 1 *","prompt":"fixture","recurring":false}),
        ),
        reply(
            "create",
            serde_json::json!({"id":"known","humanSchedule":"0 0 1 1 *","recurring":false,"durable":false}),
            false,
        ),
    ]
}
fn schedules(output: &[MapOutput]) -> Vec<&Resource> {
    objects(output)
        .filter_map(|object| match object {
            TraceObject::Resource(resource) if resource.kind == ResourceKind::Schedule => {
                Some(resource)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn recorded_schedule_registration_listing_and_deletion_remain_independent_of_execution() {
    for (raw, live) in [(LIVE, true), (NATIVE, false)] {
        let output = run(&frames(raw), live);
        let reduced = reduce(&output);
        let resources: Vec<_> = reduced
            .objects
            .values()
            .filter_map(|object| match object.declaration.known() {
                Some(TraceObject::Resource(resource))
                    if resource.kind == ResourceKind::Schedule =>
                {
                    Some(resource)
                }
                _ => None,
            })
            .collect();
        assert_eq!(resources.len(), 1);
        let resource = resources[0];
        assert!(
            matches!(resource.resource_ref.address(),ObjectAddress::Native{namespace,key,..}if namespace=="claude.schedule"&&key==&["9b30bd4d"])
        );
        assert!(resource.creator.is_some());
        assert!(resource.host.is_none());
        let states = &reduced.resource_states[&resource.resource_ref];
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| matches!(state, ResourceState::ScheduleRegistered { .. }))
                .count(),
            2
        );
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| matches!(state, ResourceState::ScheduleCancelled))
                .count(),
            1
        );
        assert!(!states
            .iter()
            .any(|(state, _)| matches!(state, ResourceState::ScheduleFired { .. })));
        assert!(reduced
            .execution(&AnyObjectRef::Resource(resource.resource_ref.clone()))
            .is_none());
        let failed = operation(
            &owner("87a03655-be7b-432b-bca4-5cba20392b03"),
            "toolu_schedule_5",
        );
        assert_eq!(
            reduced
                .execution(&AnyObjectRef::Operation(failed))
                .unwrap()
                .outcome
                .known(),
            Some(&TerminalOutcome::Failed)
        );
    }
}

#[test]
fn failed_or_mismatched_schedule_results_do_not_register_or_cancel_jobs() {
    let output = run(
        &[
            call("create", "CronCreate", serde_json::json!({})),
            reply(
                "create",
                serde_json::json!({"id":"not-created","humanSchedule":"* * * * *","durable":false,"recurring":true}),
                true,
            ),
        ],
        false,
    );
    assert!(schedules(&output).is_empty());
    let mut rows = created();
    rows.extend([
        call("delete", "CronDelete", serde_json::json!({"id":"known"})),
        reply("delete", serde_json::json!({"id":"different"}), false),
    ]);
    let output = run(&rows, false);
    assert!(!output.iter().flat_map(|o| &o.claims).any(|claim| matches!(
        claim.fact,
        Fact::ResourceState {
            state: ResourceState::ScheduleCancelled,
            ..
        }
    )));
    assert!(output.iter().flat_map(|o|&o.claims).any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::ConflictingIdentity)));
    let output = run(
        &[
            call("lookup", "OtherTool", serde_json::json!({})),
            reply(
                "lookup",
                serde_json::json!({"id":"fake","humanSchedule":"* * * * *","durable":false,"recurring":true}),
                false,
            ),
        ],
        false,
    );
    assert!(
        schedules(&output).is_empty(),
        "result field names do not identify a built-in invocation"
    );
}

#[test]
fn listings_and_firing_prose_do_not_invent_cancellation_firing_or_cross_session_scope() {
    let mut rows = created();
    rows.extend([
        call("list", "CronList", serde_json::json!({})),
        reply("list", serde_json::json!({"jobs":[]}), false),
    ]);
    let mut notice: Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/native/claude/system-subtype-scheduled-task-fire.jsonl"
    ))
    .unwrap();
    notice["sessionId"] = "root".into();
    rows.push(notice);
    let output = run(&rows, false);
    let reduced = reduce(&output);
    assert_eq!(
        reduced
            .resource_states
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        1
    );
    assert!(reduced.executions.keys().all(|reference| matches!(reference, AnyObjectRef::Operation(_))));
    assert!(output.last().unwrap().claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingSource)));
    assert!(objects(&output).any(|object|matches!(object,TraceObject::Message(message)if message.purpose==MessagePurpose::Notice)));
    let output = run(
        &[
            call("list", "CronList", serde_json::json!({})),
            reply(
                "list",
                serde_json::json!({"jobs":[{"id":"project-job","durable":true,"cron":"* * * * *","prompt":"shared job"}]}),
                false,
            ),
        ],
        false,
    );
    assert!(schedules(&output).is_empty());
    assert!(output.iter().flat_map(|o|&o.claims).any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingSource)));
}
