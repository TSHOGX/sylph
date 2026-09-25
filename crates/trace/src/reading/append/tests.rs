use super::*;
use crate::{
    mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper},
    reducer::{Reducer, Resolution},
};
use std::sync::Arc;

struct Sample {
    before: Arc<Reduction>,
    resolution: Resolution,
    identity: Resolution,
    previews: PayloadPreviews,
}

fn sample(old_messages: usize, extra: impl Fn(&Reduction, &[Claim]) -> Vec<Fact>) -> Sample {
    let owner =
        AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:response-user").unwrap());
    let issuer = Issuer::Environment(owner.environment().clone());
    let mut lines =
        vec![
            include_bytes!("../../../../../fixtures/native/codex/user-input-response-item.jsonl")
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap()
                .to_vec(),
        ];
    lines.extend((0..=old_messages).map(|index| {
        serde_json::to_vec(&serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message", "id": format!("message-{index}"), "role": "assistant",
                "content": [{ "type": "output_text", "text": format!("body-{index}") }]
            }
        }))
        .unwrap()
    }));
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut evidence = imbl::OrdSet::new();
    let mut before = None;
    let mut previews = PayloadPreviews::new();
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
        evidence.insert(observation_ref.clone());
        if index + 1 == lines.len() {
            let facts = extra(&reducer.resolve(), &output.claims);
            let claims: Vec<_> = facts
                .into_iter()
                .enumerate()
                .map(|(index, fact)| Claim {
                    claim_ref: ClaimRef::native(
                        owner.clone(),
                        "reader-extra",
                        vec![index.to_string()],
                    )
                    .unwrap(),
                    fact,
                    evidence: vec![observation_ref.clone()],
                    rule_id: "reader-extra".into(),
                    mapper_build: "test".into(),
                    supersedes: vec![],
                    source_revision: None,
                })
                .collect();
            reducer.apply(&claims, &evidence, 4096).unwrap();
            before = Some(reducer.resolve_incremental(100_000).reduction);
        }
        reducer.apply(&output.claims, &evidence, 4096).unwrap();
        state = output.state;
        for payload in output.payloads {
            previews.insert(
                payload.reference,
                Arc::new(PayloadPreview {
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    preview_limit: 2048,
                    total_bytes: Some(payload.bytes.len() as u64),
                    ranges: vec![PayloadRange {
                        start: 0,
                        end_exclusive: payload.bytes.len() as u64,
                    }],
                    availability: Some(ReadAvailability::Available),
                    ..Default::default()
                }),
            );
        }
        offset += raw.len() as u64;
    }
    let resolution = reducer.resolve_incremental(100_000);
    Sample {
        before: before.unwrap(),
        resolution,
        identity: reducer.resolve_incremental(100_000),
        previews,
    }
}

fn index(sample: &Sample) -> AppendIndex {
    AppendIndex::build(sample.before.clone(), 100_000, &mut AppendWork::default()).unwrap()
}

fn append(
    sample: &Sample,
    index: &AppendIndex,
    limit: usize,
    work: &mut AppendWork,
) -> Option<AppendProjection> {
    project_independent_append(
        index,
        sample.resolution.append.as_ref().unwrap(),
        &sample.previews,
        32,
        limit,
        work,
    )
}

#[test]
fn an_append_that_satisfies_older_unresolved_content_has_no_reader_certificate() {
    let sample = sample(2, |before, pending| {
        let revision = pending
            .iter()
            .find_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::MessageRevision(revision),
                } => Some(revision.revision_ref.clone()),
                _ => None,
            })
            .unwrap();
        let observation = before
            .history_contents
            .values()
            .next()
            .unwrap()
            .values()
            .next()
            .unwrap();
        let mut observation = observation.clone();
        observation.slots[0].content = HistoryContent::Message(revision);
        vec![Fact::HistoryContentObserved { observation }]
    });
    assert!(sample.resolution.append.is_none());
    assert_eq!(sample.resolution.work.full_resolutions, 1);
}

#[test]
fn a_selected_predecessor_requires_reconciling_its_selected_detail_collections() {
    let sample = sample(2, |before, _| {
        let history = before.histories.values().next().unwrap();
        let other =
            AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:other").unwrap());
        vec![Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: SelectionRef::native(
                    other.clone(),
                    "selection",
                    vec!["pinned".into()],
                )
                .unwrap(),
                agent: other,
                membership: SelectionMembership::EntrySet {
                    entries: vec![history.archive_order.back().unwrap().clone()],
                },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        }]
    });
    assert!(append(&sample, &index(&sample), 1000, &mut AppendWork::default()).is_none());
}

#[test]
fn submitted_delivery_rows_prevent_appending_after_the_archive_tail() {
    let sample = sample(2, |before, _| {
        let owner = before.histories.keys().next().unwrap().clone();
        vec![Fact::Declare {
            object: TraceObject::Delivery(Delivery {
                delivery_ref: DeliveryRef::native(owner.clone(), "server", vec!["queued".into()])
                    .unwrap(),
                sender: Author::Human { subject: None },
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent { agent: owner },
                },
                route: DeliveryRoute::ServerCommand {
                    command_id: "command".into(),
                },
            }),
        }]
    });
    assert!(append(&sample, &index(&sample), 1000, &mut AppendWork::default()).is_none());
}

#[test]
fn equal_but_replaced_snapshots_cannot_reuse_a_reader_index() {
    let sample = sample(2, |_, _| vec![]);
    let replaced = AppendIndex::build(
        Arc::new((*sample.before).clone()),
        100_000,
        &mut AppendWork::default(),
    )
    .unwrap();
    assert!(append(&sample, &replaced, 1000, &mut AppendWork::default()).is_none());
    let accepted = append(&sample, &index(&sample), 1000, &mut AppendWork::default()).unwrap();
    assert!(append(&sample, &accepted.index, 1000, &mut AppendWork::default()).is_none());
}

#[test]
fn an_identity_certificate_retains_the_index_without_examining_reader_dependencies() {
    let sample = sample(2, |_, _| vec![]);
    let appended = append(&sample, &index(&sample), 1000, &mut AppendWork::default()).unwrap();
    let mut work = AppendWork::default();
    let unchanged = project_independent_append(
        &appended.index,
        sample.identity.append.as_ref().unwrap(),
        &sample.previews,
        32,
        0,
        &mut work,
    )
    .unwrap();
    assert!(unchanged.projection.objects.is_empty());
    assert!(unchanged.projection.windows.is_empty());
    assert_eq!(work, AppendWork::default());
    assert!(Arc::ptr_eq(
        &unchanged.index.reduction,
        &sample.identity.reduction
    ));
}

#[test]
fn archive_length_does_not_increase_reader_append_guard_work() {
    let mut counts = vec![];
    for old_messages in [2, 200] {
        let sample = sample(old_messages, |_, _| vec![]);
        let mut work = AppendWork::default();
        let appended = append(&sample, &index(&sample), 1000, &mut work).unwrap();
        let full = project(&sample.resolution.reduction, &sample.previews, 32);
        for (reference, detail) in &appended.projection.objects {
            assert_eq!(Some(detail), full.objects.get(reference));
            assert_eq!(
                appended.projection.object_relations.get(reference),
                full.object_relations.get(reference)
            );
        }
        for (scope, window) in &appended.projection.windows {
            assert_eq!(
                window.as_slice(),
                &full.windows[scope][full.windows[scope].len() - window.len()..]
            );
        }
        counts.push(work);
    }
    assert_eq!(counts[0], counts[1]);
    assert!(counts[0].guard_references_examined < 32);
}

#[test]
fn boundary_relation_fanout_exhausts_a_bounded_guard_without_changing_the_index() {
    let sample = sample(2, |before, _| {
        let relation = before.relations.back().unwrap().relation.clone();
        (0..100)
            .map(|_| Fact::Relation {
                relation: relation.clone(),
            })
            .collect()
    });
    let index = index(&sample);
    let mut work = AppendWork::default();
    assert!(append(&sample, &index, 32, &mut work).is_none());
    assert!(work.guard_budget_exhausted);
    assert_eq!(work.guard_references_examined, 32);
    assert!(append(&sample, &index, 1000, &mut AppendWork::default()).is_some());
}

fn coverage_sample(count: usize) -> Sample {
    sample(2, |before, _| {
        let subject = AnyObjectRef::HistoryEntry(
            before
                .histories
                .values()
                .next()
                .unwrap()
                .archive_order
                .back()
                .unwrap()
                .clone(),
        );
        (0..count)
            .map(|index| Fact::Coverage {
                coverage: Coverage {
                    subject: subject.clone(),
                    source: SourceRef {
                        issuer: Issuer::Server("reader-test".into()),
                        instance: index.to_string(),
                    },
                    dimension: CoverageDimension::Timing,
                    interval: CoverageInterval::Object {
                        object: subject.clone(),
                    },
                    state: CoverageState::Complete,
                },
            })
            .collect()
    })
}

#[test]
fn object_scope_coverage_is_preserved_in_appended_details() {
    let sample = coverage_sample(1);
    let appended = append(&sample, &index(&sample), 1000, &mut AppendWork::default()).unwrap();
    let full = project(&sample.resolution.reduction, &sample.previews, 32);
    for (reference, detail) in appended.projection.objects {
        assert_eq!(detail.coverage, full.objects[&reference].coverage);
        assert!(detail
            .coverage
            .iter()
            .any(|coverage| coverage.dimension == CoverageDimension::Timing));
    }
}

#[test]
fn coverage_fanout_exhausts_the_reader_guard_budget() {
    let sample = coverage_sample(100);
    let mut work = AppendWork::default();
    assert!(append(&sample, &index(&sample), 32, &mut work).is_none());
    assert!(work.guard_budget_exhausted);
    assert_eq!(work.guard_references_examined, 32);
}

#[test]
fn reader_index_capacity_is_enforced_during_build_and_append() {
    let sample = sample(2, |_, _| vec![]);
    let mut work = AppendWork::default();
    assert!(AppendIndex::build(sample.before.clone(), 0, &mut work).is_none());
    assert!(work.index_unavailable);
    let mut measured = AppendWork::default();
    AppendIndex::build(sample.before.clone(), 100_000, &mut measured).unwrap();
    let index = AppendIndex::build(
        sample.before.clone(),
        measured.references_indexed,
        &mut AppendWork::default(),
    )
    .unwrap();
    let mut work = AppendWork::default();
    assert!(append(&sample, &index, 1000, &mut work).is_none());
    assert!(work.index_unavailable);
}

mod inheritance;
