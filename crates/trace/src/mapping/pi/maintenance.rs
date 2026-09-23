use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ObservedWork {
    operation: OperationRef,
    start: ObservationRef,
    activation: Option<ActivationRef>,
}

impl Context<'_, '_> {
    pub(super) fn begin_work(
        &mut self,
        owner: &AgentRef,
        name: &str,
        category: OperationCategory,
        input: Option<PayloadRef>,
    ) -> ObservedWork {
        let operation = OperationRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec![name.into()],
        )
        .unwrap();
        let activation = self.state.outer.as_ref().map(|run| run.activation.clone());
        self.object(
            owner,
            &format!("pi/{name}/operation"),
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation: activation.clone(),
                requester: Some(RequesterRef::Agent(owner.clone())),
                category,
                input_revision: input,
                argument_state: ArgumentState::Sealed,
            }),
        );
        self.add(
            owner,
            &format!("pi/{name}/start"),
            Fact::Execution {
                execution: ExecutionRef::Operation(operation.clone()),
                boundary: ExecutionBoundary::Started { time: None },
            },
        );
        ObservedWork {
            operation,
            start: self.input.canonical_observation.clone(),
            activation,
        }
    }

    pub(super) fn finish_work(
        &mut self,
        owner: &AgentRef,
        work: &ObservedWork,
        payload: PayloadRef,
        outcome: TerminalOutcome,
        reason: Option<ExecutionCloseReason>,
        slot: &str,
    ) {
        let first = self.output.claims.len();
        self.result(
            owner,
            work.operation.clone(),
            payload.clone(),
            ResultRepresentation::Runtime,
            ResultDisposition::FinalReturn,
            slot,
        );
        self.add(
            owner,
            &format!("{slot}/end"),
            Fact::Execution {
                execution: ExecutionRef::Operation(work.operation.clone()),
                boundary: ExecutionBoundary::Terminated {
                    time: None,
                    reason: reason.clone(),
                },
            },
        );
        self.add(
            owner,
            &format!("{slot}/outcome"),
            Fact::Execution {
                execution: ExecutionRef::Operation(work.operation.clone()),
                boundary: ExecutionBoundary::Outcome {
                    outcome,
                    reason,
                    detail: Some(payload),
                },
            },
        );
        for claim in &mut self.output.claims[first..] {
            if !claim.evidence.contains(&work.start) {
                claim.evidence.push(work.start.clone());
            }
        }
    }

    pub(super) fn maintenance(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        match kind {
            "auto_retry_start" => {
                if value
                    .get("attempt")
                    .and_then(Value::as_u64)
                    .is_none_or(|value| value == 0)
                {
                    self.issue(
                        Some(owner),
                        "pi/retry-attempt",
                        DiagnosticCode::DecodeFailure,
                        "Retry scheduling lacks its positive attempt number",
                    );
                    return;
                }
                let payload = self.json(value, "/");
                let work = if let Some(work) = &self.state.retry {
                    work.clone()
                } else {
                    let work = self.begin_work(
                        owner,
                        "retry",
                        OperationCategory::Extension {
                            namespace: "pi.runtime".into(),
                            name: "retry".into(),
                        },
                        Some(payload.clone()),
                    );
                    self.state.retry = Some(work.clone());
                    work
                };
                // Pi schedules each attempt but emits retry_end only after final
                // recovery, exhaustion or cancellation. Intermediate schedules are
                // progress of this episode, not invented terminal boundaries.
                self.result(
                    owner,
                    work.operation,
                    payload,
                    ResultRepresentation::Runtime,
                    ResultDisposition::Progress,
                    "pi/retry-attempt",
                );
                self.output
                    .claims
                    .last_mut()
                    .unwrap()
                    .evidence
                    .push(work.start);
            }
            "auto_retry_end" => {
                let Some(work) = self.state.retry.take() else {
                    self.issue(
                        Some(owner),
                        "pi/retry-start-missing",
                        DiagnosticCode::CaptureGap,
                        "Retry completion has no observed scheduling start",
                    );
                    return;
                };
                let payload = self.json(value, "/");
                let outcome = match value.get("success").and_then(Value::as_bool) {
                    Some(true) => TerminalOutcome::Succeeded,
                    Some(false) => TerminalOutcome::Failed,
                    None => TerminalOutcome::Unknown,
                };
                self.finish_work(
                    owner,
                    &work,
                    payload,
                    outcome,
                    Some(ExecutionCloseReason::VendorStop),
                    "pi/retry",
                );
            }
            "compaction_start" => {
                if self.state.compaction.is_some() {
                    self.issue(
                        Some(owner),
                        "pi/compaction-overlap",
                        DiagnosticCode::CaptureGap,
                        "A compaction start replaced an operation with no observed end",
                    );
                }
                let payload = self.json(value, "/");
                let work = self.begin_work(
                    owner,
                    "compaction",
                    OperationCategory::Extension {
                        namespace: "pi.runtime".into(),
                        name: "compaction".into(),
                    },
                    Some(payload),
                );
                self.state.compaction = Some(work);
            }
            "compaction_end" => {
                let payload = self.json(value, "/");
                let aborted = value.get("aborted").and_then(Value::as_bool);
                let failed = value
                    .get("errorMessage")
                    .is_some_and(|error| !error.is_null());
                let result = value.get("result").filter(|result| {
                    result.get("summary").and_then(Value::as_str).is_some()
                        && text(result, "firstKeptEntryId").is_some_and(|id| !id.is_empty())
                        && result
                            .get("tokensBefore")
                            .and_then(Value::as_number)
                            .is_some()
                });
                if value.get("result").is_some_and(|result| !result.is_null()) && result.is_none() {
                    self.issue(
                        Some(owner),
                        "pi/compaction-result-shape",
                        DiagnosticCode::DecodeFailure,
                        "Compaction result lacks its summary, kept-entry identity or token count",
                    );
                }
                let outcome = if aborted == Some(true) {
                    TerminalOutcome::Interrupted
                } else if failed {
                    TerminalOutcome::Failed
                } else if aborted == Some(false) && result.is_some() {
                    TerminalOutcome::Succeeded
                } else {
                    TerminalOutcome::Unknown
                };
                if let Some(work) = self.state.compaction.take() {
                    self.finish_work(
                        owner,
                        &work,
                        payload,
                        outcome,
                        Some(ExecutionCloseReason::VendorStop),
                        "pi/compaction",
                    );
                } else {
                    self.issue(
                        Some(owner),
                        "pi/compaction-start-missing",
                        DiagnosticCode::CaptureGap,
                        "Compaction completion has no observed start",
                    );
                }
                if let Some(result) = result.filter(|_| outcome == TerminalOutcome::Succeeded) {
                    let payload = self.json(result, "/result");
                    self.object(owner, "pi/live-compaction-checkpoint", TraceObject::ContextCheckpoint(ContextCheckpoint {
                        checkpoint_ref: CheckpointRef::occurrence(owner.clone(), self.input.capture.source_position.clone(), vec!["compaction".into()]).unwrap(),
                        owner: OwnerRef::Agent { agent: owner.clone() }, basis: ContextBasis::Summary,
                        content: ContextContent::OpaqueReplacement { payload },
                        coverage: InclusionCoverage::Unknown { reason: "Live compaction result has no native checkpoint ID or complete context membership".into() },
                    }));
                }
            }
            _ => unreachable!("closed maintenance event set"),
        }
    }

    pub(super) fn release_unfinished_maintenance(
        &mut self,
        owner: &AgentRef,
        activation: &ActivationRef,
    ) {
        for (name, work) in [
            ("retry", &mut self.state.retry),
            ("compaction", &mut self.state.compaction),
        ] {
            if work
                .as_ref()
                .is_some_and(|work| work.activation.as_ref() == Some(activation))
            {
                let missing = work.take().unwrap();
                let mut diagnostic = claim(
                    self.input,
                    owner,
                    &format!("pi/{name}/end-missing"),
                    PiMapper.build(),
                    Fact::Diagnostic {
                        diagnostic: TraceDiagnostic {
                            subject: Some(AnyObjectRef::Operation(missing.operation)),
                            code: DiagnosticCode::CaptureGap,
                            message: format!(
                                "Outer settlement was observed without {name} completion"
                            ),
                            related_claims: vec![],
                        },
                    },
                );
                diagnostic.evidence.push(missing.start);
                self.output.claims.push(diagnostic);
            }
        }
    }
}
