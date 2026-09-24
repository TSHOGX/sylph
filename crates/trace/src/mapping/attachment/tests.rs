use super::*;
use crate::mapping::{ArchiveContext, MapperState};

#[test]
fn a_late_sidecar_keeps_the_caller_result_and_requires_its_capture_owned_bytes() {
    let agent = AgentRef::Session(sylph_protocol::SessionId::decode("env:claude:root").unwrap());
    let issuer = Issuer::Environment(agent.environment().clone());
    let parent = ObservationRef {
        issuer: issuer.clone(),
        producer_id: "capture".into(),
        producer_epoch: "one".into(),
        producer_seq: 1,
    };
    let source = SourceRef {
        issuer: issuer.clone(),
        instance: "sidecar-events".into(),
    };
    let position = SourcePosition {
        source: source.clone(),
        generation: 0,
        locator: SourceLocator::Frame { ordinal: 2 },
    };
    let attachment = CaptureAttachment {
        locator: "/output/body".into(),
        source_position: Some(SourcePosition {
            source: SourceRef {
                instance: "sidecar-body".into(),
                ..source
            },
            generation: 0,
            locator: SourceLocator::Snapshot {
                revision: "one".into(),
            },
        }),
        observed_version: Some("one".into()),
        payload_ref: Some(PayloadRef::bytes("b".repeat(64)).unwrap()),
        availability: AttachmentAvailability::Captured,
        relation: "tool_output".into(),
    };
    let event = SourceAttachmentEvent {
        parent_observation: parent.clone(),
        parent_slot: "/message/content/0".into(),
        attachment: attachment.clone(),
    };
    let mut capture = Capture {
        observation_ref: ObservationRef {
            producer_seq: 2,
            ..parent.clone()
        },
        source_position: position,
        ingestion_authorization_epoch: IngestionEpoch::new("epoch").unwrap(),
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "capture".into(),
            value: 2,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: TransportKind::SourceAttachment,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![attachment],
        capture_diagnostics: vec![],
    };
    let operation = OperationRef::native(agent, "claude.tool", vec!["call-1".into()]).unwrap();
    let archive = ArchiveContext {
        observations: [parent.clone()].into_iter().collect(),
        correlations: [(
            crate::mapping::attachment_key(&parent, &event.parent_slot, &event.attachment.locator),
            AnyObjectRef::Operation(operation.clone()),
        )]
        .into_iter()
        .collect(),
        ..ArchiveContext::default()
    };
    let raw = serde_json::to_vec(&event).unwrap();
    let run = |capture: &Capture| {
        AttachmentMapper.map(MapInput {
            capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &MapperState::default(),
            archive: &archive,
        })
    };
    let output = run(&capture);
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Declare {object:TraceObject::Result(result)} if result.operation==operation&&result.representation==ResultRepresentation::Runtime&&result.delivery.is_none())));
    assert!(output.claims.iter().all(|claim| !matches!(
        claim.fact,
        Fact::Execution { .. } | Fact::DeliveryStage { .. }
    )));
    capture.attachments.clear();
    let unowned = run(&capture);
    assert!(unowned.claims.is_empty());
    assert_eq!(unowned.diagnostics[0].code, DiagnosticCode::CaptureGap);
}

#[test]
fn one_sidecar_snapshot_can_supply_distinct_results_to_two_operations() {
    let agent = AgentRef::Session(sylph_protocol::SessionId::decode("env:claude:root").unwrap());
    let issuer = Issuer::Environment(agent.environment().clone());
    let body_position = SourcePosition {
        source: SourceRef {
            issuer: issuer.clone(),
            instance: "shared-output-file".into(),
        },
        generation: 0,
        locator: SourceLocator::Snapshot {
            revision: "same-bytes".into(),
        },
    };
    let payload = PayloadRef::bytes("b".repeat(64)).unwrap();
    let mut outputs = vec![];
    for (seq, call) in [(1, "first-call"), (2, "second-call")] {
        let parent = ObservationRef {
            issuer: issuer.clone(),
            producer_id: "parent".into(),
            producer_epoch: "one".into(),
            producer_seq: seq,
        };
        let attachment = CaptureAttachment {
            locator: "/shared/output".into(),
            source_position: Some(body_position.clone()),
            observed_version: Some("same-bytes".into()),
            payload_ref: Some(payload.clone()),
            availability: AttachmentAvailability::Captured,
            relation: "tool_output".into(),
        };
        let event = SourceAttachmentEvent {
            parent_observation: parent.clone(),
            parent_slot: "/message/content/0".into(),
            attachment: attachment.clone(),
        };
        let operation =
            OperationRef::native(agent.clone(), "claude.tool-call", vec![call.into()]).unwrap();
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "bindings".into(),
                producer_epoch: "one".into(),
                producer_seq: seq,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: "bindings".into(),
                },
                generation: 0,
                locator: SourceLocator::Frame { ordinal: seq },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("epoch").unwrap(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "capture".into(),
                value: seq as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind: TransportKind::SourceAttachment,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
            attachments: vec![attachment],
            capture_diagnostics: vec![],
        };
        let archive = ArchiveContext {
            observations: [parent.clone()].into_iter().collect(),
            correlations: [(
                crate::mapping::attachment_key(
                    &parent,
                    &event.parent_slot,
                    &event.attachment.locator,
                ),
                AnyObjectRef::Operation(operation),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        outputs.push(AttachmentMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&event).unwrap(),
            state: &MapperState::default(),
            archive: &archive,
        }));
    }
    let claims: Vec<_> = outputs.into_iter().flat_map(|out| out.claims).collect();
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    let mut reducer = crate::reducer::Reducer::default();
    reducer.apply(&claims, &evidence, 32).unwrap();
    let reduced = reducer.resolve();
    let results: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Result(result)) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        2,
        "a shared payload snapshot cannot merge different invocation results"
    );
    assert_ne!(results[0].operation, results[1].operation);
    assert!(
        results.iter().all(
            |result| result.payload.as_ref().map(|payload| &payload.reference) == Some(&payload)
        )
    );
}
