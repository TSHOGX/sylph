//! Workflow hosts are resources. Explicit child IDs and journal keys keep their
//! own scopes; a completed host cannot settle an unobserved child activation.

use super::*;
use crate::mapping::SourceOwner;

fn resource(root: &AgentRef, run: &str) -> ResourceRef {
    ResourceRef::native(root.clone(), "claude.workflow", vec![run.into()]).unwrap()
}

impl Context<'_, '_> {
    pub(super) fn workflow_launch(
        &mut self,
        owner: &AgentRef,
        root: &AgentRef,
        creator: &OperationRef,
        value: &Value,
        slot: &str,
    ) {
        let Some(run) = text(value, "runId").filter(|id| !id.is_empty()) else {
            self.issue(
                Some(owner),
                &format!("{slot}/workflow-id"),
                DiagnosticCode::DecodeFailure,
                "Workflow launch result lacks its runId",
            );
            return;
        };
        let resource = resource(root, run);
        self.workflow_resource(root, &resource, creator, "claude/workflow/launch");
        self.output.correlations.insert(
            correlation_key(root, "claude.workflow-run", &[run.into()]),
            AnyObjectRef::Operation(creator.clone()),
        );
        if let Some(directory) = text(value, "transcriptDir").filter(|path| !path.is_empty()) {
            // This is a locator recorded by the vendor, never a filename-derived
            // agent identity. A retained source definition must match it exactly.
            let locator = format!("{}/journal.jsonl", directory.trim_end_matches('/'));
            self.output.correlations.insert(
                correlation_key(root, "claude.workflow-journal", &[locator]),
                AnyObjectRef::Resource(resource.clone()),
            );
        }
        if text(value, "status") == Some("async_launched") {
            self.add(
                root,
                "claude/workflow/launched",
                Fact::ResourceState {
                    resource,
                    state: ResourceState::Running,
                },
            );
        }
    }

    fn workflow_resource(
        &mut self,
        root: &AgentRef,
        resource: &ResourceRef,
        creator: &OperationRef,
        slot: &str,
    ) {
        self.object(
            root,
            &format!("{slot}/resource"),
            TraceObject::Resource(Resource {
                resource_ref: resource.clone(),
                creator: Some(creator.clone()),
                host: Some(OwnerRef::Agent {
                    agent: creator.scope().clone(),
                }),
                kind: ResourceKind::Extension {
                    namespace: "claude".into(),
                    name: "workflow".into(),
                },
            }),
        );
        self.add(
            root,
            &format!("{slot}/resource-use"),
            Fact::Relation {
                relation: Relation::UsesResource {
                    operation: creator.clone(),
                    resource: resource.clone(),
                },
            },
        );
    }

    fn workflow_correlation(
        &mut self,
        root: &AgentRef,
        namespace: &str,
        key: &str,
    ) -> Option<(AnyObjectRef, Vec<ObservationRef>)> {
        let index = correlation_key(root, namespace, &[key.into()]);
        let Some(reference) = self.input.archive.correlations.get(&index).cloned() else {
            self.request(root, namespace, vec![key.into()]);
            return None;
        };
        let Some(evidence) = self
            .input
            .archive
            .correlation_evidence(&index)
            .filter(|proof| {
                !proof.is_empty()
                    && proof
                        .iter()
                        .all(|item| self.input.archive.observations.contains(item))
            })
        else {
            self.issue(
                Some(root),
                "claude/workflow/join-proof",
                DiagnosticCode::MissingSource,
                "Workflow correlation lacks retained supporting observations",
            );
            return None;
        };
        Some((reference, evidence.clone()))
    }

    fn workflow_proof(&mut self, first: usize, evidence: &[ObservationRef]) {
        for claim in &mut self.output.claims[first..] {
            claim.evidence.extend_from_slice(evidence);
            claim.evidence.sort();
            claim.evidence.dedup();
        }
    }

    pub(super) fn workflow_manifest(&mut self, value: &Value, provenance: &SourceOwner) {
        let root = root_of(&provenance.agent);
        let Some(run) = text(value, "runId").filter(|id| !id.is_empty()) else {
            self.issue(
                Some(&root),
                "claude/workflow/manifest-id",
                DiagnosticCode::DecodeFailure,
                "Workflow manifest lacks its runId",
            );
            return;
        };
        let Some((AnyObjectRef::Operation(creator), mut evidence)) =
            self.workflow_correlation(&root, "claude.workflow-run", run)
        else {
            return;
        };
        evidence.extend(provenance.evidence.iter().cloned());
        let first = self.output.claims.len();
        let resource = resource(&root, run);
        self.workflow_resource(&root, &resource, &creator, "claude/workflow/manifest");
        let payload = self.json(value, "/");
        self.result(
            &root,
            &creator,
            payload.clone(),
            ResultRepresentation::Runtime,
            "claude/workflow/manifest",
        );
        self.workflow_state(
            &root,
            resource.clone(),
            text(value, "status"),
            Some(payload.reference),
            "claude/workflow/state",
        );
        self.workflow_started(
            &root,
            &resource,
            value,
            "startTime",
            "claude/workflow/start",
        );
        self.workflow_measurements(
            &root,
            value,
            AnyObjectRef::Resource(resource),
            "claude/workflow/usage",
            true,
        );
        if let Some(progress) = value.get("workflowProgress").and_then(Value::as_array) {
            for (position, item) in progress.iter().enumerate() {
                if text(item, "type") == Some("workflow_phase") {
                    continue;
                }
                if text(item, "type") != Some("workflow_agent") {
                    self.issue(
                        Some(&root),
                        &format!("claude/workflow/progress/{position}/kind"),
                        DiagnosticCode::UnsupportedKind,
                        "Unknown workflow progress entry remains retained",
                    );
                    continue;
                }
                let Some(id) = text(item, "agentId").filter(|id| !id.is_empty()) else {
                    continue;
                };
                let child = Self::child(&root, id);
                self.declare_agent(
                    &child,
                    text(item, "label").map(str::to_owned),
                    text(item, "model").map(str::to_owned),
                );
                let slot = format!("claude/workflow/progress/{position}");
                self.add(
                    &root,
                    &format!("{slot}/interaction"),
                    Fact::Relation {
                        relation: Relation::Interaction {
                            sender: creator.scope().clone(),
                            recipient: child.clone(),
                            delivery: None,
                            operation: Some(creator.clone()),
                        },
                    },
                );
                // A progress index is scoped to this run and attempt. It is not
                // the journal key; neither text nor matching child IDs proves that join.
                let (Some(index), Some(attempt)) = (
                    item.get("index").and_then(Value::as_u64).filter(|n| *n > 0),
                    item.get("attempt")
                        .and_then(Value::as_u64)
                        .filter(|n| *n > 0),
                ) else {
                    self.issue(
                        Some(&root),
                        &format!("{slot}/identity"),
                        DiagnosticCode::DecodeFailure,
                        "Workflow child activity lacks its positive index or attempt",
                    );
                    continue;
                };
                let activity = ResourceRef::native(
                    root.clone(),
                    "claude.workflow-activity",
                    vec![run.into(), index.to_string(), attempt.to_string()],
                )
                .unwrap();
                self.object(
                    &root,
                    &format!("{slot}/activity"),
                    TraceObject::Resource(Resource {
                        resource_ref: activity.clone(),
                        creator: Some(creator.clone()),
                        host: Some(OwnerRef::Agent { agent: child }),
                        kind: ResourceKind::Extension {
                            namespace: "claude.workflow".into(),
                            name: "child-activity".into(),
                        },
                    }),
                );
                let detail = self.json(item, &format!("/workflowProgress/{position}"));
                self.workflow_state(
                    &root,
                    activity.clone(),
                    text(item, "state"),
                    Some(detail.reference),
                    &slot,
                );
                self.workflow_started(
                    &root,
                    &activity,
                    item,
                    "startedAt",
                    &format!("{slot}/start"),
                );
                self.workflow_measurements(
                    &root,
                    item,
                    AnyObjectRef::Resource(activity),
                    &slot,
                    false,
                );
            }
        } else {
            self.issue(
                Some(&root),
                "claude/workflow/progress-shape",
                DiagnosticCode::DecodeFailure,
                "Workflow manifest has no progress array; child membership is unknown",
            );
        }
        self.workflow_proof(first, &evidence);
    }

    fn workflow_state(
        &mut self,
        root: &AgentRef,
        resource: ResourceRef,
        status: Option<&str>,
        detail: Option<PayloadRef>,
        slot: &str,
    ) {
        let outcome = match status {
            Some("completed" | "done") => Some(TerminalOutcome::Succeeded),
            Some("failed" | "error") => Some(TerminalOutcome::Failed),
            _ => None,
        };
        let state = match (status, outcome) {
            (_, Some(outcome)) => ResourceState::Closed {
                outcome,
                reason: Some(ExecutionCloseReason::VendorStop),
            },
            (Some("running" | "start"), _) => ResourceState::Running,
            _ => ResourceState::Unknown {
                reason: "Workflow status has no supported lifecycle interpretation".into(),
            },
        };
        self.add(
            root,
            &format!("{slot}/state"),
            Fact::ResourceState {
                resource: resource.clone(),
                state,
            },
        );
        if let Some(outcome) = outcome {
            self.add(
                root,
                &format!("{slot}/end"),
                Fact::Execution {
                    execution: ExecutionRef::Resource(resource.clone()),
                    boundary: ExecutionBoundary::Terminated {
                        time: None,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                },
            );
            self.add(
                root,
                &format!("{slot}/outcome"),
                Fact::Execution {
                    execution: ExecutionRef::Resource(resource),
                    boundary: ExecutionBoundary::Outcome {
                        outcome,
                        reason: Some(ExecutionCloseReason::VendorStop),
                        detail,
                    },
                },
            );
        }
    }

    fn workflow_started(
        &mut self,
        root: &AgentRef,
        resource: &ResourceRef,
        value: &Value,
        field: &str,
        slot: &str,
    ) {
        let Some(stamp) = value.get(field).and_then(Value::as_i64) else {
            return;
        };
        self.add(
            root,
            slot,
            Fact::Execution {
                execution: ExecutionRef::Resource(resource.clone()),
                boundary: ExecutionBoundary::Started {
                    time: Some(TimeEvidence {
                        domain: format!("claude-wall:{}", root.environment()),
                        value: stamp,
                        unit: TimeUnit::Milliseconds,
                        precision: Some(1),
                        basis: TimeBasis::SourceWall,
                    }),
                },
            },
        );
    }

    fn workflow_measurements(
        &mut self,
        root: &AgentRef,
        value: &Value,
        subject: AnyObjectRef,
        slot: &str,
        total: bool,
    ) {
        for (field, metric, unit) in [
            (
                if total { "totalTokens" } else { "tokens" },
                Metric::TotalTokens,
                MeasurementUnit::Tokens,
            ),
            (
                if total { "totalToolCalls" } else { "toolCalls" },
                Metric::Extension {
                    namespace: "claude.workflow".into(),
                    name: "tool_calls".into(),
                },
                MeasurementUnit::Count,
            ),
            (
                "durationMs",
                Metric::ReportedDuration,
                MeasurementUnit::Milliseconds,
            ),
        ] {
            let Some(number) = value.get(field).and_then(Value::as_number) else {
                continue;
            };
            self.object(
                root,
                &format!("{slot}/{field}"),
                TraceObject::Measurement(Measurement {
                    model: None,
                    measurement_ref: MeasurementRef::occurrence(
                        root.clone(),
                        self.input.capture.source_position.clone(),
                        vec![slot.into(), field.into()],
                    )
                    .unwrap(),
                    subject: subject.clone(),
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

    pub(super) fn workflow_journal(&mut self, value: &Value) {
        let position = &self.input.capture.source_position;
        let key = (position.source.clone(), position.generation);
        let Some(source) = self.input.archive.source_definitions.get(&key).cloned() else {
            self.output.requests.push(ArchiveRequest::Source {
                source: position.source.clone(),
                generation: position.generation,
            });
            return;
        };
        let Some(provenance) = self
            .input
            .archive
            .source_owners
            .get(&key)
            .filter(|owner| {
                !owner.evidence.is_empty()
                    && owner
                        .evidence
                        .iter()
                        .all(|item| self.input.archive.observations.contains(item))
            })
            .cloned()
        else {
            self.output.requests.push(ArchiveRequest::Source {
                source: position.source.clone(),
                generation: position.generation,
            });
            return;
        };
        let root = root_of(&provenance.agent);
        let Some((AnyObjectRef::Resource(resource), mut evidence)) =
            self.workflow_correlation(&root, "claude.workflow-journal", &source.definition.locator)
        else {
            return;
        };
        let ObjectAddress::Native { namespace, key, .. } = resource.address() else {
            return;
        };
        if namespace != "claude.workflow" || key.len() != 1 {
            return;
        }
        let run = key[0].clone();
        let Some((AnyObjectRef::Operation(creator), launch_evidence)) =
            self.workflow_correlation(&root, "claude.workflow-run", &run)
        else {
            return;
        };
        evidence.extend(launch_evidence);
        evidence.extend(provenance.evidence);
        evidence.push(source.observation);
        let (Some(key), Some(child_id)) = (
            text(value, "key").filter(|id| !id.is_empty()),
            text(value, "agentId").filter(|id| !id.is_empty()),
        ) else {
            self.issue(
                Some(&root),
                "claude/workflow/journal-identity",
                DiagnosticCode::DecodeFailure,
                "Workflow journal invocation lacks its key or agentId",
            );
            return;
        };
        let kind = text(value, "type");
        if !matches!(kind, Some("started" | "result")) {
            self.issue(
                Some(&root),
                "claude/workflow/journal-kind",
                DiagnosticCode::UnsupportedKind,
                "Unknown workflow journal record remains retained",
            );
            return;
        }
        let first = self.output.claims.len();
        let child = Self::child(&root, child_id);
        self.declare_agent(&child, None, None);
        let operation = OperationRef::native(
            creator.scope().clone(),
            "claude.workflow-invocation",
            vec![run, key.into()],
        )
        .unwrap();
        self.object(
            &root,
            "claude/workflow/journal-operation",
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: creator.scope().clone(),
                },
                activation: None,
                requester: Some(RequesterRef::Agent(creator.scope().clone())),
                category: OperationCategory::Delegation,
                input_revision: None,
                argument_state: ArgumentState::Sealed,
            }),
        );
        self.add(
            &root,
            "claude/workflow/journal-resource",
            Fact::Relation {
                relation: Relation::UsesResource {
                    operation: operation.clone(),
                    resource,
                },
            },
        );
        self.add(
            &root,
            "claude/workflow/journal-nesting",
            Fact::Relation {
                relation: Relation::Nesting {
                    enclosing: creator,
                    nested: operation.clone(),
                },
            },
        );
        self.add(
            &root,
            "claude/workflow/journal-interaction",
            Fact::Relation {
                relation: Relation::Interaction {
                    sender: operation.scope().clone(),
                    recipient: child,
                    delivery: None,
                    operation: Some(operation.clone()),
                },
            },
        );
        if kind == Some("started") {
            self.add(
                &root,
                "claude/workflow/journal-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation),
                    boundary: ExecutionBoundary::Started { time: None },
                },
            );
        } else if let Some(result) = value.get("result") {
            let payload = self.json(result, "/result");
            self.result(
                &root,
                &operation,
                payload.clone(),
                ResultRepresentation::Runtime,
                "claude/workflow/journal-result",
            );
            self.add(
                &root,
                "claude/workflow/journal-end",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation.clone()),
                    boundary: ExecutionBoundary::Terminated {
                        time: None,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                },
            );
            self.add(
                &root,
                "claude/workflow/journal-outcome",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation),
                    boundary: ExecutionBoundary::Outcome {
                        outcome: TerminalOutcome::Succeeded,
                        reason: Some(ExecutionCloseReason::VendorStop),
                        detail: Some(payload.reference),
                    },
                },
            );
        } else {
            self.issue(
                Some(&root),
                "claude/workflow/journal-result-missing",
                DiagnosticCode::DecodeFailure,
                "Workflow journal result record has no result field",
            );
        }
        self.workflow_proof(first, &evidence);
    }
}
