//! Interpretation of durable server command facts, independent of vendor dispatch.

use super::derived_payload_with_media;
use sylph_protocol::trace::*;

use super::{
    claim, command_identity_key, correlation_key, derived_payload, ArchiveRequest, MapInput,
    MapOutput, MappingDiagnostic, SemanticMapper,
};

pub struct ServerMapper;

fn source_issue(input: &MapInput<'_>, output: &mut MapOutput, code: DiagnosticCode, message: &str) {
    if code == DiagnosticCode::MissingOwner {
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code,
            message: message.into(),
        });
        return;
    }
    if let Some(scope) = input.capture.declared_subject_refs.first() {
        output.claims.push(claim(
            input,
            scope,
            "server/source-diagnostic",
            "sylph-control-2",
            diagnostic(scope, code, message),
        ));
    } else {
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code,
            message: message.into(),
        });
    }
}

fn control_scope(
    input: &MapInput<'_>,
    fact: &OperationalFact,
    output: &mut MapOutput,
) -> Option<AgentRef> {
    if let OperationalFact::IdentityBound { agent_ref, .. } = fact {
        return Some(agent_ref.clone());
    }
    let command_id = match fact {
        OperationalFact::Accepted { command } => &command.command_id,
        OperationalFact::Dispatched { command_id, .. }
        | OperationalFact::Delivery { command_id, .. }
        | OperationalFact::Revised {command_id,..} => command_id,
        OperationalFact::IdentityBound { .. } => unreachable!(),
    };
    if let OperationalFact::Accepted { command } = fact {
        if let CommandTarget::Launch { inventory } = &command.target {
            let key = command_identity_key(&input.capture.observation_ref.issuer, command_id);
            if let Some(AnyObjectRef::Agent(agent)) = input.archive.correlations.get(&key) {
                if agent.environment() != &inventory.environment
                    || agent.anchor_session().agent != inventory.vendor
                {
                    source_issue(
                        input,
                        output,
                        DiagnosticCode::ConflictingOwner,
                        "Launch identity differs from its authorized inventory",
                    );
                    return None;
                }
                return Some(agent.clone());
            }
            output.requests.push(ArchiveRequest::CommandIdentity {
                issuer: input.capture.observation_ref.issuer.clone(),
                command_id: command_id.clone(),
            });
            source_issue(
                input,
                output,
                DiagnosticCode::MissingOwner,
                "Accepted launch input awaits evidenced native identity",
            );
            return None;
        }
    }
    if let Some(scope) = input.capture.declared_subject_refs.first() {
        return Some(scope.clone());
    }
    let key = command_identity_key(&input.capture.observation_ref.issuer, command_id);
    if let Some(AnyObjectRef::Agent(agent)) = input.archive.correlations.get(&key) {
        return Some(agent.clone());
    }
    if !matches!(fact, OperationalFact::Accepted { .. }) {
        output.requests.push(ArchiveRequest::CommandIdentity {
            issuer: input.capture.observation_ref.issuer.clone(),
            command_id: command_id.clone(),
        });
    }
    source_issue(
        input,
        output,
        DiagnosticCode::MissingOwner,
        "A server control occurrence has no evidenced target",
    );
    None
}

impl SemanticMapper for ServerMapper {
    fn build(&self) -> &'static str {
        "sylph-control-2"
    }

    fn map(&self, input: MapInput<'_>) -> MapOutput {
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let fact = match serde_json::from_slice::<OperationalFact>(input.raw) {
            Ok(fact) => fact,
            Err(error) => {
                source_issue(
                    &input,
                    &mut output,
                    DiagnosticCode::DecodeFailure,
                    &error.to_string(),
                );
                return output;
            }
        };
        if input.capture.declared_subject_refs.len() > 1
            || !matches!(input.capture.observation_ref.issuer, Issuer::Server(_))
            || input.capture.transport_kind != TransportKind::Control
        {
            source_issue(
                &input,
                &mut output,
                DiagnosticCode::ConflictingOwner,
                "Control evidence requires one known target at most and a server control source",
            );
            return output;
        }
        let Some(resolved_scope) = control_scope(&input, &fact, &mut output) else {
            return output;
        };
        let scope = &resolved_scope;
        let scope_evidence = match &fact {
            OperationalFact::Accepted { command }
                if matches!(command.target, CommandTarget::Launch { .. }) =>
            {
                input
                    .archive
                    .correlation_evidence
                    .get(&command_identity_key(
                        &input.capture.observation_ref.issuer,
                        &command.command_id,
                    ))
                    .cloned()
                    .unwrap_or_default()
            }
            _ => vec![],
        };
        let mut add = |slot: &str, fact| {
            let mut value = claim(&input, scope, slot, self.build(), fact);
            value.evidence.extend(scope_evidence.iter().cloned());
            output.claims.push(value)
        };
        match fact {
            OperationalFact::IdentityBound {
                command_id,
                agent_ref,
                source_evidence,
            } => {
                if source_evidence.is_empty() || !matches!(&agent_ref, AgentRef::Session(_))
                    || source_evidence.iter().any(|reference| !matches!(&reference.issuer, Issuer::Environment(environment) if environment == agent_ref.environment())) {
                    source_issue(&input, &mut output, DiagnosticCode::ConflictingIdentity, "Identity binding requires native environment evidence");
                    return output;
                }
                let missing: Vec<_> = source_evidence
                    .iter()
                    .filter(|reference| !input.archive.observations.contains(*reference)||!input.archive.observation_sources.contains_key(*reference))
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    output.requests.extend(
                        missing
                            .into_iter()
                            .map(|observation| ArchiveRequest::Observation { observation }),
                    );
                    return output;
                }
                let subject = AnyObjectRef::Agent(agent_ref.clone());
                if !input
                    .archive
                    .semantic.object_evidence
                    .get(&subject)
                    .is_some_and(|proof| {
                        source_evidence
                            .iter()
                            .any(|observation| proof.contains(observation))
                    })
                {
                    output.requests.push(ArchiveRequest::Object {
                        object_ref: subject,
                    });
                    source_issue(
                        &input,
                        &mut output,
                        DiagnosticCode::MissingOwner,
                        "The retained native observations do not yet prove this agent identity",
                    );
                    return output;
                }
                let key = command_identity_key(&input.capture.observation_ref.issuer, &command_id);
                output.bound_agents.push(agent_ref.clone());
                for observation in &source_evidence {if let Some(position)=input.archive.observation_sources.get(observation) {
                    output.source_owners.push(super::SourceOwnership {source:position.source.clone(),generation:position.generation,owner:super::SourceOwner {agent:agent_ref.clone(),evidence:vec![observation.clone(),input.canonical_observation.clone()],rule:"server/native-identity-binding".into()}});
                }}
                output
                    .correlations
                    .insert(key.clone(), AnyObjectRef::Agent(agent_ref));
                output.correlation_evidence.insert(
                    key,
                    source_evidence
                        .into_iter()
                        .chain(std::iter::once(input.canonical_observation.clone()))
                        .collect(),
                );
            }
            OperationalFact::Accepted { command } => {
                if command.target.agent().is_some_and(|target| target != scope)
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
                let submitted_encoding = if std::str::from_utf8(&command.input.bytes).is_ok() {
                    PayloadEncoding::Utf8
                } else {
                    PayloadEncoding::Binary
                };
                let submitted = derived_payload_with_media(
                    &input,
                    command.input.bytes.clone(),
                    "/command/input/bytes",
                    "server/base64-submitted-bytes",
                    command.input.media_type.clone(),
                    submitted_encoding,
                );
                let payload = submitted.reference.clone();
                output.payloads.push(submitted);
                let correlation = correlation_key(
                    scope,
                    "sylph.command.revision",
                    &[command.command_id.clone(),command.input.revision.clone()],
                );
                output.correlation_evidence.insert(
                    correlation.clone(),
                    vec![input.canonical_observation.clone()],
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
                            parts: match submitted_parts(&input,scope,&command.command_id,&command.input,&mut output.payloads) {
                                Ok(parts)=>parts,
                                Err(reason)=>{output.diagnostics.push(MappingDiagnostic {evidence:vec![input.canonical_observation.clone()],code:DiagnosticCode::DecodeFailure,message:reason});return output;}
                            },
                        }),
                    },
                );
                let delivery = delivery(scope, &command.command_id,&command.input.revision);
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
                            route: DeliveryRoute::ServerCommand {
                                command_id: command.command_id,
                            },
                        }),
                    },
                );
                add(
                    "server/requested-delivery-revision",
                    Fact::DeliveryRevisionObserved {
                        delivery: delivery.clone(),
                        revision,
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
            OperationalFact::Revised {command_id,previous_revision,input:revision,authorization}=>{
                let author=Author::Human {subject:Some(authorization.subject)};
                let message=MessageRef::native(scope.clone(),"sylph.command.message",vec![command_id.clone()]).unwrap();
                let revision_ref=MessageRevisionRef::native(scope.clone(),"sylph.command.revision",vec![command_id.clone(),revision.revision.clone()]).unwrap();
                let parts=submitted_parts(&input,scope,&command_id,&revision,&mut output.payloads);
                let parts=match parts {Ok(parts)=>parts,Err(reason)=>{add("server/invalid-revision",diagnostic(scope,DiagnosticCode::DecodeFailure,&reason));return output;}};
                add("server/submitted-message",Fact::Declare {object:TraceObject::Message(Message {message_ref:message.clone(),author:author.clone(),protocol_role:None,purpose:MessagePurpose::TaskInput,production:None})});
                add("server/submitted-revision",Fact::Declare {object:TraceObject::MessageRevision(MessageRevision {revision_ref:revision_ref.clone(),message,representation:Representation::Submitted,form:RevisionForm::Correction {replaces:MessageRevisionRef::native(scope.clone(),"sylph.command.revision",vec![command_id.clone(),previous_revision.clone()]).unwrap()},parts})});
                let delivery=delivery(scope,&command_id,&revision.revision);
                add("server/previous-queue-cancelled",Fact::DeliveryStage {delivery:self::delivery(scope,&command_id,&previous_revision),stage:DeliveryStage::Cancelled {reason:Some("Queued input was revised before dispatch".into())}});
                add("server/revised-delivery",Fact::Declare {object:TraceObject::Delivery(Delivery {delivery_ref:delivery.clone(),sender:author,recipient:Recipient::Agent {agent:OwnerRef::Agent {agent:scope.clone()}},route:DeliveryRoute::ServerCommand {command_id:command_id.clone()}})});
                add("server/revised-delivery-revision",Fact::DeliveryRevisionObserved {delivery:delivery.clone(),revision:revision_ref});
                add("server/revised-queue",Fact::DeliveryStage {delivery:delivery.clone(),stage:DeliveryStage::Queued});
                let key=correlation_key(scope,"sylph.command.revision",&[command_id,revision.revision]);
                output.correlations.insert(key.clone(),AnyObjectRef::Delivery(delivery));output.correlation_evidence.insert(key,vec![input.canonical_observation.clone()]);
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
            OperationalFact::Delivery { command_id, input_revision, fact } => {
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
                    correlation_key(scope, "sylph.command.revision", &[command_id.clone(),input_revision.clone()]);
                let Some(target) = input.archive.correlations.get(&key) else {
                    output.requests.push(ArchiveRequest::Correlation {
                        agent: scope.clone(),
                        namespace: "sylph.command.revision".into(),
                        key: vec![command_id,input_revision],
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

fn delivery(scope: &AgentRef, command_id: &str,input_revision:&str) -> DeliveryRef {
    DeliveryRef::native(
        scope.clone(),
        "sylph.command.delivery",
        vec![command_id.into(),input_revision.into()],
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

fn submitted_parts(input:&MapInput<'_>,scope:&AgentRef,command_id:&str,revision:&SubmittedRevision,payloads:&mut Vec<super::DerivedPayload>)->Result<Vec<ContentPart>,String> {
    use base64::Engine;
    let mut bodies=Vec::new();
    if revision.media_type=="application/vnd.sylph.input+json" {
        let value:serde_json::Value=serde_json::from_slice(&revision.bytes).map_err(|error|error.to_string())?;
        if let Some(text)=value.get("text").and_then(serde_json::Value::as_str) {bodies.push((text.as_bytes().to_vec(),"text/plain".to_owned(),ContentKind::Text,PayloadEncoding::Utf8,"/text".to_owned()));}
        let attachments=value.get("attachments").and_then(serde_json::Value::as_array).ok_or_else(||"Multipart input omits attachments array".to_owned())?;
        for (index,attachment) in attachments.iter().enumerate() {
            let mime=attachment.get("type").and_then(serde_json::Value::as_str).ok_or_else(||"Attachment omits its MIME type".to_owned())?;
            let data=attachment.get("data").and_then(serde_json::Value::as_str).ok_or_else(||"Attachment omits bytes".to_owned())?;
            let bytes=base64::engine::general_purpose::STANDARD.decode(data).map_err(|error|error.to_string())?;
            let kind=if mime.starts_with("image/"){ContentKind::Image}else if mime.starts_with("audio/"){ContentKind::Audio}else if mime.starts_with("video/"){ContentKind::Video}else{ContentKind::Attachment};
            bodies.push((bytes,mime.into(),kind,PayloadEncoding::Binary,format!("/attachments/{index}/data")));
        }
    } else {
        let encoding=if std::str::from_utf8(&revision.bytes).is_ok(){PayloadEncoding::Utf8}else{PayloadEncoding::Binary};
        bodies.push((revision.bytes.clone(),revision.media_type.clone(),if revision.media_type.starts_with("text/"){ContentKind::Text}else{ContentKind::Attachment},encoding,"/command/input/bytes".into()));
    }
    let mut parts=Vec::new();
    for (index,(bytes,mime,kind,encoding,selector)) in bodies.into_iter().enumerate() {
        let payload=derived_payload_with_media(input,bytes,selector,"server/base64-submitted-bytes",mime.clone(),encoding);
        parts.push(ContentPart {part_ref:MessagePartRef::native(scope.clone(),"sylph.command.part",vec![command_id.into(),revision.revision.clone(),index.to_string()]).map_err(|error|error.to_string())?,kind,payload:payload.reference.clone(),media_type:Some(mime),encoding});payloads.push(payload);
    }
    Ok(parts)
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
