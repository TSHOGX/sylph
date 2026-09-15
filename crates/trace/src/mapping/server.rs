//! Interpretation of durable server command facts, independent of vendor dispatch.

use sylph_protocol::trace::*;

use super::{
    claim, correlation_key, derived_payload, ArchiveRequest, MapInput, MapOutput,
    MappingDiagnostic, SemanticMapper,
};

pub struct ServerMapper;

impl SemanticMapper for ServerMapper {
    fn build(&self) -> &'static str {
        "sylph-control-1"
    }

    fn map(&self, input: MapInput<'_>) -> MapOutput {
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let Some(scope) = input.capture.declared_subject_refs.first() else {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::MissingOwner,
                message: "A server control occurrence has no declared target".into(),
            });
            return output;
        };
        let mut add = |slot: &str, fact| {
            output
                .claims
                .push(claim(&input, scope, slot, self.build(), fact))
        };
        if input.capture.declared_subject_refs.len() != 1
            || !matches!(input.capture.observation_ref.issuer, Issuer::Server(_))
            || input.capture.transport_kind != TransportKind::Control
        {
            add("server/invalid-capture", diagnostic(scope, DiagnosticCode::ConflictingOwner,
                "A server control occurrence requires one declared target and a server control source"));
            return output;
        }
        let fact = match serde_json::from_slice::<OperationalFact>(input.raw) {
            Ok(fact) => fact,
            Err(error) => {
                add(
                    "server/decode",
                    diagnostic(scope, DiagnosticCode::DecodeFailure, &error.to_string()),
                );
                return output;
            }
        };
        match fact {
            OperationalFact::Accepted { command } => {
                if &command.target != scope
                    || command.ingestion_authorization_epoch
                        != input.capture.ingestion_authorization_epoch
                {
                    add("server/target-mismatch", diagnostic(scope, DiagnosticCode::ConflictingOwner,
                        "The accepted command target or ingestion epoch differs from its capture"));
                    return output;
                }
                if command.command_id.is_empty()
                    || command.input.revision.is_empty()
                    || command.authorization.subject.is_empty()
                {
                    add("server/invalid-command", diagnostic(scope, DiagnosticCode::DecodeFailure,
                        "The accepted command is missing its command, revision, or author identity"));
                    return output;
                }
                let submitted = derived_payload(
                    &input,
                    command.input.bytes.clone(),
                    "/command/input/bytes",
                    "server/base64-submitted-bytes",
                );
                let payload = submitted.reference.clone();
                output.payloads.push(submitted);
                let correlation = correlation_key(
                    scope,
                    "sylph.command",
                    std::slice::from_ref(&command.command_id),
                );
                if !matches!(
                    command.kind,
                    CommandKind::Send
                        | CommandKind::Steer
                        | CommandKind::Queue
                        | CommandKind::NativeSteer
                        | CommandKind::NativeQueue
                ) {
                    output.correlations.insert(
                        correlation,
                        AnyObjectRef::Operation(operation(scope, &command.command_id)),
                    );
                    add(
                        "server/control-operation",
                        Fact::Declare {
                            object: TraceObject::Operation(Operation {
                                operation_ref: operation(scope, &command.command_id),
                                owner: OwnerRef::Agent {
                                    agent: scope.clone(),
                                },
                                activation: None,
                                requester: None,
                                category: OperationCategory::Extension {
                                    namespace: "sylph.control".into(),
                                    name: match command.kind {
                                        CommandKind::Stop => "stop",
                                        CommandKind::Approval => "approval",
                                        CommandKind::Configure => "configure",
                                        _ => unreachable!(),
                                    }
                                    .into(),
                                },
                                input_revision: Some(payload),
                                argument_state: ArgumentState::Sealed,
                            }),
                        },
                    );
                    return output;
                }
                let message = MessageRef::native(
                    scope.clone(),
                    "sylph.command.message",
                    vec![command.command_id.clone()],
                )
                .unwrap();
                let revision = MessageRevisionRef::native(
                    scope.clone(),
                    "sylph.command.revision",
                    vec![command.command_id.clone(), command.input.revision.clone()],
                )
                .unwrap();
                let author = Author::Human {
                    subject: Some(command.authorization.subject.clone()),
                };
                add(
                    "server/submitted-message",
                    Fact::Declare {
                        object: TraceObject::Message(Message {
                            message_ref: message.clone(),
                            author: author.clone(),
                            protocol_role: None,
                            purpose: MessagePurpose::TaskInput,
                            production: None,
                        }),
                    },
                );
                add(
                    "server/submitted-revision",
                    Fact::Declare {
                        object: TraceObject::MessageRevision(MessageRevision {
                            revision_ref: revision.clone(),
                            message,
                            representation: Representation::Submitted,
                            form: RevisionForm::Complete,
                            parts: vec![ContentPart {
                                part_ref: MessagePartRef::native(
                                    scope.clone(),
                                    "sylph.command.part",
                                    vec![
                                        command.command_id.clone(),
                                        command.input.revision,
                                        "0".into(),
                                    ],
                                )
                                .unwrap(),
                                kind: if command.input.media_type.starts_with("text/") {
                                    ContentKind::Text
                                } else {
                                    ContentKind::Attachment
                                },
                                payload,
                            }],
                        }),
                    },
                );
                let delivery = delivery(scope, &command.command_id);
                output
                    .correlations
                    .insert(correlation, AnyObjectRef::Delivery(delivery.clone()));
                add(
                    "server/requested-delivery",
                    Fact::Declare {
                        object: TraceObject::Delivery(Delivery {
                            delivery_ref: delivery.clone(),
                            sender: author,
                            recipient: Recipient::Agent {
                                agent: OwnerRef::Agent {
                                    agent: scope.clone(),
                                },
                            },
                            message_revision: revision,
                            route: DeliveryRoute::ServerCommand {
                                command_id: command.command_id,
                            },
                        }),
                    },
                );
                add(
                    "server/delivery-requested",
                    Fact::DeliveryStage {
                        delivery: delivery.clone(),
                        stage: DeliveryStage::Requested,
                    },
                );
                if command.kind == CommandKind::Queue {
                    add(
                        "server/server-queue",
                        Fact::DeliveryStage {
                            delivery,
                            stage: DeliveryStage::Queued,
                        },
                    );
                }
            }
            OperationalFact::Dispatched { command_id, .. } => {
                // A transport attempt establishes neither receiver acceptance nor
                // execution. Its exact operational fact remains retained evidence.
                if command_id.is_empty() {
                    add(
                        "server/invalid-dispatch",
                        diagnostic(
                            scope,
                            DiagnosticCode::DecodeFailure,
                            "Dispatch is missing its command identity",
                        ),
                    );
                }
            }
            OperationalFact::Delivery { command_id, fact } => {
                if command_id.is_empty() {
                    add(
                        "server/invalid-delivery",
                        diagnostic(
                            scope,
                            DiagnosticCode::DecodeFailure,
                            "Delivery is missing its command identity",
                        ),
                    );
                    return output;
                }
                let key =
                    correlation_key(scope, "sylph.command", std::slice::from_ref(&command_id));
                let Some(target) = input.archive.correlations.get(&key) else {
                    output.requests.push(ArchiveRequest::Correlation {
                        agent: scope.clone(),
                        namespace: "sylph.command".into(),
                        key: vec![command_id],
                    });
                    return output;
                };
                let AnyObjectRef::Delivery(target_delivery) = target else {
                    if let AnyObjectRef::Operation(operation) = target {
                        if let DeliveryFact::Rejected { receipt } = fact {
                            let body = derived_payload(
                                &input,
                                receipt,
                                "/fact/receipt",
                                "server/base64-vendor-receipt",
                            );
                            add(
                                "server/control-rejected",
                                Fact::Execution {
                                    execution: ExecutionRef::Operation(operation.clone()),
                                    boundary: ExecutionBoundary::Outcome {
                                        outcome: TerminalOutcome::Failed,
                                        reason: None,
                                        detail: Some(body.reference.clone()),
                                    },
                                },
                            );
                            output.payloads.push(body);
                        }
                    } else {
                        add(
                            "server/invalid-correlation",
                            diagnostic(
                                scope,
                                DiagnosticCode::ConflictingIdentity,
                                "The command correlation does not name a delivery or operation",
                            ),
                        );
                    }
                    return output;
                };
                let stage = match fact {
                    DeliveryFact::ReceiverAccepted { .. } => Some(DeliveryStage::Accepted),
                    DeliveryFact::Consumed { revision, .. } if !revision.is_empty() => {
                        Some(DeliveryStage::Admitted { entry: None })
                    }
                    DeliveryFact::Consumed { .. } => {
                        add(
                            "server/invalid-consumption",
                            diagnostic(
                                scope,
                                DiagnosticCode::DecodeFailure,
                                "Consumption must name the immutable submitted revision",
                            ),
                        );
                        None
                    }
                    DeliveryFact::Rejected { receipt } => {
                        let detail = derived_payload(
                            &input,
                            receipt,
                            "/fact/receipt",
                            "server/base64-vendor-receipt",
                        );
                        output.payloads.push(detail);
                        Some(DeliveryStage::Rejected { reason: "The receiver rejected the command; retained receipt supplies the recorded details".into() })
                    }
                    DeliveryFact::Cancelled { reason } => Some(DeliveryStage::Cancelled {
                        reason: Some(reason),
                    }),
                    DeliveryFact::OutcomeUnknown { reason } => {
                        add(
                            "server/delivery-unconfirmed",
                            Fact::Coverage {
                                coverage: Coverage {
                                    subject: AnyObjectRef::Delivery(target_delivery.clone()),
                                    source: input.capture.source_position.source.clone(),
                                    dimension: CoverageDimension::Messages,
                                    interval: CoverageInterval::Object {
                                        object: AnyObjectRef::Delivery(target_delivery.clone()),
                                    },
                                    state: CoverageState::Unavailable { reason },
                                },
                            },
                        );
                        None
                    }
                };
                if let Some(stage) = stage {
                    add(
                        "server/delivery-stage",
                        Fact::DeliveryStage {
                            delivery: target_delivery.clone(),
                            stage,
                        },
                    );
                }
            }
        }
        output
    }
}

fn delivery(scope: &AgentRef, command_id: &str) -> DeliveryRef {
    DeliveryRef::native(
        scope.clone(),
        "sylph.command.delivery",
        vec![command_id.into()],
    )
    .unwrap()
}

fn operation(scope: &AgentRef, command_id: &str) -> OperationRef {
    OperationRef::native(
        scope.clone(),
        "sylph.command.operation",
        vec![command_id.into()],
    )
    .unwrap()
}

fn diagnostic(scope: &AgentRef, code: DiagnosticCode, message: &str) -> Fact {
    Fact::Diagnostic {
        diagnostic: TraceDiagnostic {
            subject: Some(AnyObjectRef::Agent(scope.clone())),
            code,
            message: message.into(),
            related_claims: vec![],
        },
    }
}

#[cfg(test)]
mod tests;
