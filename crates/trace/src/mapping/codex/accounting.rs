//! Response records, latest samples and cumulative series never share accounting identity.
use super::*;

enum Sample<'a> {
    Response(&'a str),
    Cumulative {
        series: &'a str,
        reset: Option<&'a str>,
    },
    Latest,
}
impl Context<'_, '_> {
    pub(super) fn token_count(&mut self, owner: &AgentRef, value: &Value, live: bool) {
        if value.is_null() {
            return;
        }
        self.token_sample(
            owner,
            &value[if live { "total" } else { "total_token_usage" }],
            live,
            AnyObjectRef::Agent(owner.clone()),
            "thread-total",
            Sample::Cumulative {
                series: "codex.thread-token-count",
                reset: None,
            },
        );
        self.token_sample(
            owner,
            &value[if live { "last" } else { "last_token_usage" }],
            live,
            AnyObjectRef::Agent(owner.clone()),
            "latest",
            Sample::Latest,
        );
    }
    pub(super) fn response_usage(&mut self, containing: &AgentRef, value: &Value) {
        let Some(owner) = string(value, "thread_id")
            .filter(|id| !id.is_empty())
            .and_then(|id| self.agent(id))
        else {
            self.issue(
                containing,
                "codex/usage-owner-missing",
                DiagnosticCode::MissingOwner,
                "Response accounting requires its actual owning thread_id",
            );
            return;
        };
        let Some(response) = string(value, "response_id").filter(|id| !id.is_empty()) else {
            self.issue(
                &owner,
                "codex/response-id-missing",
                DiagnosticCode::MissingOwner,
                "Response accounting omits its response ID",
            );
            return;
        };
        self.token_sample(
            &owner,
            &value["usage"],
            false,
            AnyObjectRef::Agent(owner.clone()),
            "response",
            Sample::Response(response),
        );
        self.token_sample(
            &owner,
            &value["thread_token_usage"],
            false,
            AnyObjectRef::Agent(owner.clone()),
            "response-thread-total",
            Sample::Cumulative {
                series: "codex.response-thread-total",
                reset: None,
            },
        );
        if !value["turn_token_usage"].is_null() {
            if let Some(turn) = string(value, "turn_id").filter(|id| !id.is_empty()) {
                self.token_sample(
                    &owner,
                    &value["turn_token_usage"],
                    false,
                    AnyObjectRef::Activation(activation_ref(&owner, turn)),
                    "response-turn-total",
                    Sample::Cumulative {
                        series: "codex.response-turn-total",
                        reset: Some(turn),
                    },
                );
            } else {
                self.issue(
                    &owner,
                    "codex/usage-turn-missing",
                    DiagnosticCode::MissingOwner,
                    "Turn accounting lacks its own turn_id; root_turn_id is not a substitute",
                );
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn token_sample(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        live: bool,
        subject: AnyObjectRef,
        slot: &str,
        sample: Sample<'_>,
    ) {
        let mut observed = false;
        for (snake, camel, metric) in [
            ("input_tokens", "inputTokens", Metric::InputTokens),
            ("output_tokens", "outputTokens", Metric::OutputTokens),
            (
                "cached_input_tokens",
                "cachedInputTokens",
                Metric::CachedTokens,
            ),
            (
                "reasoning_output_tokens",
                "reasoningOutputTokens",
                Metric::ReasoningTokens,
            ),
            ("total_tokens", "totalTokens", Metric::TotalTokens),
            (
                "cache_write_input_tokens",
                "",
                Metric::Extension {
                    namespace: "codex.usage".into(),
                    name: "cache_write_input_tokens".into(),
                },
            ),
        ] {
            if live && camel.is_empty() {
                continue;
            }
            let field = if live { camel } else { snake };
            let Some(number) = value.get(field).and_then(Value::as_number) else {
                continue;
            };
            observed = true;
            let reference = match &sample {
                Sample::Response(response) => MeasurementRef::native(
                    owner.clone(),
                    "codex.response-usage",
                    vec![(*response).into(), field.into()],
                )
                .unwrap(),
                _ => MeasurementRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["usage".into(), slot.into(), field.into()],
                )
                .unwrap(),
            };
            let mode = match &sample {
                Sample::Cumulative { series, reset } => MeasurementMode::Cumulative {
                    counter_domain: Some(correlation_key(
                        owner,
                        series,
                        &reset
                            .iter()
                            .map(|id| (*id).to_owned())
                            .chain([field.into()])
                            .collect::<Vec<_>>(),
                    )),
                    reset_epoch: reset.map(str::to_owned),
                    sample_position: self.input.capture.source_position.clone(),
                },
                _ => MeasurementMode::Gauge,
            };
            self.declare(
                owner,
                &format!("codex/usage/{slot}/{field}"),
                TraceObject::Measurement(Measurement {
                    measurement_ref: reference,
                    subject: subject.clone(),
                    model: None,
                    metric,
                    unit: MeasurementUnit::Tokens,
                    value: number.clone(),
                    mode,
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![],
                }),
            );
        }
        if observed && matches!(sample, Sample::Cumulative { reset: None, .. }) {
            self.issue(owner,&format!("codex/{slot}/unknown-counter-baseline"),DiagnosticCode::UnknownCounterBaseline,"Cumulative accounting has no evidenced reset epoch or zero baseline; no derived delta is claimed");
        }
    }
}
