//! Reused task handles route observations; each observed start owns a new activity.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Episode {
    resource: ResourceRef,
    invocation: Option<String>,
    start: ObservationRef,
    ambiguous: bool,
    closed: bool,
}
impl Context<'_, '_> {
    fn task_binding(&self, owner: &AgentRef, id: &str, invocation: &str) -> String {
        correlation_key(
            owner,
            "claude.task-invocation",
            &[
                self.input.capture.source_position.source.encode(),
                self.input.capture.source_position.generation.to_string(),
                id.into(),
                invocation.into(),
            ],
        )
    }
    pub(super) fn task(&mut self, owner: &AgentRef, value: &Value) {
        let Some(id) = text(value, "task_id").filter(|id| !id.is_empty()) else {
            self.issue(
                Some(owner),
                "claude/task-id",
                DiagnosticCode::MissingOwner,
                "Task event has no nonempty task_id",
            );
            return;
        };
        let key = correlation_key(owner, "claude.active-task", &[id.into()]);
        let invocation = text(value, "tool_use_id").filter(|id| !id.is_empty());
        let started = text(value, "subtype") == Some("task_started");
        let previous = self.state.tasks.get(&key).cloned();
        let (resource, mut evidence) = if started {
            let resource = if let Some(uuid) = text(value, "uuid").filter(|id| !id.is_empty()) {
                ResourceRef::native(
                    owner.clone(),
                    "claude.task-start",
                    vec![
                        self.input.capture.source_position.source.encode(),
                        self.input.capture.source_position.generation.to_string(),
                        uuid.into(),
                    ],
                )
                .unwrap()
            } else {
                ResourceRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["claude.task".into(), id.into()],
                )
                .unwrap()
            };
            let ambiguous = previous
                .as_ref()
                .is_some_and(|episode| !episode.closed && episode.resource != resource);
            if ambiguous {
                self.issue(Some(owner), "claude/task-overlap", DiagnosticCode::CaptureGap,
                    "A task handle was restarted before its prior activity notification; unqualified updates cannot identify an episode");
            }
            self.state.tasks.insert(
                key.clone(),
                Episode {
                    resource: resource.clone(),
                    invocation: invocation.map(str::to_owned),
                    start: self.input.canonical_observation.clone(),
                    ambiguous,
                    closed: false,
                },
            );
            if let Some(invocation) = invocation {
                let binding = self.task_binding(owner, id, invocation);
                self.output
                    .correlations
                    .insert(binding.clone(), AnyObjectRef::Resource(resource.clone()));
                self.output
                    .correlation_evidence
                    .insert(binding, vec![self.input.canonical_observation.clone()]);
            }
            self.object(
                owner,
                "claude/task-resource",
                TraceObject::Resource(Resource {
                    resource_ref: resource.clone(),
                    creator: invocation.map(|id| operation(owner, id)),
                    host: Some(OwnerRef::Agent {
                        agent: owner.clone(),
                    }),
                    kind: ResourceKind::Extension {
                        namespace: "claude.task".into(),
                        name: text(value, "task_type").unwrap_or("task").into(),
                    },
                }),
            );
            (resource, vec![])
        } else if let Some(invocation) = invocation {
            let binding = self.task_binding(owner, id, invocation);
            if let Some(AnyObjectRef::Resource(resource)) =
                self.input.archive.correlations.get(&binding)
            {
                (
                    resource.clone(),
                    self.input
                        .archive
                        .correlation_evidence
                        .get(&binding)
                        .cloned()
                        .unwrap_or_default(),
                )
            } else if let Some(previous) = previous
                .as_ref()
                .filter(|episode| episode.invocation.as_deref() == Some(invocation))
            {
                (previous.resource.clone(), vec![previous.start.clone()])
            } else {
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: owner.clone(),
                    namespace: "claude.task-invocation".into(),
                    key: vec![
                        self.input.capture.source_position.source.encode(),
                        self.input.capture.source_position.generation.to_string(),
                        id.into(),
                        invocation.into(),
                    ],
                });
                return;
            }
        } else if let Some(previous) = previous.as_ref().filter(|episode| !episode.ambiguous) {
            (previous.resource.clone(), vec![previous.start.clone()])
        } else {
            self.issue(
                Some(owner),
                "claude/task-episode-missing",
                DiagnosticCode::MissingSource,
                "Task state has no unambiguous retained start or invocation identity",
            );
            return;
        };
        let status = text(&value["patch"], "status").or_else(|| text(value, "status"));
        let first_measurement = self.output.claims.len();
        self.measurements(
            owner,
            &value["usage"],
            "claude/task-usage",
            AnyObjectRef::Resource(resource.clone()),
        );
        for (field, metric, unit) in [
            ("total_tokens", Metric::TotalTokens, MeasurementUnit::Tokens),
            (
                "duration_ms",
                Metric::ReportedDuration,
                MeasurementUnit::Milliseconds,
            ),
            (
                "tool_uses",
                Metric::Extension {
                    namespace: "claude.task".into(),
                    name: "tool_uses".into(),
                },
                MeasurementUnit::Count,
            ),
        ] {
            if let Some(number) = value["usage"].get(field).and_then(Value::as_number) {
                self.object(
                    owner,
                    &format!("claude/task-usage/{field}"),
                    TraceObject::Measurement(Measurement {
                        measurement_ref: MeasurementRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec!["task-usage".into(), field.into()],
                        )
                        .unwrap(),
                        subject: AnyObjectRef::Resource(resource.clone()),
                        model: None,
                        metric,
                        unit,
                        value: number.clone(),
                        mode: MeasurementMode::Gauge,
                        aggregation_scope: AggregationScope::NamedSubject,
                        components: vec![],
                    }),
                );
            }
        }
        for claim in &mut self.output.claims[first_measurement..] {
            claim.evidence.extend(evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
        if !started && status.is_none() {
            return;
        }
        let state = if started {
            ResourceState::Running
        } else {
            match status {
                Some("completed") => ResourceState::Closed {
                    outcome: TerminalOutcome::Succeeded,
                    reason: None,
                },
                Some("failed") => ResourceState::Closed {
                    outcome: TerminalOutcome::Failed,
                    reason: None,
                },
                Some("stopped") => ResourceState::Closed {
                    outcome: TerminalOutcome::Interrupted,
                    reason: None,
                },
                Some("running") => ResourceState::Running,
                _ => ResourceState::Unknown {
                    reason: "Task status does not establish a terminal outcome".into(),
                },
            }
        };
        if let Some(episode) = self
            .state
            .tasks
            .get_mut(&key)
            .filter(|episode| episode.resource == resource)
        {
            episode.closed |= matches!(state, ResourceState::Closed { .. });
        }
        self.add(
            owner,
            "claude/task-state",
            Fact::ResourceState {
                resource: resource.clone(),
                state,
            },
        );
        evidence.push(self.input.canonical_observation.clone());
        evidence.sort();
        evidence.dedup();
        self.output
            .claims
            .last_mut()
            .unwrap()
            .evidence
            .extend(evidence);
        self.output.claims.last_mut().unwrap().evidence.sort();
        self.output.claims.last_mut().unwrap().evidence.dedup();
        if text(value, "subtype") == Some("task_notification")
            && self
                .state
                .tasks
                .get(&key)
                .is_some_and(|episode| episode.resource == resource)
        {
            self.state.tasks.remove(&key);
        }
    }
}
