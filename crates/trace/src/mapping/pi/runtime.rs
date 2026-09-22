//! Per-invocation runtime associations and exact sidecar source locations.
use super::*;

impl Context<'_, '_> {
    pub(super) fn bind_tool_sidecars(
        &mut self,
        operation: &OperationRef,
        details: &Value,
        slot: &str,
        proof: &[ObservationRef],
    ) {
        for field in ["fullOutputPath", "persistedOutputPath", "outputFile"] {
            let Some(path) = text(details, field).filter(|path| !path.is_empty()) else {
                continue;
            };
            let key = super::super::attachment_key(self.input.canonical_observation, slot, path);
            self.output
                .correlations
                .insert(key.clone(), AnyObjectRef::Operation(operation.clone()));
            let mut evidence = proof.to_vec();
            evidence.push(self.input.canonical_observation.clone());
            if let Some(header) = &self.state.header_evidence {
                evidence.push(header.clone());
            }
            evidence.sort();
            evidence.dedup();
            self.output.correlation_evidence.insert(key, evidence);
        }
    }

    fn live_call(&self, id: &str) -> Option<TrackedTool> {
        self.state
            .current_pending
            .get(id)
            .filter(|pending| {
                pending.awaiting_execution && matches!(pending.operation.address(), ObjectAddress::Occurrence { position, .. }
                if position.source == self.input.capture.source_position.source
                    && position.generation == self.input.capture.source_position.generation)
            })
            .cloned()
    }

    pub(super) fn runtime_tool(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        let Some(id) = text(value, "toolCallId").filter(|id| !id.is_empty()) else {
            self.issue(
                Some(owner),
                "pi/tool-id",
                DiagnosticCode::MissingOwner,
                "Runtime tool event lacks its nonempty toolCallId",
            );
            return;
        };
        let first = self.output.claims.len();
        let starts = kind == "tool_execution_start";
        if starts && self.state.runtime_tools.contains_key(id) {
            self.issue(Some(owner), "pi/runtime-overlap", DiagnosticCode::CaptureGap,
                "A repeated tool start replaces an invocation with no observed end; its outcome remains unknown");
        }
        let existing = if starts {
            None
        } else {
            self.state.runtime_tools.get(id).cloned()
        };
        let missing_start = !starts && existing.is_none();
        let mut work = existing
            .or_else(|| self.live_call(id))
            .unwrap_or_else(|| TrackedTool {
                operation: OperationRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["runtime-tool".into(), id.into()],
                )
                .unwrap(),
                evidence: vec![],
                activation: self.state.outer.as_ref().map(|run| run.activation.clone()),
                awaiting_execution: false,
            });
        // Do not reuse an already-running model association when the vendor has
        // emitted another start with the same ID. That is another observed start,
        // not a correction or a receipt duplicate (receipts are deduplicated earlier).
        if starts
            && self
                .state
                .runtime_tools
                .get(id)
                .is_some_and(|previous| previous.operation == work.operation)
        {
            work.operation = OperationRef::occurrence(
                owner.clone(),
                self.input.capture.source_position.clone(),
                vec!["runtime-tool".into(), id.into()],
            )
            .unwrap();
            work.evidence.clear();
        }
        if starts || missing_start {
            work.evidence.push(self.input.canonical_observation.clone());
        }
        work.awaiting_execution = false;
        work.evidence.sort();
        work.evidence.dedup();
        if starts {
            if let Some(name) = text(value, "toolName").filter(|name| !name.is_empty()) {
                let input = value.get("args").map(|args| self.json(args, "/args"));
                self.object(
                    owner,
                    "pi/runtime-tool",
                    TraceObject::Operation(Operation {
                        operation_ref: work.operation.clone(),
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: work.activation.clone(),
                        requester: Some(RequesterRef::Agent(owner.clone())),
                        category: OperationCategory::Tool {
                            namespace: Some("pi".into()),
                            name: name.into(),
                        },
                        input_revision: input,
                        argument_state: ArgumentState::Sealed,
                    }),
                );
            } else {
                self.issue(
                    Some(owner),
                    "pi/tool-name",
                    DiagnosticCode::DecodeFailure,
                    "Runtime tool start lacks its toolName",
                );
            }
            self.add(
                owner,
                "pi/runtime-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(work.operation.clone()),
                    boundary: ExecutionBoundary::Started { time: None },
                },
            );
        } else {
            if missing_start {
                self.issue(Some(owner), "pi/runtime-start-missing", DiagnosticCode::CaptureGap,
                    "Runtime result has no observed active start; no earlier invocation is reused by producer epoch");
            }
            let field = if kind == "tool_execution_update" {
                "partialResult"
            } else {
                "result"
            };
            if let Some(result) = value.get(field) {
                let payload = self.json(result, &format!("/{field}"));
                self.result(
                    owner,
                    work.operation.clone(),
                    payload,
                    Representation::Runtime,
                    if kind == "tool_execution_update" {
                        ResultDisposition::Progress
                    } else {
                        ResultDisposition::FinalReturn
                    },
                    "pi/runtime",
                );
                self.bind_tool_sidecars(
                    &work.operation,
                    &result["details"],
                    &format!("/{field}"),
                    &work.evidence,
                );
            } else {
                self.issue(
                    Some(owner),
                    "pi/runtime-result-missing",
                    DiagnosticCode::DecodeFailure,
                    "Runtime result field is absent; it is not an observed null result",
                );
            }
            if kind == "tool_execution_end" {
                let outcome = match value.get("isError").and_then(Value::as_bool) {
                    Some(true) => TerminalOutcome::Failed,
                    Some(false) => TerminalOutcome::Succeeded,
                    None => TerminalOutcome::Unknown,
                };
                self.add(
                    owner,
                    "pi/runtime-outcome",
                    Fact::Execution {
                        execution: ExecutionRef::Operation(work.operation.clone()),
                        boundary: ExecutionBoundary::Outcome {
                            outcome,
                            reason: None,
                            detail: None,
                        },
                    },
                );
            }
        }
        for claim in &mut self.output.claims[first..] {
            claim.evidence.extend(work.evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
        self.state.current_pending.insert(id.into(), work.clone());
        if kind == "tool_execution_end" {
            self.state.runtime_tools.remove(id);
        } else {
            self.state.runtime_tools.insert(id.into(), work);
        }
    }

    pub(super) fn release_runtime_tools(
        &mut self,
        owner: &AgentRef,
        activation: Option<&ActivationRef>,
    ) {
        let released: Vec<_> = self
            .state
            .runtime_tools
            .iter()
            .filter(|(_, tool)| {
                activation.is_none_or(|activation| tool.activation.as_ref() == Some(activation))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in released {
            let work = self.state.runtime_tools.remove(&id).unwrap();
            let first = self.output.claims.len();
            self.add(owner, &format!("pi/runtime-end-missing/{id}"), Fact::Diagnostic { diagnostic: TraceDiagnostic {
                subject: Some(AnyObjectRef::Operation(work.operation)), code: DiagnosticCode::CaptureGap,
                message: "An outer-run boundary released an unfinished runtime association without inventing its outcome".into(), related_claims: vec![],
            } });
            self.output.claims[first].evidence.extend(work.evidence);
            self.output.claims[first].evidence.sort();
            self.output.claims[first].evidence.dedup();
        }
    }
}

impl Context<'_, '_> {
    fn inner_result_key(inner: &InnerMessages, index: u64, id: &str) -> Vec<String> {
        vec![
            serde_json::to_string(&inner.start).expect("typed source position"),
            index.to_string(),
            id.into(),
        ]
    }

    pub(super) fn record_inner_result(
        &mut self,
        owner: &AgentRef,
        id: &str,
        index: u64,
        pending: &TrackedTool,
    ) {
        let Some(inner) = self.state.inner.as_ref().filter(|inner| inner.coherent) else {
            return;
        };
        let key = correlation_key(
            owner,
            "pi.inner-result",
            &Self::inner_result_key(inner, index, id),
        );
        self.output.correlations.insert(
            key.clone(),
            AnyObjectRef::Operation(pending.operation.clone()),
        );
        let mut evidence = pending.evidence.clone();
        evidence.extend([
            inner.evidence.clone(),
            self.input.canonical_observation.clone(),
        ]);
        evidence.sort();
        evidence.dedup();
        self.output.correlation_evidence.insert(key, evidence);
    }

    pub(super) fn echo_tool_sidecars(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        let field = if kind == "agent_end" {
            "messages"
        } else {
            "toolResults"
        };
        let Some(messages) = value.get(field).and_then(Value::as_array) else {
            return;
        };
        let offset = self
            .state
            .inner
            .as_ref()
            .filter(|inner| inner.coherent)
            .and_then(|inner| {
                let offset = if kind == "agent_end" {
                    Some(0)
                } else {
                    inner.assistant.and_then(|index| index.checked_add(1))
                }?;
                (offset.checked_add(messages.len() as u64) == Some(inner.count)).then_some(offset)
            });
        // The pinned agent loop appends each emitted message to newMessages and
        // emits tool-result messages serially after their assistant. A contiguous
        // observed prefix and exact message count certify these ordinal joins;
        // matching text, timestamp or a reused toolCallId alone does not.
        for (index, message) in messages.iter().enumerate() {
            if text(message, "role") != Some("toolResult")
                || !["fullOutputPath", "persistedOutputPath", "outputFile"]
                    .iter()
                    .any(|field| text(&message["details"], field).is_some())
            {
                continue;
            }
            let Some(id) = text(message, "toolCallId") else {
                continue;
            };
            let Some(offset) = offset else {
                self.issue(Some(owner), "pi/echo-prefix-gap", DiagnosticCode::CaptureGap,
                    "Echoed tool output has no complete observed message-prefix certificate; its invocation remains unresolved");
                return;
            };
            let parts = Self::inner_result_key(
                self.state.inner.as_ref().unwrap(),
                offset + index as u64,
                id,
            );
            let key = correlation_key(owner, "pi.inner-result", &parts);
            let Some(AnyObjectRef::Operation(operation)) =
                self.input.archive.correlations.get(&key).cloned()
            else {
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: owner.clone(),
                    namespace: "pi.inner-result".into(),
                    key: parts,
                });
                continue;
            };
            let Some(proof) = self
                .input
                .archive
                .correlation_evidence
                .get(&key)
                .filter(|proof| {
                    !proof.is_empty()
                        && proof.iter().all(|observation| {
                            self.input.archive.observations.contains(observation)
                        })
                })
                .cloned()
            else {
                self.issue(
                    Some(owner),
                    "pi/echo-proof-missing",
                    DiagnosticCode::MissingSource,
                    "Echoed tool result lacks retained association evidence",
                );
                continue;
            };
            self.bind_tool_sidecars(
                &operation,
                &message["details"],
                &format!("/{field}/{index}"),
                &proof,
            );
        }
    }
}
