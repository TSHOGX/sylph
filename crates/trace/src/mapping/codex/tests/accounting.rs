use super::*;
use std::collections::BTreeSet;
fn measurements(outputs: &[MapOutput]) -> Vec<&Measurement> {
    outputs
        .iter()
        .flat_map(|output| &output.claims)
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Measurement(value),
            } => Some(value),
            _ => None,
        })
        .collect()
}
#[test]
fn recorded_response_turn_thread_and_latest_usage_keep_distinct_scopes_and_series() {
    let rows: Vec<Value> =
        include_str!("../../../../../../fixtures/unified-trace/codex/accounting/native.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let outputs = run(&rows, false);
    let values = measurements(&outputs);
    let totals: Vec<_> = values
        .iter()
        .filter(|value| value.metric == Metric::TotalTokens)
        .collect();
    assert_eq!(totals.len(), 15);
    let mut groups = BTreeMap::<String, Vec<i64>>::new();
    for value in totals {
        let key = match value.measurement_ref.address() {
            ObjectAddress::Native { namespace, .. } => namespace.clone(),
            ObjectAddress::Occurrence { local_slot, .. } => local_slot[1].clone(),
        };
        groups
            .entry(key)
            .or_default()
            .push(value.value.as_i64().unwrap());
    }
    for values in groups.values_mut() {
        values.sort();
    }
    assert_eq!(groups["codex.response-usage"], [58679, 58710, 58804]);
    assert_eq!(groups["latest"], [58679, 58710, 58804]);
    assert_eq!(groups["response-turn-total"], [58679, 117389, 176193]);
    assert_eq!(groups["response-thread-total"], [58679, 117389, 176193]);
    assert_eq!(groups["thread-total"], [787021551, 787080261, 787139065]);
    let domains: BTreeSet<_> = values
        .iter()
        .filter(|value| value.metric == Metric::TotalTokens)
        .filter_map(|value| match &value.mode {
            MeasurementMode::Cumulative {
                counter_domain: Some(domain),
                ..
            } => Some(domain),
            _ => None,
        })
        .collect();
    assert_eq!(
        domains.len(),
        3,
        "turn and both thread counters have distinct domains"
    );
    assert_eq!(
        values
            .iter()
            .filter(
                |value| value.metric == Metric::TotalTokens && value.mode == MeasurementMode::Gauge
            )
            .count(),
        6
    );

    assert_eq!(values.iter().filter(|value|matches!(&value.metric,Metric::Extension{name,..}if name=="cache_write_input_tokens")).count(),15);
    assert!(values.iter().filter(|value|matches!(value.subject,AnyObjectRef::Activation(_))).all(|value|matches!(&value.mode,MeasurementMode::Cumulative{reset_epoch:Some(turn),..}if turn=="01a06a66-236a-73d2-9e61-beb67ca79f1a")));
    assert!(
        !outputs
            .iter()
            .flat_map(|output| &output.claims)
            .any(|claim| matches!(
                claim.fact,
                Fact::Execution { .. }
                    | Fact::Declare {
                        object: TraceObject::Activation(_) | TraceObject::Operation(_)
                    }
            ))
    );
}
#[test]
fn response_accounting_requires_its_own_thread_and_turn_identity() {
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"token_usage_record","payload":{"response_id":"response","session_id":"root","root_turn_id":"parent-turn","usage":{"total_tokens":7},"turn_token_usage":{"total_tokens":8}}}),
        ],
        false,
    );
    assert!(measurements(&output).is_empty());
    let output = run(
        &[
            header(),
            serde_json::json!({"type":"token_usage_record","payload":{"thread_id":"child","response_id":"response","session_id":"root","root_turn_id":"parent-turn","usage":{"total_tokens":7},"turn_token_usage":{"total_tokens":8},"thread_token_usage":{"total_tokens":9}}}),
        ],
        false,
    );
    let values = measurements(&output);
    assert_eq!(values.len(), 2);
    assert!(
        values
            .iter()
            .all(|value| value.subject == AnyObjectRef::Agent(agent("child")))
    );
    assert!(output.iter().flat_map(|output|&output.claims).any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingOwner)));
}
