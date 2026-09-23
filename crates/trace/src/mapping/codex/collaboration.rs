use super::*;
impl Context<'_, '_> {
    pub(super) fn collaboration_event(&mut self, containing: &AgentRef, kind: &str, value: &Value) {
        let Some(owner) = string(value, "sender_thread_id").and_then(|id| self.agent(id)) else {
            self.issue(
                containing,
                "codex/collaboration-sender",
                DiagnosticCode::MissingOwner,
                "Collaboration event omits the actual sending thread",
            );
            return;
        };
        let Some(id) = string(value, "call_id") else {
            self.issue(
                &owner,
                "codex/collaboration-call",
                DiagnosticCode::MissingOwner,
                "Collaboration event omits its native call id",
            );
            return;
        };
        let operation = OperationRef::native(
            owner.clone(),
            "codex.collaboration-runtime",
            vec![id.into()],
        )
        .unwrap();
        let start = kind.ends_with("_begin");
        let category = if kind.contains("spawn") {
            OperationCategory::Delegation
        } else if kind.contains("waiting") {
            OperationCategory::Wait
        } else {
            OperationCategory::Communication
        };
        let key = operation.encode();
        let previous = self.state.runtime.operations.get(&key).cloned();
        let definition = previous.unwrap_or_else(|| Operation {
            operation_ref: operation.clone(),
            owner: OwnerRef::Agent {
                agent: owner.clone(),
            },
            activation: None,
            requester: None,
            category,
            input_revision: None,
            argument_state: ArgumentState::Sealed,
        });
        self.declare(
            &owner,
            "codex/collaboration-operation",
            TraceObject::Operation(definition.clone()),
        );
        self.state.runtime.operations.insert(key, definition);
        if start {
            self.add(
                &owner,
                "codex/collaboration-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation.clone()),
                    boundary: ExecutionBoundary::Started {
                        time: time(value, "started_at_ms", TimeUnit::Milliseconds, &owner),
                    },
                },
            );
        } else {
            let payload = self.opaque(value, "/payload");
            self.declare(
                &owner,
                "codex/collaboration-result",
                TraceObject::Result(ResultOccurrence {
                    result_ref: ResultRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec!["collaboration-result".into()],
                    )
                    .unwrap(),
                    operation: operation.clone(),
                    recipient: Some(Recipient::Agent {
                        agent: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    }),
                    disposition: ResultDisposition::FinalReturn,
                    representation: ResultRepresentation::Runtime,
                    payload: Some(payload),
                    delivery: None,
                }),
            );
            // AgentStatus reports the target's observed state, not this operation's
            // success or an identified child activation outcome.
            self.add(
                &owner,
                "codex/collaboration-return",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation.clone()),
                    boundary: ExecutionBoundary::Terminated {
                        time: time(value, "completed_at_ms", TimeUnit::Milliseconds, &owner),
                        reason: Some(ExecutionCloseReason::VendorStop),
                    },
                },
            );
        }
        if kind == "collab_agent_spawn_end" {
            if let Some(child) = string(value, "new_thread_id").and_then(|id| self.agent(id)) {
                self.declare(
                    &owner,
                    "codex/spawn-child",
                    TraceObject::Agent(AgentThread {
                        agent_ref: child.clone(),
                        vendor: AgentName::parse("codex").unwrap(),
                        name: string(value, "new_agent_nickname").map(str::to_owned),
                        model: string(value, "model").map(str::to_owned),
                    }),
                );
                self.add(
                    &owner,
                    "codex/spawn-relation",
                    Fact::Relation {
                        relation: Relation::Spawn {
                            parent: owner.clone(),
                            child,
                            operation: Some(operation.clone()),
                        },
                    },
                );
                self.add(
                    &owner,
                    "codex/spawn-success",
                    Fact::Execution {
                        execution: ExecutionRef::Operation(operation),
                        boundary: ExecutionBoundary::Outcome {
                            outcome: TerminalOutcome::Succeeded,
                            reason: None,
                            detail: None,
                        },
                    },
                );
            } else {
                self.add(
                    &owner,
                    "codex/spawn-failed",
                    Fact::Execution {
                        execution: ExecutionRef::Operation(operation),
                        boundary: ExecutionBoundary::Outcome {
                            outcome: TerminalOutcome::Failed,
                            reason: None,
                            detail: None,
                        },
                    },
                );
            }
        } else {
            let targets: Vec<_> = string(value, "receiver_thread_id")
                .into_iter()
                .chain(
                    value
                        .get("receiver_thread_ids")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
                .chain(
                    value
                        .get("statuses")
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|values| values.keys().map(String::as_str)),
                )
                .collect();
            for (index, target) in targets.into_iter().enumerate() {
                if let Some(recipient) = self.agent(target) {
                    self.add(
                        &owner,
                        &format!("codex/collaboration-target/{index}"),
                        Fact::Relation {
                            relation: Relation::Interaction {
                                sender: owner.clone(),
                                recipient,
                                delivery: None,
                                operation: Some(operation.clone()),
                            },
                        },
                    );
                }
            }
        }
    }
}

impl Context<'_, '_> {
    pub(super) fn collaboration_item(
        &mut self,
        containing: &AgentRef,
        item: &Value,
        live: bool,
        started: bool,
        activation: Option<ActivationRef>,
    ) {
        let field = |native, wire| if live { wire } else { native };
        let Some(owner) =
            string(item, field("sender_thread_id", "senderThreadId")).and_then(|id| self.agent(id))
        else {
            self.issue(
                containing,
                "codex/collab-item-owner",
                DiagnosticCode::MissingOwner,
                "Collaboration item lacks its actual sender thread",
            );
            return;
        };
        let Some(id) = string(item, "id") else {
            return;
        };
        let tool = string(item, "tool").unwrap_or("");
        // The app-server enum uses camelCase; native TurnItem serialization uses
        // snake_case. These are two vendor contracts, not interchangeable aliases.
        let category = match (live, tool) {
            (true, "spawnAgent") | (false, "spawn_agent") => OperationCategory::Delegation,
            (_, "wait") => OperationCategory::Wait,
            (
                true,
                "sendInput" | "sendMessage" | "followupTask" | "interruptAgent" | "closeAgent"
                | "resumeAgent",
            )
            | (
                false,
                "send_input" | "send_message" | "followup_task" | "interrupt_agent" | "close_agent"
                | "resume_agent",
            ) => OperationCategory::Communication,
            _ => OperationCategory::Extension {
                namespace: "codex.collaboration".into(),
                name: tool.into(),
            },
        };
        let spawns = matches!(category, OperationCategory::Delegation);
        let operation =
            OperationRef::native(owner.clone(), "codex.collaboration-item", vec![id.into()])
                .unwrap();
        let input = string(item, "prompt").map(|text| {
            self.content(
                text.as_bytes().to_vec(),
                if live {
                    "/params/item/prompt"
                } else {
                    "/payload/item/prompt"
                },
            )
        });
        self.declare(
            &owner,
            "codex/collab-item-operation",
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation,
                requester: None,
                category,
                input_revision: input,
                argument_state: ArgumentState::Sealed,
            }),
        );
        if started {
            self.add(
                &owner,
                "codex/collab-item-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation.clone()),
                    boundary: ExecutionBoundary::Started { time: None },
                },
            );
        } else {
            let result = self.opaque(
                item,
                if live {
                    "/params/item"
                } else {
                    "/payload/item"
                },
            );
            self.declare(
                &owner,
                "codex/collab-item-result",
                TraceObject::Result(ResultOccurrence {
                    result_ref: ResultRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec!["collaboration-item".into()],
                    )
                    .unwrap(),
                    operation: operation.clone(),
                    recipient: Some(Recipient::Agent {
                        agent: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    }),
                    disposition: ResultDisposition::FinalReturn,
                    representation: ResultRepresentation::Runtime,
                    payload: Some(result),
                    delivery: None,
                }),
            );
            let outcome = match string(item, "status") {
                Some("completed") => TerminalOutcome::Succeeded,
                Some("failed") => TerminalOutcome::Failed,
                Some("interrupted") => TerminalOutcome::Interrupted,
                _ => TerminalOutcome::Unknown,
            };
            self.close(
                &owner,
                ExecutionRef::Operation(operation.clone()),
                outcome,
                None,
                None,
            );
        }
        if let Some(targets) = item
            .get(field("receiver_thread_ids", "receiverThreadIds"))
            .and_then(Value::as_array)
        {
            for (index, target) in targets.iter().filter_map(Value::as_str).enumerate() {
                let Some(child) = self.agent(target) else {
                    continue;
                };
                let relation = if spawns && !started && string(item, "status") == Some("completed")
                {
                    self.declare(
                        &owner,
                        &format!("codex/collab-child/{index}"),
                        TraceObject::Agent(AgentThread {
                            agent_ref: child.clone(),
                            vendor: AgentName::parse("codex").unwrap(),
                            name: None,
                            model: string(item, "model").map(str::to_owned),
                        }),
                    );
                    Relation::Spawn {
                        parent: owner.clone(),
                        child,
                        operation: Some(operation.clone()),
                    }
                } else {
                    Relation::Interaction {
                        sender: owner.clone(),
                        recipient: child,
                        delivery: None,
                        operation: Some(operation.clone()),
                    }
                };
                self.add(
                    &owner,
                    &format!("codex/collab-item-target/{index}"),
                    Fact::Relation { relation },
                );
            }
        }
        if !started {
            self.history(&owner, vec![HistoryContent::Operation(operation)], None);
        }
    }
}
