//! Goal identity comes from the native instance, not objective text or arrival order.

use super::*;
use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};

impl Context<'_, '_> {
    pub(super) fn goal_update(&mut self, owner: &AgentRef, value: &Value) {
        let Some(goal) = value.get("goal").filter(|goal| goal.is_object()) else {
            self.issue(
                owner,
                "codex/goal-shape",
                DiagnosticCode::DecodeFailure,
                "Goal update lacks its structured goal state",
            );
            return;
        };
        for object in [value, goal] {
            if object.get("threadId").is_some_and(|id| {
                id.as_str().and_then(|id| self.agent(id)).as_ref() != Some(owner)
            }) {
                self.issue(
                    owner,
                    "codex/goal-owner",
                    DiagnosticCode::ConflictingOwner,
                    "Goal update belongs to another native thread or has an invalid threadId",
                );
                return;
            }
        }
        self.issue(owner, "codex/goal-instance-unresolved", DiagnosticCode::MissingSource,
            "Public ThreadGoal state omits its native goal_id; retained state cannot be joined to a goal instance by objective, timestamp or arrival order");
    }
}

pub(super) fn map_database(input: MapInput<'_>, snapshot: NativeDatabaseSnapshot) -> MapOutput {
    let mut output = MapOutput {
        state: input.state.clone(),
        ..Default::default()
    };
    if snapshot.status != NativeDatabaseStatus::Available {
        issue(
            &input,
            &mut output,
            DiagnosticCode::MissingSource,
            "Native goal metadata is unavailable; absence does not establish a goal outcome",
        );
        return output;
    }
    let (Some(NativeDatabaseValue::Text(thread)), Some(NativeDatabaseValue::Text(id))) =
        (snapshot.row.get("thread_id"), snapshot.row.get("goal_id"))
    else {
        issue(
            &input,
            &mut output,
            DiagnosticCode::MissingOwner,
            "Goal metadata lacks its actual thread and goal identity",
        );
        return output;
    };
    if thread.is_empty() || id.is_empty() {
        issue(
            &input,
            &mut output,
            DiagnosticCode::MissingOwner,
            "Native goal thread and instance identities must be nonempty",
        );
        return output;
    }
    let Issuer::Environment(environment) = &input.capture.observation_ref.issuer else {
        issue(
            &input,
            &mut output,
            DiagnosticCode::MissingOwner,
            "Native goal metadata requires its acquiring environment",
        );
        return output;
    };
    let owner = AgentRef::Session(SessionId::new(
        environment.clone(),
        AgentName::parse("codex").unwrap(),
        NativeId::new(thread),
    ));
    let resource = match ResourceRef::native(owner.clone(), "codex.goal", vec![id.clone()]) {
        Ok(reference) => reference,
        Err(error) => {
            issue(
                &input,
                &mut output,
                DiagnosticCode::MissingOwner,
                &format!("Invalid native goal identity: {error}"),
            );
            return output;
        }
    };
    output.state.governing_agent = Some(owner.clone());
    let add = |output: &mut MapOutput, slot: &str, fact: Fact| {
        output
            .claims
            .push(claim(&input, &owner, slot, CodexMapper::BUILD, fact));
    };
    add(
        &mut output,
        "codex/goal-agent",
        Fact::Declare {
            object: TraceObject::Agent(AgentThread {
                agent_ref: owner.clone(),
                vendor: AgentName::parse("codex").unwrap(),
                name: None,
                model: None,
            }),
        },
    );
    add(
        &mut output,
        "codex/goal-resource",
        Fact::Declare {
            object: TraceObject::Resource(Resource {
                resource_ref: resource.clone(),
                creator: None,
                host: Some(OwnerRef::Agent {
                    agent: owner.clone(),
                }),
                kind: ResourceKind::Goal,
            }),
        },
    );
    let state = match snapshot.row.get("status") {
        Some(NativeDatabaseValue::Text(status)) if !status.is_empty() => {
            ResourceState::GoalUpdated {
                status: status.clone(),
                detail: Some(input.capture.payload_ref.clone()),
            }
        }
        _ => {
            issue(
                &input,
                &mut output,
                DiagnosticCode::DecodeFailure,
                "Native goal status is missing or is not a nonempty SQLite text value",
            );
            ResourceState::Unknown {
                reason: "Native goal status was not observed in its required shape".into(),
            }
        }
    };
    add(
        &mut output,
        "codex/goal-state",
        Fact::ResourceState {
            resource: resource.clone(),
            state,
        },
    );
    for (field, metric, unit, cumulative) in [
        (
            "tokens_used",
            Metric::TotalTokens,
            MeasurementUnit::Tokens,
            true,
        ),
        (
            "time_used_seconds",
            Metric::ReportedDuration,
            MeasurementUnit::Seconds,
            true,
        ),
        (
            "token_budget",
            Metric::Extension {
                namespace: "codex.goal".into(),
                name: "token_budget".into(),
            },
            MeasurementUnit::Tokens,
            false,
        ),
    ] {
        if !cumulative
            && matches!(
                snapshot.row.get(field),
                None | Some(NativeDatabaseValue::Null)
            )
        {
            continue;
        }
        let value = match snapshot.row.get(field) {
            Some(NativeDatabaseValue::Integer(value)) if *value >= 0 => *value,
            _ => {
                issue(
                    &input,
                    &mut output,
                    DiagnosticCode::DecodeFailure,
                    &format!(
                        "Native goal {field} is missing or is not a nonnegative SQLite integer"
                    ),
                );
                continue;
            }
        };
        // The native instance is the reset domain. No zero sample, delta or
        // relationship to thread/response totals is invented from that identity.
        let mode = if cumulative {
            MeasurementMode::Cumulative {
                counter_domain: Some(correlation_key(
                    &owner,
                    "codex.goal-counter",
                    &[id.clone(), field.into()],
                )),
                reset_epoch: Some(id.clone()),
                sample_position: input.capture.source_position.clone(),
            }
        } else {
            MeasurementMode::Gauge
        };
        add(
            &mut output,
            &format!("codex/goal/{field}"),
            Fact::Declare {
                object: TraceObject::Measurement(Measurement {
                    measurement_ref: MeasurementRef::occurrence(
                        owner.clone(),
                        input.capture.source_position.clone(),
                        vec!["goal".into(), field.into()],
                    )
                    .unwrap(),
                    subject: AnyObjectRef::Resource(resource.clone()),
                    metric,
                    unit,
                    value: value.into(),
                    mode,
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![],
                }),
            },
        );
    }
    let key = correlation_key(&owner, "codex.goal-id", std::slice::from_ref(id));
    output
        .correlations
        .insert(key.clone(), AnyObjectRef::Resource(resource));
    output
        .correlation_evidence
        .insert(key, vec![input.canonical_observation.clone()]);
    output
}

fn issue(input: &MapInput<'_>, output: &mut MapOutput, code: DiagnosticCode, message: &str) {
    output.diagnostics.push(MappingDiagnostic {
        evidence: vec![input.canonical_observation.clone()],
        code,
        message: message.into(),
    });
}

#[cfg(test)]
mod tests;
