use super::*;
#[test]
fn recorded_queries_keep_local_usage_separate_from_cumulative_model_usage_and_cost() {
    let rows: Vec<Value> =
        include_str!("../../../../../../fixtures/unified-trace/claude/accounting/stdout.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let output = run(&rows, true);
    let values: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Measurement(value) => Some(value),
            _ => None,
        })
        .collect();
    let query: Vec<_> = values
        .iter()
        .filter(|value| {
            value.metric == Metric::InputTokens
                && matches!(value.subject, AnyObjectRef::Activation(_))
        })
        .collect();
    assert_eq!(
        query
            .iter()
            .map(|value| value.value.as_i64().unwrap())
            .collect::<Vec<_>>(),
        [10, 20]
    );
    assert_ne!(query[0].subject, query[1].subject);
    let models: Vec<_> = values
        .iter()
        .filter(|value| value.metric == Metric::InputTokens && value.model.is_some())
        .collect();
    assert_eq!(
        models
            .iter()
            .map(|value| value.value.as_i64().unwrap())
            .collect::<Vec<_>>(),
        [10, 30]
    );
    assert!(
        models
            .iter()
            .all(|value| matches!(value.subject, AnyObjectRef::Agent(_))
                && matches!(
                    value.mode,
                    MeasurementMode::Cumulative {
                        reset_epoch: None,
                        ..
                    }
                ))
    );
    assert!(
        models
            .iter()
            .all(
                |value| value.model.as_ref().unwrap().reported_name == "claude-sonnet-4-6"
                    && value.model.as_ref().unwrap().provider.as_deref() == Some("firstParty")
            )
    );
    let cost: Vec<_> = values
        .iter()
        .filter(|value| matches!(value.metric, Metric::Cost { .. }) && value.model.is_none())
        .collect();
    assert_eq!(cost.len(), 2);
    assert!(
        cost.iter()
            .all(|value| matches!(value.subject, AnyObjectRef::Agent(_))
                && matches!(value.mode, MeasurementMode::Cumulative { .. }))
    );
    assert_eq!(cost[0].value.to_string(), "0.000045");
    assert_eq!(cost[1].value.to_string(), "0.000135");
    let limits: Vec<_> = values.iter()
        .filter(|value| value.model.is_some()
            && matches!(&value.metric, Metric::Extension { namespace, .. } if namespace == "claude.model"))
        .collect();
    assert_eq!(limits.len(), 4);
    assert!(
        limits
            .iter()
            .all(|value| value.mode == MeasurementMode::Gauge)
    );
    assert!(values.iter().all(|value| value.components.is_empty()));
}
