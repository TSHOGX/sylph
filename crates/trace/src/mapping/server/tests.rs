use super::*;
use crate::mapping::{ArchiveContext, MapperState};
use sylph_protocol::SessionId;

fn agent() -> AgentRef {
    AgentRef::Session(SessionId::decode("env:codex:root").unwrap())
}

fn capture(sequence: u64) -> Capture {
    let issuer = Issuer::Server("server".into());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "control".into(),
            producer_epoch: "epoch".into(),
            producer_seq: sequence,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: "outbox".into(),
            },
            generation: 0,
            locator: SourceLocator::Occurrence {
                key: sequence.to_string(),
            },
        },
        ingestion_authorization_epoch: IngestionEpoch::new("admission").unwrap(),
        declared_subject_refs: vec![agent()],
        captured_at: TimeEvidence {
            domain: "server".into(),
            value: sequence as i64,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::ServerReceipt,
        },
        source_clock: None,
        transport_kind: TransportKind::Control,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}

fn accepted(kind: CommandKind) -> OperationalFact {
    OperationalFact::Accepted {
        command: Box::new(CommandAcceptance {
            command_id: "command-a".into(),
            target: CommandTarget::Agent { agent: agent() },
            kind,
            input: SubmittedRevision {
                revision: "original-v1".into(),
                media_type: "text/plain".into(),
                bytes: b"Human \0\xff\r\n".to_vec(),
            },
            authorization: AuthorizationReceipt {
                subject: "human-a".into(),
                decision_ref: "grant".into(),
                lease_ref: None,
                evaluated_at_ms: 1,
            },
            ingestion_authorization_epoch: IngestionEpoch::new("admission").unwrap(),
            retry_policy: RetryPolicy::AtMostOnce,
            idempotency_key: None,
            accepted_at_ms: 2,
        }),
    }
}

fn run(fact: &OperationalFact, capture: &Capture, archive: &ArchiveContext) -> MapOutput {
    ServerMapper.map(MapInput {
        capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(fact).unwrap(),
        state: &MapperState::default(),
        archive,
    })
}

#[test]
fn acceptance_preserves_human_bytes_without_receiver_or_execution_claims() {
    let output = run(
        &accepted(CommandKind::Send),
        &capture(1),
        &ArchiveContext::default(),
    );
    assert_eq!(output.payloads[0].bytes, b"Human \0\xff\r\n");
    let messages: Vec<_> = output
        .claims
        .iter()
        .filter_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::Message(message),
            } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].author,
        Author::Human {
            subject: Some("human-a".into())
        }
    );
    assert_eq!(messages[0].protocol_role, None);
    assert_eq!(messages[0].purpose, MessagePurpose::TaskInput);
    assert_eq!(
        messages[0].message_ref.address(),
        &ObjectAddress::Native {
            scope: agent(),
            namespace: "sylph.command.message".into(),
            key: vec!["command-a".into()]
        }
    );
    let stages: Vec<_> = output
        .claims
        .iter()
        .filter_map(|claim| match &claim.fact {
            Fact::DeliveryStage { stage, .. } => Some(stage),
            _ => None,
        })
        .collect();
    assert_eq!(stages, [&DeliveryStage::Requested]);
    assert!(!output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution { .. }
            | Fact::Declare {
                object: TraceObject::Activation(_)
            }
    )));
}

#[test]
fn server_queue_is_independent_of_native_receiver_acceptance() {
    let output = run(
        &accepted(CommandKind::Queue),
        &capture(1),
        &ArchiveContext::default(),
    );
    assert!(output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Queued,
            ..
        }
    )));
    assert!(!output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Accepted
                | DeliveryStage::Admitted { .. }
                | DeliveryStage::Included { .. },
            ..
        }
    )));
    let native = run(
        &accepted(CommandKind::NativeQueue),
        &capture(1),
        &ArchiveContext::default(),
    );
    assert!(!native.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Queued,
            ..
        }
    )));
}

#[test]
fn receipt_before_command_requests_retained_correlation_instead_of_fabricating_input() {
    let fact = OperationalFact::Delivery {
        input_revision: "original-v1".into(),
        command_id: "command-a".into(),
        fact: DeliveryFact::Consumed {
            revision: "original-v1".into(),
            receipt: vec![],
        },
    };
    let absent = run(&fact, &capture(2), &ArchiveContext::default());
    assert!(absent.claims.is_empty());
    assert_eq!(
        absent.requests,
        [ArchiveRequest::Correlation {
            agent: agent(),
            namespace: "sylph.command.revision".into(),
            key: vec!["command-a".into(),"original-v1".into()]
        }]
    );
    let accepted = run(
        &accepted(CommandKind::Send),
        &capture(1),
        &ArchiveContext::default(),
    );
    let archive = ArchiveContext {
        correlations: accepted.correlations,
        ..ArchiveContext::default()
    };
    let output = run(&fact, &capture(2), &archive);
    assert!(output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Admitted { entry: None },
            ..
        }
    )));
    assert!(!output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Included { .. },
            ..
        }
    )));
}

#[test]
fn non_message_control_and_its_rejection_do_not_create_human_input() {
    let output = run(
        &accepted(CommandKind::Stop),
        &capture(1),
        &ArchiveContext::default(),
    );
    assert!(!output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Declare {
            object: TraceObject::Message(_) | TraceObject::Delivery(_)
        } | Fact::Execution { .. }
    )));
    let archive = ArchiveContext {
        correlations: output.correlations,
        ..ArchiveContext::default()
    };
    let rejection = run(
        &OperationalFact::Delivery {
            input_revision: "original-v1".into(),
            command_id: "command-a".into(),
            fact: DeliveryFact::Rejected {
                receipt: b"rejected\r\n".to_vec(),
            },
        },
        &capture(2),
        &archive,
    );
    assert_eq!(rejection.payloads[0].bytes, b"rejected\r\n");
    assert!(rejection.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            execution: ExecutionRef::Operation(_),
            boundary: ExecutionBoundary::Outcome {
                outcome: TerminalOutcome::Failed,
                ..
            }
        }
    )));
    assert!(!rejection
        .claims
        .iter()
        .any(|claim| matches!(claim.fact, Fact::DeliveryStage { .. })));
}

#[test]
fn mismatched_target_cannot_attribute_a_submission_to_another_agent() {
    let mut capture = capture(1);
    capture.declared_subject_refs = vec![AgentRef::Session(
        SessionId::decode("env:codex:other").unwrap(),
    )];
    let output = run(
        &accepted(CommandKind::Send),
        &capture,
        &ArchiveContext::default(),
    );
    assert!(output.payloads.is_empty());
    assert!(output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::ConflictingOwner,
                ..
            }
        }
    )));
}

#[test]
fn malformed_and_unscoped_control_are_visible_mapping_diagnostics() {
    let mut capture = capture(1);
    capture.declared_subject_refs.clear();
    let output = run(
        &accepted(CommandKind::Send),
        &capture,
        &ArchiveContext::default(),
    );
    assert_eq!(output.diagnostics[0].code, DiagnosticCode::MissingOwner);
    capture.declared_subject_refs.push(agent());
    let output = ServerMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: b"{broken",
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert!(output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                code: DiagnosticCode::DecodeFailure,
                ..
            }
        }
    )));
}

#[test]
fn reread_receipts_keep_the_original_evidence_and_claim_addresses() {
    let original = capture(1);
    let mut reread = original.clone();
    reread.observation_ref.producer_seq = 200;
    let fact = accepted(CommandKind::Send);
    let raw = serde_json::to_vec(&fact).unwrap();
    let expected = run(&fact, &original, &ArchiveContext::default());
    let actual = ServerMapper.map(MapInput {
        capture: &reread,
        canonical_observation: &original.observation_ref,
        raw: &raw,
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    });
    assert_eq!(actual, expected);
}

#[test]
fn identity_binding_requires_a_proof_that_names_the_actual_agent() {
    let capture = capture(2);
    let observation = ObservationRef {
        issuer: Issuer::Environment(agent().environment().clone()),
        producer_id: "native".into(),
        producer_epoch: "first".into(),
        producer_seq: 1,
    };
    let fact = OperationalFact::IdentityBound {
        command_id: "launch".into(),
        agent_ref: agent(),
        source_evidence: vec![observation.clone()],
    };
    let mut archive = ArchiveContext {
        observations: [observation.clone()].into(),
        observation_sources:[(observation.clone(),SourcePosition {source:SourceRef {issuer:observation.issuer.clone(),instance:"runtime:fixture".into()},generation:0,locator:SourceLocator::Frame {ordinal:1}})].into(),
        ..ArchiveContext::default()
    };
    let unproved = run(&fact, &capture, &archive);
    assert!(unproved.bound_agents.is_empty());
    assert!(unproved.correlations.is_empty());
    archive
        .object_evidence
        .insert(AnyObjectRef::Agent(agent()), vec![observation]);
    assert_eq!(run(&fact, &capture, &archive).bound_agents, vec![agent()]);
}

#[test]
fn queued_edits_keep_the_original_input_and_dispatch_correlates_the_exact_revision() {
    let first=run(&accepted(CommandKind::Queue),&capture(1),&ArchiveContext::default());
    let revised=OperationalFact::Revised {command_id:"command-a".into(),previous_revision:"original-v1".into(),input:SubmittedRevision {revision:"edited-v2".into(),media_type:"text/plain".into(),bytes:b"edited request".to_vec()},authorization:AuthorizationReceipt {subject:"human-a".into(),decision_ref:"grant-2".into(),lease_ref:None,evaluated_at_ms:2}};
    let second=run(&revised,&capture(2),&ArchiveContext::default());
    assert!(second.claims.iter().any(|claim|matches!(&claim.fact,Fact::DeliveryStage {stage:DeliveryStage::Cancelled {..},delivery} if matches!(delivery.address(),ObjectAddress::Native {key,..} if key==&vec!["command-a".to_owned(),"original-v1".to_owned()]))));
    let mut archive=ArchiveContext {correlations:first.correlations.clone(),..ArchiveContext::default()};archive.correlations.extend(second.correlations.clone());
    let delivered=run(&OperationalFact::Delivery {command_id:"command-a".into(),input_revision:"edited-v2".into(),fact:DeliveryFact::Consumed {revision:"edited-v2".into(),receipt:b"{}".to_vec()}},&capture(3),&archive);
    assert!(delivered.claims.iter().any(|claim|matches!(&claim.fact,Fact::DeliveryStage {delivery,stage:DeliveryStage::Admitted {..}} if matches!(delivery.address(),ObjectAddress::Native {key,..} if key==&vec!["command-a".to_owned(),"edited-v2".to_owned()]))));
    assert_eq!(first.payloads[0].bytes,b"Human \0\xff\r\n");assert_eq!(second.payloads[0].bytes,b"edited request");
}

#[test]
fn submitted_multipart_input_preserves_text_and_decoded_attachment_bytes() {
    let mut fact=accepted(CommandKind::Send);let OperationalFact::Accepted {command}=&mut fact else {unreachable!()};
    command.input.media_type="application/vnd.sylph.input+json".into();command.input.bytes=serde_json::to_vec(&serde_json::json!({"text":"keep\r\n你好","attachments":[{"name":"pixel.png","type":"image/png","data":"AAEC/w=="}]})).unwrap();
    let output=run(&fact,&capture(1),&ArchiveContext::default());let revision=output.claims.iter().find_map(|claim|match &claim.fact {Fact::Declare {object:TraceObject::MessageRevision(value)}=>Some(value),_=>None}).unwrap();assert_eq!(revision.parts.len(),2);assert_eq!(revision.parts[0].kind,ContentKind::Text);assert_eq!(revision.parts[1].kind,ContentKind::Image);assert_eq!(revision.parts[1].media_type.as_deref(),Some("image/png"));
    assert_eq!(output.payloads.iter().find(|payload|payload.reference==revision.parts[0].payload).unwrap().bytes,"keep\r\n你好".as_bytes());assert_eq!(output.payloads.iter().find(|payload|payload.reference==revision.parts[1].payload).unwrap().bytes,[0,1,2,255]);
}
