//! Runtime identities come from Codex's retained trace/protocol envelopes. JavaScript
//! source and model tool names never establish which nested calls actually executed.
use super::*;
use base64::Engine;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct RuntimeState {
    pub operations: BTreeMap<String, Operation>,
    cells: BTreeMap<String, (ResourceRef, OperationRef)>,
    processes: BTreeMap<String, ResourceRef>,
}
impl Context<'_, '_> {
    fn runtime_key(&self, owner: &AgentRef, incarnation: &str, id: &str) -> String {
        correlation_key(owner, "codex.runtime", &[incarnation.into(), id.into()])
    }
    fn runtime_issue(
        &mut self,
        owner: &AgentRef,
        subject: AnyObjectRef,
        slot: &str,
        message: &str,
    ) {
        self.add(
            owner,
            slot,
            Fact::Coverage {
                coverage: Coverage {
                    subject: subject.clone(),
                    source: self.input.capture.source_position.source.clone(),
                    dimension: CoverageDimension::PayloadRanges,
                    interval: CoverageInterval::Object { object: subject },
                    state: CoverageState::Gap {
                        reason: message.into(),
                    },
                },
            },
        );
    }
    fn runtime_result(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        slot: &str,
        payload: Option<PayloadRef>,
        disposition: ResultDisposition,
        representation: Representation,
    ) {
        self.declare(
            owner,
            slot,
            TraceObject::Result(ResultOccurrence {
                result_ref: ResultRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec![slot.into()],
                )
                .unwrap(),
                operation: operation.clone(),
                recipient: None,
                disposition,
                representation,
                payload,
                delivery: None,
            }),
        );
    }
    fn raw_payload(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        field: &str,
        subject: AnyObjectRef,
    ) -> Option<PayloadRef> {
        if value.is_null() {
            return None;
        }
        let key = super::super::captured_payload_key(
            self.input.canonical_observation,
            &format!("/payload/{field}"),
        );
        let Some(captured) = self.input.archive.captured_payloads.get(&key) else {
            self.output.requests.push(ArchiveRequest::CapturedPayload {
                parent: self.input.canonical_observation.clone(),
                slot: format!("/payload/{field}"),
            });
            return None;
        };
        let refs: std::collections::BTreeSet<_> = captured
            .bindings
            .iter()
            .filter_map(|binding| binding.attachment.payload_ref.clone())
            .collect();
        if refs.len() != 1 || captured.bytes.is_none() {
            self.runtime_issue(
                owner,
                subject,
                &format!("codex/runtime-payload/{field}"),
                if captured.limited {
                    "Runtime payload exceeds the semantic decode bound"
                } else {
                    "Runtime payload is missing, unreadable or has conflicting retained revisions"
                },
            );
            return None;
        }
        refs.into_iter().next()
    }
    fn raw_payload_value(&self, field: &str) -> Option<Value> {
        let key = super::super::captured_payload_key(
            self.input.canonical_observation,
            &format!("/payload/{field}"),
        );
        self.input
            .archive
            .captured_payloads
            .get(&key)
            .and_then(|payload| payload.bytes.as_ref())
            .and_then(|bytes| serde_json::from_slice(bytes).ok())
    }
    pub(super) fn raw_trace(&mut self, value: &Value) {
        if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
            self.owner_missing("Unsupported Codex raw trace schema version");
            return;
        }
        let Some(incarnation) = string(value, "rollout_id") else {
            return;
        };
        let payload = &value["payload"];
        let kind = string(payload, "type").unwrap_or("");
        let native = string(value, "thread_id")
            .or_else(|| string(payload, "thread_id"))
            .or_else(|| string(payload, "root_thread_id"));
        let Some(owner) = native.and_then(|id| self.agent(id)) else {
            self.owner_missing("Runtime trace has no explicit thread identity");
            return;
        };
        let activation = string(value, "codex_turn_id")
            .or_else(|| string(payload, "codex_turn_id"))
            .map(|id| super::activation(&owner, id));
        let stamp = time(value, "wall_time_unix_ms", TimeUnit::Milliseconds, &owner);
        let mut payload_evidence = Vec::new();
        for (field, reference) in payload
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, value)| value.get("raw_payload_id").is_some())
        {
            let slot = format!("/payload/{field}");
            let key = super::super::captured_payload_key(self.input.canonical_observation, &slot);
            match self.input.archive.captured_payloads.get(&key) {
                None => self.output.requests.push(ArchiveRequest::CapturedPayload {
                    parent: self.input.canonical_observation.clone(),
                    slot,
                }),
                Some(captured) => {
                    if captured.bytes.is_none() {
                        self.output.diagnostics.push(MappingDiagnostic {
                            evidence: vec![self.input.canonical_observation.clone()],
                            code: if captured.limited {
                                DiagnosticCode::ResourceLimit
                            } else {
                                DiagnosticCode::CaptureGap
                            },
                            message: format!(
                                "Runtime payload at {slot} is unavailable or conflicting"
                            ),
                        });
                        if !captured.limited {
                            self.output.blocks_source = true;
                            self.output.requests.push(ArchiveRequest::CapturedPayload {
                                parent: self.input.canonical_observation.clone(),
                                slot,
                            });
                        }
                        return;
                    }
                    for binding in &captured.bindings {
                        payload_evidence.extend(binding.evidence.iter().cloned());
                    }
                    if reference.get("path").and_then(Value::as_str).is_none() {
                        self.issue(
                            &owner,
                            "codex/raw-payload-path",
                            DiagnosticCode::DecodeFailure,
                            "Runtime payload reference lacks its path",
                        );
                        return;
                    }
                }
            }
        }
        if !self.output.requests.is_empty() {
            self.output.blocks_source = true;
            return;
        }
        let first_claim = self.output.claims.len();
        match kind {
            "rollout_started" | "thread_started" => {
                self.announce_agent(&owner, &Value::Null, true);
            }
            "code_cell_started" => {
                let (Some(handle), Some(call)) = (
                    string(payload, "runtime_cell_id"),
                    string(payload, "model_visible_call_id"),
                ) else {
                    self.issue(
                        &owner,
                        "codex/cell-identity",
                        DiagnosticCode::MissingOwner,
                        "Code cell requires its runtime handle and model call id",
                    );
                    return;
                };
                let operation =
                    OperationRef::native(owner.clone(), "codex.model-call", vec![call.into()])
                        .unwrap();
                let resource = ResourceRef::native(
                    owner.clone(),
                    "codex.code-cell",
                    vec![incarnation.into(), handle.into()],
                )
                .unwrap();
                self.declare(
                    &owner,
                    "codex/code-cell",
                    TraceObject::Resource(Resource {
                        resource_ref: resource.clone(),
                        creator: Some(operation.clone()),
                        host: Some(OwnerRef::Agent {
                            agent: owner.clone(),
                        }),
                        kind: ResourceKind::CodeCell {
                            native_handle: handle.into(),
                            incarnation: incarnation.into(),
                        },
                    }),
                );
                self.add(
                    &owner,
                    "codex/cell-use",
                    Fact::Relation {
                        relation: Relation::UsesResource {
                            operation: operation.clone(),
                            resource: resource.clone(),
                        },
                    },
                );
                self.add(
                    &owner,
                    "codex/cell-running",
                    Fact::ResourceState {
                        resource: resource.clone(),
                        state: ResourceState::Running,
                    },
                );
                self.add(
                    &owner,
                    "codex/cell-start",
                    Fact::Execution {
                        execution: ExecutionRef::Resource(resource.clone()),
                        boundary: ExecutionBoundary::Started { time: stamp },
                    },
                );
                let key = self.runtime_key(&owner, incarnation, handle);
                self.state.runtime.cells.insert(key, (resource, operation));
            }
            "code_cell_initial_response" | "code_cell_ended" => {
                let Some(handle) = string(payload, "runtime_cell_id") else {
                    return;
                };
                let key = self.runtime_key(&owner, incarnation, handle);
                let Some((resource, operation)) = self.state.runtime.cells.get(&key).cloned()
                else {
                    self.issue(
                        &owner,
                        "codex/cell-unresolved",
                        DiagnosticCode::MissingSource,
                        "Code-cell response has no creation in its runtime incarnation",
                    );
                    return;
                };
                let status = string(payload, "status").unwrap_or("");
                let disposition = if kind == "code_cell_initial_response"
                    && matches!(status, "starting" | "running" | "yielded")
                {
                    ResultDisposition::Yield
                } else if kind == "code_cell_ended" {
                    ResultDisposition::FinalReturn
                } else {
                    ResultDisposition::Unknown
                };
                let body = self.raw_payload(
                    &owner,
                    &payload["response_payload"],
                    "response_payload",
                    AnyObjectRef::Resource(resource.clone()),
                );
                self.runtime_result(
                    &owner,
                    &operation,
                    "codex/cell-result",
                    body,
                    disposition,
                    // The vendor records RuntimeResponse before formatting the
                    // custom-tool output that its caller will receive.
                    Representation::Runtime,
                );
                let state = match status {
                    "starting" | "running" => ResourceState::Running,
                    "yielded" => ResourceState::Yielded,
                    "completed" => ResourceState::Closed {
                        outcome: TerminalOutcome::Succeeded,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                    "failed" => ResourceState::Closed {
                        outcome: TerminalOutcome::Failed,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                    "terminated" => ResourceState::Closed {
                        outcome: TerminalOutcome::Interrupted,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                    _ => ResourceState::Unknown {
                        reason: "Unknown code-cell runtime status".into(),
                    },
                };
                self.add(
                    &owner,
                    "codex/cell-state",
                    Fact::ResourceState { resource, state },
                );
            }
            "tool_call_started" => {
                let Some(id) = string(payload, "tool_call_id") else {
                    return;
                };
                let reference = OperationRef::native(
                    owner.clone(),
                    "codex.runtime-tool",
                    vec![incarnation.into(), id.into()],
                )
                .unwrap();
                let (requester, enclosing) = if string(&payload["requester"], "type")
                    == Some("code_cell")
                {
                    let cell = string(&payload["requester"], "runtime_cell_id")
                        .and_then(|handle| {
                            self.state.runtime.cells.get(&self.runtime_key(
                                &owner,
                                incarnation,
                                handle,
                            ))
                        })
                        .cloned();
                    match cell {
                        Some((resource, operation)) => {
                            (Some(RequesterRef::Resource(resource)), Some(operation))
                        }
                        None => {
                            self.issue(&owner,"codex/nested-cell-unresolved",DiagnosticCode::MissingSource,"Nested tool's runtime cell has no matching creation in this incarnation");
                            (None, None)
                        }
                    }
                } else {
                    (Some(RequesterRef::Agent(owner.clone())), None)
                };
                let category = match string(&payload["kind"], "type") {
                    Some("exec_command") => OperationCategory::TerminalIo {
                        action: TerminalAction::Launch,
                    },
                    Some("write_stdin") => OperationCategory::TerminalIo {
                        action: TerminalAction::Write,
                    },
                    Some("apply_patch") => OperationCategory::Edit,
                    Some("wait_agent") => OperationCategory::Wait,
                    Some("spawn_agent") => OperationCategory::Delegation,
                    Some("assign_agent_task" | "send_message" | "close_agent") => {
                        OperationCategory::Communication
                    }
                    Some("mcp") => OperationCategory::Tool {
                        namespace: string(&payload["kind"], "server").map(str::to_owned),
                        name: string(&payload["kind"], "tool").unwrap_or("unknown").into(),
                    },
                    _ => OperationCategory::Extension {
                        namespace: "codex.runtime".into(),
                        name: string(&payload["kind"], "name")
                            .or_else(|| string(&payload["kind"], "type"))
                            .unwrap_or("unknown")
                            .into(),
                    },
                };
                let mut input = self.raw_payload(
                    &owner,
                    &payload["invocation_payload"],
                    "invocation_payload",
                    AnyObjectRef::Operation(reference.clone()),
                );
                let invocation = self.raw_payload_value("invocation_payload");
                let category = if let Some(invocation) = &invocation {
                    if let Some(text) = invocation
                        .pointer("/payload/arguments")
                        .or_else(|| invocation.pointer("/payload/input"))
                        .and_then(Value::as_str)
                    {
                        input = Some(
                            self.content(text.as_bytes().to_vec(), "/runtime-invocation/payload"),
                        );
                    }
                    match string(invocation, "tool_namespace") {
                        Some(namespace)
                            if !matches!(namespace, "functions" | "collaboration" | "clock") =>
                        {
                            OperationCategory::Tool {
                                namespace: Some(namespace.into()),
                                name: string(invocation, "tool_name").unwrap_or("unknown").into(),
                            }
                        }
                        _ => category,
                    }
                } else {
                    category
                };
                let category = if let Some(invocation) = &invocation {
                    if string(invocation, "tool_name") == Some("wait")
                        && invocation.get("tool_namespace").is_none_or(Value::is_null)
                    {
                        if let Some(handle) = invocation
                            .pointer("/payload/arguments")
                            .and_then(Value::as_str)
                            .and_then(|text| serde_json::from_str::<Value>(text).ok())
                            .and_then(|value| string(&value, "cell_id").map(str::to_owned))
                        {
                            let key = self.runtime_key(&owner, incarnation, &handle);
                            if let Some((resource, _)) = self.state.runtime.cells.get(&key).cloned()
                            {
                                self.add(
                                    &owner,
                                    "codex/code-wait-resource",
                                    Fact::Relation {
                                        relation: Relation::UsesResource {
                                            operation: reference.clone(),
                                            resource,
                                        },
                                    },
                                );
                                OperationCategory::Wait
                            } else {
                                self.issue(
                                    &owner,
                                    "codex/code-wait-unresolved",
                                    DiagnosticCode::MissingSource,
                                    "Code wait has no matching cell in its runtime incarnation",
                                );
                                category
                            }
                        } else {
                            category
                        }
                    } else {
                        category
                    }
                } else {
                    category
                };
                let operation = Operation {
                    operation_ref: reference.clone(),
                    owner: OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                    activation,
                    requester,
                    category,
                    input_revision: input,
                    argument_state: ArgumentState::Sealed,
                };
                self.declare(
                    &owner,
                    "codex/runtime-tool",
                    TraceObject::Operation(operation.clone()),
                );
                self.add(
                    &owner,
                    "codex/runtime-tool-start",
                    Fact::Execution {
                        execution: ExecutionRef::Operation(reference.clone()),
                        boundary: ExecutionBoundary::Started { time: stamp },
                    },
                );
                if let Some(enclosing) = enclosing {
                    self.add(
                        &owner,
                        "codex/runtime-nesting",
                        Fact::Relation {
                            relation: Relation::Nesting {
                                enclosing,
                                nested: reference.clone(),
                            },
                        },
                    );
                }
                self.state
                    .runtime
                    .operations
                    .insert(self.runtime_key(&owner, incarnation, id), operation);
            }
            "tool_call_runtime_started" => {
                let Some(id) = string(payload, "tool_call_id") else {
                    return;
                };
                let operation = OperationRef::native(
                    owner.clone(),
                    "codex.runtime-tool",
                    vec![incarnation.into(), id.into()],
                )
                .unwrap();
                if let Some(runtime) = self.raw_payload_value("runtime_payload") {
                    self.mapped_runtime_payload(&owner, &operation, &runtime, true, id);
                }
            }
            "tool_call_ended" | "tool_call_runtime_ended" => {
                let Some(id) = string(payload, "tool_call_id") else {
                    return;
                };
                let reference = OperationRef::native(
                    owner.clone(),
                    "codex.runtime-tool",
                    vec![incarnation.into(), id.into()],
                )
                .unwrap();
                let field = if kind == "tool_call_ended" {
                    "result_payload"
                } else {
                    "runtime_payload"
                };
                if kind == "tool_call_runtime_ended" {
                    if let Some(runtime) = self.raw_payload_value(field) {
                        self.mapped_runtime_payload(&owner, &reference, &runtime, false, id);
                    }
                }
                let mut body = self.raw_payload(
                    &owner,
                    &payload[field],
                    field,
                    AnyObjectRef::Operation(reference.clone()),
                );
                if kind == "tool_call_ended" {
                    if let Some(result) = self.raw_payload_value(field) {
                        if let Some(value) =
                            result.get("value").or_else(|| result.get("response_item"))
                        {
                            body = Some(self.opaque(value, "/runtime-result/value"));
                        }
                    }
                }
                self.runtime_result(
                    &owner,
                    &reference,
                    "codex/runtime-tool-return",
                    body,
                    ResultDisposition::FinalReturn,
                    if kind == "tool_call_ended" {
                        Representation::CallerResult
                    } else {
                        Representation::Runtime
                    },
                );
                if kind == "tool_call_ended" {
                    self.close(
                        &owner,
                        ExecutionRef::Operation(reference),
                        runtime_outcome(string(payload, "status")),
                        stamp,
                        None,
                    );
                }
            }
            "codex_turn_started" | "codex_turn_ended" => {
                let Some(reference) = activation else {
                    self.issue(
                        &owner,
                        "codex/runtime-activation",
                        DiagnosticCode::MissingOwner,
                        "Runtime activation boundary has no native turn identity",
                    );
                    return;
                };
                self.declare(
                    &owner,
                    "codex/runtime-activation",
                    TraceObject::Activation(Activation {
                        activation_ref: reference.clone(),
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    }),
                );
                if kind == "codex_turn_started" {
                    self.add(
                        &owner,
                        "codex/runtime-activation-start",
                        Fact::Execution {
                            execution: ExecutionRef::Activation(reference),
                            boundary: ExecutionBoundary::Started { time: stamp },
                        },
                    );
                } else {
                    self.close(
                        &owner,
                        ExecutionRef::Activation(reference),
                        runtime_outcome(string(payload, "status")),
                        stamp,
                        None,
                    );
                }
            }
            "thread_ended" => {
                self.add(
                    &owner,
                    "codex/runtime-thread-closed",
                    Fact::AgentState {
                        agent: owner.clone(),
                        state: AgentState::VendorClosed {
                            reason: string(payload, "status").map(str::to_owned),
                        },
                    },
                );
            }
            "rollout_ended" => {}
            _ => self.issue(
                &owner,
                "codex/runtime-kind",
                DiagnosticCode::UnsupportedKind,
                &format!("Retained raw runtime event {kind:?} has no supported field mapping"),
            ),
        }
        for claim in &mut self.output.claims[first_claim..] {
            claim.evidence.extend(payload_evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
    }

    fn mapped_runtime_payload(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        value: &Value,
        started: bool,
        call_id: &str,
    ) {
        if string(value, "call_id").or_else(|| string(value, "event_id")) != Some(call_id) {
            self.issue(
                owner,
                "codex/runtime-call-mismatch",
                DiagnosticCode::ConflictingOwner,
                "Runtime payload identity differs from its captured tool boundary",
            );
            return;
        }
        // ToolRuntimePayload's custom Serialize implementation writes the bare
        // protocol struct. Its surrounding raw event owns the begin/end boundary.
        // Exec end still contains command, and synchronous exec omits process_id;
        // neither field can distinguish the lifecycle phase. Collaboration payload
        // structs have disjoint required fields, including a nullable new_thread_id.
        let kind = if value.get("command").is_some_and(Value::is_array)
            && string(value, "call_id").is_some()
        {
            Some(if started {
                "exec_command_begin"
            } else {
                "exec_command_end"
            })
        } else if string(value, "sender_thread_id").is_some() {
            if value.get("new_thread_id").is_some()
                || (started && value.get("model").is_some() && value.get("prompt").is_some())
            {
                Some(if started {
                    "collab_agent_spawn_begin"
                } else {
                    "collab_agent_spawn_end"
                })
            } else if value.get("receiver_thread_ids").is_some() || value.get("statuses").is_some()
            {
                Some(if started {
                    "collab_waiting_begin"
                } else {
                    "collab_waiting_end"
                })
            } else if value.get("receiver_thread_id").is_some() && value.get("prompt").is_some() {
                Some(if started {
                    "collab_agent_interaction_begin"
                } else {
                    "collab_agent_interaction_end"
                })
            } else if value.get("receiver_thread_id").is_some() {
                Some(if started {
                    "collab_close_begin"
                } else {
                    "collab_close_end"
                })
            } else {
                None
            }
        } else if value.get("changes").is_some_and(Value::is_object)
            && value
                .get(if started { "auto_approved" } else { "success" })
                .is_some_and(Value::is_boolean)
        {
            Some(if started {
                "patch_apply_begin"
            } else {
                "patch_apply_end"
            })
        } else if value.get("invocation").is_some_and(Value::is_object) {
            Some(if started {
                "mcp_tool_call_begin"
            } else {
                "mcp_tool_call_end"
            })
        } else if string(value, "event_id").is_some() && string(value, "agent_thread_id").is_some()
        {
            Some("sub_agent_activity")
        } else {
            None
        };
        let Some(kind) = kind else {
            self.issue(
                owner,
                "codex/runtime-event-shape",
                DiagnosticCode::DecodeFailure,
                "Retained runtime protocol payload has no recognizable envelope",
            );
            return;
        };
        if matches!(kind, "exec_command_begin" | "exec_command_end") {
            self.core_runtime(owner, kind, value);
            if let Some(id) = string(value, "call_id") {
                let incarnation = format!(
                    "{}:{}",
                    self.input.capture.source_position.source.encode(),
                    self.input.capture.source_position.generation
                );
                let runtime = OperationRef::native(
                    owner.clone(),
                    "codex.exec-runtime",
                    vec![incarnation, id.into()],
                )
                .unwrap();
                self.add(
                    owner,
                    "codex/tool-runtime-nesting",
                    Fact::Relation {
                        relation: Relation::Nesting {
                            enclosing: operation.clone(),
                            nested: runtime,
                        },
                    },
                );
            }
        } else if kind.starts_with("collab_") {
            self.collaboration_event(owner, kind, value);
        } else if kind == "sub_agent_activity" {
            // This snapshot identifies an affected peer, never its activation or
            // spawn parent. The native item decoder enforces the same distinction.
            let mut item = value.clone();
            item["type"] = Value::String("SubAgentActivity".into());
            self.item(owner, &item, false, false, None, &Value::Null);
        } else {
            self.structured_runtime(owner, operation, value, started, kind);
        }
    }

    fn structured_runtime(
        &mut self,
        owner: &AgentRef,
        caller: &OperationRef,
        value: &Value,
        started: bool,
        kind: &str,
    ) {
        let patch = kind.starts_with("patch_");
        let category = if patch {
            OperationCategory::Edit
        } else {
            let (Some(server), Some(tool)) = (
                string(&value["invocation"], "server"),
                string(&value["invocation"], "tool"),
            ) else {
                return;
            };
            OperationCategory::Tool {
                namespace: Some(server.into()),
                name: tool.into(),
            }
        };
        let incarnation = format!(
            "{}:{}",
            self.input.capture.source_position.source.encode(),
            self.input.capture.source_position.generation
        );
        let reference = OperationRef::native(
            owner.clone(),
            if patch {
                "codex.patch-runtime"
            } else {
                "codex.mcp-runtime"
            },
            vec![incarnation, string(value, "call_id").unwrap().into()],
        )
        .unwrap();
        let key = reference.encode();
        let previous = self.state.runtime.operations.get(&key).cloned();
        let definition = previous.unwrap_or_else(|| Operation {
            operation_ref: reference.clone(),
            owner: OwnerRef::Agent {
                agent: owner.clone(),
            },
            activation: string(value, "turn_id").map(|id| super::activation(owner, id)),
            requester: None,
            category,
            input_revision: None,
            argument_state: ArgumentState::Sealed,
        });
        let mut definition = definition;
        if started {
            definition.input_revision = Some(self.opaque(value, "/payload/runtime_payload"));
        }
        self.declare(
            owner,
            "codex/structured-runtime",
            TraceObject::Operation(definition.clone()),
        );
        self.state.runtime.operations.insert(key, definition);
        self.add(
            owner,
            "codex/structured-runtime-nesting",
            Fact::Relation {
                relation: Relation::Nesting {
                    enclosing: caller.clone(),
                    nested: reference.clone(),
                },
            },
        );
        if started {
            self.add(
                owner,
                "codex/structured-runtime-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(reference),
                    boundary: ExecutionBoundary::Started { time: None },
                },
            );
        } else {
            let body = self.opaque(value, "/payload/runtime_payload");
            self.runtime_result(
                owner,
                &reference,
                "codex/structured-runtime-result",
                Some(body),
                ResultDisposition::FinalReturn,
                Representation::Runtime,
            );
            let outcome = if patch {
                if value.get("success").and_then(Value::as_bool) == Some(false) {
                    TerminalOutcome::Failed
                } else {
                    runtime_outcome(string(value, "status"))
                }
            } else if value["result"].get("Err").is_some()
                || value["result"]["Ok"]["isError"].as_bool() == Some(true)
            {
                TerminalOutcome::Failed
            } else if value["result"].get("Ok").is_some() {
                TerminalOutcome::Succeeded
            } else {
                TerminalOutcome::Unknown
            };
            self.close(
                owner,
                ExecutionRef::Operation(reference),
                outcome,
                None,
                None,
            );
        }
    }

    pub(super) fn core_runtime(&mut self, owner: &AgentRef, kind: &str, value: &Value) {
        let Some(id) = string(value, "call_id") else {
            self.issue(
                owner,
                "codex/exec-id",
                DiagnosticCode::MissingOwner,
                "Exec runtime event lacks its own call id",
            );
            return;
        };
        let incarnation = format!(
            "{}:{}",
            self.input.capture.source_position.source.encode(),
            self.input.capture.source_position.generation
        );
        let reference = OperationRef::native(
            owner.clone(),
            "codex.exec-runtime",
            vec![incarnation.clone(), id.into()],
        )
        .unwrap();
        let key = self.runtime_key(owner, &incarnation, id);
        if kind == "exec_command_output_delta" {
            let body = string(value, "chunk")
                .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok());
            if let Some(bytes) = body {
                let payload = derived_payload_with_media(
                    self.input,
                    bytes,
                    "/payload/chunk",
                    "codex/base64-runtime-bytes",
                    "application/octet-stream",
                    PayloadEncoding::Binary,
                );
                let body = payload.reference.clone();
                self.output.payloads.push(payload);
                self.runtime_result(
                    owner,
                    &reference,
                    "codex/exec-output-delta",
                    Some(body),
                    ResultDisposition::Progress,
                    Representation::Runtime,
                );
            } else {
                self.issue(
                    owner,
                    "codex/exec-delta",
                    DiagnosticCode::DecodeFailure,
                    "Exec delta is not valid base64 bytes",
                );
            }
            return;
        }
        let action = if kind == "terminal_interaction"
            || string(value, "source") == Some("unified_exec_interaction")
        {
            if string(value, "stdin")
                .or_else(|| string(value, "interaction_input"))
                .is_some_and(|input| !input.is_empty())
            {
                TerminalAction::Write
            } else {
                TerminalAction::Poll
            }
        } else {
            TerminalAction::Launch
        };
        let reference = if kind == "terminal_interaction" {
            OperationRef::occurrence(
                owner.clone(),
                self.input.capture.source_position.clone(),
                vec!["terminal-interaction".into()],
            )
            .unwrap()
        } else {
            reference
        };
        let existing = (kind != "terminal_interaction")
            .then(|| self.state.runtime.operations.get(&key).cloned())
            .flatten();
        let operation = existing.unwrap_or_else(|| Operation {
            operation_ref: reference.clone(),
            owner: OwnerRef::Agent {
                agent: owner.clone(),
            },
            activation: string(value, "turn_id").map(|id| super::activation(owner, id)),
            requester: None,
            category: OperationCategory::TerminalIo { action },
            input_revision: None,
            argument_state: ArgumentState::Sealed,
        });
        let mut operation = operation;
        operation.operation_ref = reference.clone();
        if operation.input_revision.is_none() {
            operation.input_revision = Some(self.opaque(value, "/payload"));
        }
        self.declare(
            owner,
            "codex/exec-operation",
            TraceObject::Operation(operation.clone()),
        );
        if kind != "terminal_interaction" {
            self.state.runtime.operations.insert(key, operation);
        }
        if let Some(handle) = string(value, "process_id") {
            let handle_key = self.runtime_key(owner, &incarnation, handle);
            let resource = if action == TerminalAction::Launch {
                let resource = ResourceRef::native(
                    owner.clone(),
                    "codex.exec-process",
                    vec![incarnation.clone(), id.into(), handle.into()],
                )
                .unwrap();
                self.declare(
                    owner,
                    "codex/exec-process",
                    TraceObject::Resource(Resource {
                        resource_ref: resource.clone(),
                        creator: Some(reference.clone()),
                        host: Some(OwnerRef::Agent {
                            agent: owner.clone(),
                        }),
                        kind: ResourceKind::Terminal {
                            native_handle: handle.into(),
                            incarnation: reference.encode(),
                        },
                    }),
                );
                self.state
                    .runtime
                    .processes
                    .insert(handle_key.clone(), resource.clone());
                Some(resource)
            } else {
                self.state.runtime.processes.get(&handle_key).cloned()
            };
            if let Some(resource) = resource {
                self.add(
                    owner,
                    "codex/exec-resource-use",
                    Fact::Relation {
                        relation: Relation::UsesResource {
                            operation: reference.clone(),
                            resource: resource.clone(),
                        },
                    },
                );
                if kind == "exec_command_begin" && action == TerminalAction::Launch {
                    self.add(
                        owner,
                        "codex/exec-process-running",
                        Fact::ResourceState {
                            resource: resource.clone(),
                            state: ResourceState::Running,
                        },
                    );
                }
                if kind == "exec_command_end" {
                    let outcome = if value
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0)
                    {
                        TerminalOutcome::Failed
                    } else {
                        runtime_outcome(string(value, "status"))
                    };
                    self.add(
                        owner,
                        "codex/exec-process-closed",
                        Fact::ResourceState {
                            resource,
                            state: ResourceState::Closed {
                                outcome,
                                reason: Some(ExecutionCloseReason::VendorStop),
                            },
                        },
                    );
                }
            } else {
                self.issue(
                    owner,
                    "codex/exec-process-unresolved",
                    DiagnosticCode::MissingSource,
                    "Poll or stdin target has no proved creation in this source incarnation",
                );
            }
        }
        if kind == "exec_command_begin" {
            self.add(
                owner,
                "codex/exec-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(reference),
                    boundary: ExecutionBoundary::Started {
                        time: time(value, "started_at_ms", TimeUnit::Milliseconds, owner),
                    },
                },
            );
        } else if kind == "exec_command_end" {
            for channel in ["stdout", "stderr", "aggregated_output", "formatted_output"] {
                if let Some(text) = string(value, channel) {
                    let body =
                        self.content(text.as_bytes().to_vec(), &format!("/payload/{channel}"));
                    self.runtime_result(
                        owner,
                        &reference,
                        &format!("codex/exec-{channel}"),
                        Some(body),
                        ResultDisposition::FinalReturn,
                        Representation::Runtime,
                    );
                }
            }
            let outcome = if value
                .get("exit_code")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
            {
                TerminalOutcome::Failed
            } else {
                runtime_outcome(string(value, "status"))
            };
            self.close(
                owner,
                ExecutionRef::Operation(reference.clone()),
                outcome,
                time(value, "completed_at_ms", TimeUnit::Milliseconds, owner),
                None,
            );
            if let (Some(seconds), Some(nanos)) = (
                value.pointer("/duration/secs").and_then(Value::as_u64),
                value.pointer("/duration/nanos").and_then(Value::as_u64),
            ) {
                if let Some(ms) = seconds
                    .checked_mul(1000)
                    .and_then(|seconds| seconds.checked_add(nanos / 1_000_000))
                {
                    self.duration(owner, AnyObjectRef::Operation(reference), ms.into());
                }
            }
        }
    }

    pub(super) fn goal_update(&mut self, owner: &AgentRef, value: &Value) {
        let goal = &value["goal"];
        if string(goal, "threadId").is_some_and(|id| self.agent(id).as_ref() != Some(owner)) {
            self.issue(
                owner,
                "codex/goal-owner",
                DiagnosticCode::ConflictingOwner,
                "Goal snapshot belongs to another native thread",
            );
            return;
        }
        self.issue(owner,"codex/goal-instance-unresolved",DiagnosticCode::MissingSource,"Goal state is retained, but ThreadGoal has no instance id; a proved creation operation is required before assigning a Goal resource");
    }
}
fn runtime_outcome(status: Option<&str>) -> TerminalOutcome {
    match status {
        Some("completed" | "succeeded") => TerminalOutcome::Succeeded,
        Some("failed") => TerminalOutcome::Failed,
        Some("cancelled" | "declined" | "aborted") => TerminalOutcome::Cancelled,
        Some("interrupted" | "terminated") => TerminalOutcome::Interrupted,
        _ => TerminalOutcome::Unknown,
    }
}

pub(super) fn map_goal_database(
    input: MapInput<'_>,
    snapshot: sylph_protocol::NativeDatabaseSnapshot,
) -> MapOutput {
    use sylph_protocol::{NativeDatabaseStatus, NativeDatabaseValue};
    let mut output = MapOutput {
        state: input.state.clone(),
        ..Default::default()
    };
    if snapshot.status != NativeDatabaseStatus::Available {
        return output;
    }
    let (Some(NativeDatabaseValue::Text(thread)), Some(NativeDatabaseValue::Text(id))) =
        (snapshot.row.get("thread_id"), snapshot.row.get("goal_id"))
    else {
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code: DiagnosticCode::MissingOwner,
            message: "Goal metadata lacks its actual thread and goal identity".into(),
        });
        return output;
    };
    let Issuer::Environment(environment) = &input.capture.observation_ref.issuer else {
        return output;
    };
    let owner = AgentRef::Session(SessionId::new(
        environment.clone(),
        AgentName::parse("codex").unwrap(),
        NativeId::new(thread),
    ));
    let resource = ResourceRef::native(owner.clone(), "codex.goal", vec![id.clone()]).unwrap();
    output.claims.push(claim(
        &input,
        &owner,
        "codex/goal-resource",
        "codex-1",
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
    ));
    let status = match snapshot.row.get("status") {
        Some(NativeDatabaseValue::Text(status)) => status.clone(),
        _ => "unknown".into(),
    };
    output.claims.push(claim(
        &input,
        &owner,
        "codex/goal-state",
        "codex-1",
        Fact::ResourceState {
            resource: resource.clone(),
            state: ResourceState::GoalUpdated {
                status,
                detail: Some(input.capture.payload_ref.clone()),
            },
        },
    ));
    // The correlation records an observed native identifier, not objective equality.
    output.correlations.insert(
        correlation_key(&owner, "codex.goal-id", std::slice::from_ref(id)),
        AnyObjectRef::Resource(resource),
    );
    output
}

#[cfg(test)]
mod tests;
