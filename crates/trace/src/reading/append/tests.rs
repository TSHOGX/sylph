use super::*;
use crate::{
    mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper},
    reducer::Reducer,
};

fn sample() -> (Reduction, Reduction, BTreeMap<PayloadRef, PayloadPreview>) {
    let owner =
        AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:response-user").unwrap());
    let issuer = Issuer::Environment(owner.environment().clone());
    let lines = [
        include_bytes!("../../../../../fixtures/native/codex/user-input-response-item.jsonl").split(|byte| *byte == b'\n').next().unwrap().to_vec(),
        br#"{"type":"response_item","payload":{"type":"message","id":"old","role":"assistant","content":[{"type":"output_text","text":"old body"}]}}"#.to_vec(),
        br#"{"type":"response_item","payload":{"type":"message","id":"new","role":"assistant","content":[{"type":"output_text","text":"new body"}]}}"#.to_vec(),
    ];
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut evidence = BTreeSet::new();
    let mut before = Reduction::default();
    let mut previews = BTreeMap::new();
    let mut offset = 0;
    for (index, raw) in lines.iter().enumerate() {
        let observation_ref = ObservationRef {
            issuer: issuer.clone(),
            producer_id: "append".into(),
            producer_epoch: "one".into(),
            producer_seq: index as u64 + 1,
        };
        let capture = Capture {
            observation_ref: observation_ref.clone(),
            source_position: SourcePosition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: "native".into(),
                },
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + raw.len() as u64,
                    record_ordinal: index as u64,
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("epoch").unwrap(),
            declared_subject_refs: vec![owner.clone()],
            captured_at: TimeEvidence {
                domain: "fixture".into(),
                value: index as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes(blake3::hash(raw).to_hex()).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        let output = crate::mapping::codex::CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &observation_ref,
            raw,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        evidence.insert(observation_ref);
        reducer.apply(&output.claims, &evidence, 100).unwrap();
        state = output.state;
        for payload in output.payloads {
            previews.insert(
                payload.reference,
                PayloadPreview {
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    preview_limit: 2048,
                    total_bytes: Some(payload.bytes.len() as u64),
                    ranges: vec![PayloadRange {
                        start: 0,
                        end_exclusive: payload.bytes.len() as u64,
                    }],
                    availability: Some(ReadAvailability::Available),
                    ..Default::default()
                },
            );
        }
        if index == 1 {
            before = reducer.resolve();
        }
        offset += raw.len() as u64;
    }
    let after = reducer.resolve();
    assert!(project_independent_append(
        &before,
        &after,
        &ReductionChanges::between(&before, &after),
        &previews,
        32
    )
    .is_some());
    (before, after, previews)
}

fn assert_reconciliation(
    before: &Reduction,
    after: &Reduction,
    previews: &BTreeMap<PayloadRef, PayloadPreview>,
) {
    assert!(project_independent_append(
        before,
        after,
        &ReductionChanges::between(before, after),
        previews,
        32
    )
    .is_none());
}

#[test]
fn an_append_that_satisfies_older_unresolved_content_reconciles_the_old_rows() {
    let (mut before, mut after, previews) = sample();
    let revision = after
        .messages
        .iter()
        .find(|(key, _)| !before.messages.contains_key(*key))
        .unwrap()
        .1
        .revisions[0]
        .clone();
    let entry = before.history_contents.keys().next().unwrap().clone();
    for reduction in [&mut before, &mut after] {
        for observation in reduction
            .history_contents
            .get_mut(&entry)
            .unwrap()
            .values_mut()
        {
            observation.slots[0].content = HistoryContent::Message(revision.clone());
        }
    }
    assert_reconciliation(&before, &after, &previews);
}

#[test]
fn a_selected_predecessor_requires_reconciling_its_selected_detail_collections() {
    let (mut before, mut after, previews) = sample();
    let (owner, history) = before.histories.iter().next().unwrap();
    let selection =
        SelectionRef::native(owner.clone(), "selection", vec!["pinned".into()]).unwrap();
    let members = Some(history.archive_order.clone());
    for reduction in [&mut before, &mut after] {
        reduction
            .selection_memberships
            .insert(selection.clone(), members.clone());
    }
    assert_reconciliation(&before, &after, &previews);
}

#[test]
fn submitted_delivery_rows_prevent_appending_after_the_archive_tail() {
    let (mut before, mut after, previews) = sample();
    let owner = before.histories.keys().next().unwrap().clone();
    let reference = DeliveryRef::native(owner.clone(), "server", vec!["queued".into()]).unwrap();
    let object = TraceObject::Delivery(Delivery {
        delivery_ref: reference.clone(),
        sender: Author::Human { subject: None },
        recipient: Recipient::Agent {
            agent: OwnerRef::Agent { agent: owner },
        },
        route: DeliveryRoute::ServerCommand {
            command_id: "command".into(),
        },
    });
    let reference = AnyObjectRef::Delivery(reference);
    for reduction in [&mut before, &mut after] {
        reduction.objects.insert(
            reference.clone(),
            crate::reducer::ResolvedObject {
                reference: reference.clone(),
                declaration: Resolved::Known {
                    value: object.clone(),
                    claims: vec![],
                },
            },
        );
    }
    assert_reconciliation(&before, &after, &previews);
}
