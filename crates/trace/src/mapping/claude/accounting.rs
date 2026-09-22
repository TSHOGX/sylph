//! Query-local usage and process-cumulative accounting have different subjects.
use super::*;

impl Context<'_, '_> {
    fn accounting_mode(&self, owner: &AgentRef, keys: Vec<String>) -> MeasurementMode {
        let mut keys = keys;
        keys.extend([
            self.input.capture.source_position.source.encode(),
            self.input.capture.source_position.generation.to_string(),
        ]);
        MeasurementMode::Cumulative {
            counter_domain: Some(correlation_key(owner, "claude.process-accounting", &keys)),
            reset_epoch: None,
            sample_position: self.input.capture.source_position.clone(),
        }
    }
    pub(super) fn process_accounting(&mut self, owner: &AgentRef, value: &Value) {
        if let Some(amount) = value.get("total_cost_usd").and_then(Value::as_number) {
            self.object(
                owner,
                "claude/sdk-reported-cost",
                TraceObject::Measurement(Measurement {
                    measurement_ref: MeasurementRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec!["sdk-cost".into()],
                    )
                    .unwrap(),
                    subject: AnyObjectRef::Agent(owner.clone()),
                    model: None,
                    metric: Metric::Cost {
                        pricing_basis: "claude/sdk-reported-total_cost_usd".into(),
                    },
                    unit: MeasurementUnit::Currency { code: "USD".into() },
                    value: amount.clone(),
                    mode: self.accounting_mode(owner, vec!["total_cost_usd".into()]),
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![],
                }),
            );
        }
        let Some(models) = value.get("modelUsage").and_then(Value::as_object) else {
            return;
        };
        for (name, usage) in models {
            let model = MeasurementModel {
                reported_name: name.clone(),
                canonical_name: text(usage, "canonicalModel").map(str::to_owned),
                provider: text(usage, "provider").map(str::to_owned),
            };
            for (field, metric, unit, cumulative) in [
                (
                    "inputTokens",
                    Metric::InputTokens,
                    MeasurementUnit::Tokens,
                    true,
                ),
                (
                    "outputTokens",
                    Metric::OutputTokens,
                    MeasurementUnit::Tokens,
                    true,
                ),
                (
                    "cacheReadInputTokens",
                    Metric::CachedTokens,
                    MeasurementUnit::Tokens,
                    true,
                ),
                (
                    "cacheCreationInputTokens",
                    Metric::Extension {
                        namespace: "claude.usage".into(),
                        name: "cache_creation_input_tokens".into(),
                    },
                    MeasurementUnit::Tokens,
                    true,
                ),
                (
                    "thinkingTokens",
                    Metric::ReasoningTokens,
                    MeasurementUnit::Tokens,
                    true,
                ),
                (
                    "webSearchRequests",
                    Metric::Extension {
                        namespace: "claude.usage".into(),
                        name: "web_search_requests".into(),
                    },
                    MeasurementUnit::Count,
                    true,
                ),
                (
                    "costUSD",
                    Metric::Cost {
                        pricing_basis: format!(
                            "claude/modelUsage/{}",
                            text(usage, "costBasis").unwrap_or("vendor-reported")
                        ),
                    },
                    MeasurementUnit::Currency { code: "USD".into() },
                    true,
                ),
                (
                    "contextWindow",
                    Metric::Extension {
                        namespace: "claude.model".into(),
                        name: "context_window".into(),
                    },
                    MeasurementUnit::Tokens,
                    false,
                ),
                (
                    "maxOutputTokens",
                    Metric::Extension {
                        namespace: "claude.model".into(),
                        name: "max_output_tokens".into(),
                    },
                    MeasurementUnit::Tokens,
                    false,
                ),
            ] {
                let Some(number) = usage.get(field).and_then(Value::as_number) else {
                    continue;
                };
                self.object(
                    owner,
                    &format!("claude/model-usage/{name}/{field}"),
                    TraceObject::Measurement(Measurement {
                        measurement_ref: MeasurementRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec!["model-usage".into(), name.clone(), field.into()],
                        )
                        .unwrap(),
                        subject: AnyObjectRef::Agent(owner.clone()),
                        model: Some(model.clone()),
                        metric,
                        unit,
                        value: number.clone(),
                        mode: if cumulative {
                            self.accounting_mode(owner, vec![name.clone(), field.into()])
                        } else {
                            MeasurementMode::Gauge
                        },
                        aggregation_scope: AggregationScope::NamedSubject,
                        components: vec![],
                    }),
                );
            }
        }
    }
}
