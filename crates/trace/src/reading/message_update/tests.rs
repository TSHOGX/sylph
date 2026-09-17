use super::*;
use crate::mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper};
use crate::reducer::{Reducer, Resolution};

#[derive(Default)]
struct Stream {
    reducer: Reducer,
    state: MapperState,
    previews: PayloadPreviews,
    sequence: u64,
}

impl Stream {
    fn capture(&self) -> Capture {
        let issuer = Issuer::Environment(sylph_protocol::EnvironmentId::parse("env").unwrap());
        Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "live".into(),
                producer_epoch: "one".into(),
                producer_seq: self.sequence,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: "live".into(),
                },
                generation: 0,
                locator: SourceLocator::Frame {
                    ordinal: self.sequence,
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("reader-test").unwrap(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "receipt".into(),
                value: self.sequence as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::LiveStdout,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        }
    }

    fn push(&mut self, method: &str, params: serde_json::Value) -> Resolution {
        self.sequence += 1;
        let capture = self.capture();
        let output = crate::mapping::codex::CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&serde_json::json!({ "method": method, "params": params }))
                .unwrap(),
            state: &self.state,
            archive: &ArchiveContext::default(),
        });
        self.reducer
            .apply(
                &output.claims,
                &output
                    .claims
                    .iter()
                    .flat_map(|claim| claim.evidence.iter().cloned())
                    .collect(),
                1000,
            )
            .unwrap();
        self.state = output.state;
        for payload in output.payloads {
            self.previews.insert(
                payload.reference,
                Arc::new(PayloadPreview {
                    preview: String::from_utf8(payload.bytes.clone()).unwrap(),
                    preview_limit: 2048,
                    total_bytes: Some(payload.bytes.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    ..Default::default()
                }),
            );
        }
        self.reducer.resolve_incremental(100_000)
    }

    fn facts(&mut self, facts: Vec<Fact>) -> Arc<Reduction> {
        self.facts_resolution(facts).reduction
    }

    fn facts_resolution(&mut self, facts: Vec<Fact>) -> Resolution {
        self.sequence += 1;
        let capture = self.capture();
        let owner = self
            .reducer
            .resolve_incremental(100_000)
            .reduction
            .histories
            .keys()
            .next()
            .unwrap()
            .clone();
        let claims: Vec<_> = facts
            .into_iter()
            .enumerate()
            .map(|(index, fact)| Claim {
                claim_ref: ClaimRef::occurrence(
                    owner.clone(),
                    capture.source_position.clone(),
                    vec![self.sequence.to_string(), index.to_string()],
                )
                .unwrap(),
                fact,
                evidence: vec![capture.observation_ref.clone()],
                rule_id: "reader-facts".into(),
                mapper_build: "test".into(),
                supersedes: vec![],
                source_revision: None,
            })
            .collect();
        self.reducer
            .apply(
                &claims,
                &std::iter::once(capture.observation_ref).collect(),
                1000,
            )
            .unwrap();
        self.reducer.resolve_incremental(100_000)
    }

    fn delta(&mut self, text: &str) -> Resolution {
        self.push("item/agentMessage/delta", serde_json::json!({ "threadId": "root", "turnId": "turn", "itemId": "stream", "delta": text }))
    }
}

fn seeded(unrelated: usize) -> (Stream, Arc<Reduction>) {
    let mut stream = Stream::default();
    stream.push(
        "thread/started",
        serde_json::json!({ "thread": { "id": "root" } }),
    );
    stream.push(
        "turn/started",
        serde_json::json!({ "threadId": "root", "turn": { "id": "turn" } }),
    );
    for index in 0..unrelated {
        stream.push("item/completed", serde_json::json!({ "threadId": "root", "turnId": "turn", "item": { "id": format!("old-{index}"), "type": "agentMessage", "text": "unrelated", "phase": "final_answer" } }));
    }
    let reduction = stream.delta("A中").reduction;
    (stream, reduction)
}

fn build(reduction: Arc<Reduction>) -> AppendIndex {
    AppendIndex::build(reduction, 100_000, &mut AppendWork::default()).unwrap()
}

fn compare_region(region: &ReadingProjection, full: &ReadingProjection) {
    for (scope, units) in &region.windows {
        for unit in units {
            assert_eq!(
                Some(unit),
                full.windows[scope]
                    .iter()
                    .find(|candidate| candidate.address == unit.address)
            );
        }
    }
    for (reference, detail) in &region.objects {
        assert_eq!(Some(detail), full.objects.get(reference));
        assert_eq!(
            region.object_relations.get(reference),
            full.object_relations.get(reference)
        );
    }
    for (key, parts) in &region.parts {
        assert_eq!(Some(parts), full.parts.get(key));
    }
    for (reference, payload) in &region.composed_payloads {
        assert_eq!(payload.manifest, full.composed_payloads[reference].manifest);
        assert_eq!(
            payload.revisions,
            full.composed_payloads[reference].revisions
        );
    }
}

#[test]
fn streamed_message_replacements_match_full_reading_and_keep_the_first_row() {
    let (mut stream, before) = seeded(2);
    let mut index = build(before);
    let changes = [
        (
            "item/agentMessage/delta",
            serde_json::json!({ "threadId": "root", "turnId": "turn", "itemId": "stream", "delta": "A中" }),
        ),
        (
            "item/started",
            serde_json::json!({ "threadId": "root", "turnId": "turn", "item": { "id": "stream", "type": "agentMessage", "text": "B" } }),
        ),
        (
            "item/agentMessage/delta",
            serde_json::json!({ "threadId": "root", "turnId": "turn", "itemId": "stream", "delta": "C" }),
        ),
        (
            "item/completed",
            serde_json::json!({ "threadId": "root", "turnId": "turn", "item": { "id": "stream", "type": "agentMessage", "text": "Final differs", "phase": "final_answer" } }),
        ),
        (
            "item/agentMessage/delta",
            serde_json::json!({ "threadId": "root", "turnId": "turn", "itemId": "stream", "delta": "late" }),
        ),
    ];
    let mut first_address = None;
    for (method, params) in changes {
        let old_previews = stream.previews.clone();
        let resolution = stream.push(method, params);
        let proof = resolution.message_update.as_ref().unwrap();
        let projected = project_message_update(
            &index,
            proof,
            &old_previews,
            &stream.previews,
            32,
            10_000,
            &mut AppendWork::default(),
        )
        .unwrap();
        compare_region(
            &projected.before,
            &project(proof.before(), &old_previews, 32),
        );
        compare_region(
            &projected.after,
            &project(proof.after(), &stream.previews, 32),
        );
        let row = &projected.after.windows.values().next().unwrap()[0];
        assert!(row.activation.is_some());
        if let Some(first) = &first_address {
            assert_eq!(&row.address, first);
        }
        first_address = Some(row.address.clone());
        index = projected.index;
    }
}

#[test]
fn unrelated_history_does_not_increase_message_replacement_work() {
    let mut counts = vec![];
    for unrelated in [2, 200] {
        let (mut stream, before) = seeded(unrelated);
        let index = build(before);
        let old_previews = stream.previews.clone();
        let resolution = stream.delta("next");
        let mut work = AppendWork::default();
        project_message_update(
            &index,
            resolution.message_update.as_ref().unwrap(),
            &old_previews,
            &stream.previews,
            32,
            1000,
            &mut work,
        )
        .unwrap();
        counts.push(work);
    }
    assert_eq!(counts[0], counts[1]);
}

#[test]
fn message_replacement_refuses_a_value_equal_but_unrelated_index() {
    let (mut stream, before) = seeded(2);
    let index = build(Arc::new((*before).clone()));
    let old_previews = stream.previews.clone();
    let resolution = stream.delta("next");
    assert!(project_message_update(
        &index,
        resolution.message_update.as_ref().unwrap(),
        &old_previews,
        &stream.previews,
        32,
        1000,
        &mut AppendWork::default()
    )
    .is_none());
}

#[test]
fn message_replacement_budget_exhaustion_leaves_the_committed_index_reusable() {
    let (mut stream, before) = seeded(2);
    let index = build(before);
    let old_previews = stream.previews.clone();
    let resolution = stream.delta("next");
    let proof = resolution.message_update.as_ref().unwrap();
    let mut work = AppendWork::default();
    assert!(project_message_update(
        &index,
        proof,
        &old_previews,
        &stream.previews,
        32,
        4,
        &mut work
    )
    .is_none());
    assert!(work.guard_budget_exhausted);
    assert_eq!(work.guard_references_examined, 4);
    assert!(project_message_update(
        &index,
        proof,
        &old_previews,
        &stream.previews,
        32,
        1000,
        &mut AppendWork::default()
    )
    .is_some());
}

#[test]
fn another_agents_selection_of_the_message_requires_reconciliation() {
    let (mut stream, before) = seeded(2);
    let entry = before
        .histories
        .values()
        .next()
        .unwrap()
        .archive_order
        .back()
        .unwrap()
        .clone();
    let other = AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:other").unwrap());
    let before = stream.facts(vec![Fact::Declare {
        object: TraceObject::HistorySelection(HistorySelection {
            selection_ref: SelectionRef::native(other.clone(), "selection", vec!["held".into()])
                .unwrap(),
            agent: other,
            membership: SelectionMembership::EntrySet {
                entries: vec![entry],
            },
            content_scope: HistoryContentScope::AllObservedAtCut,
            excluded_heads: vec![],
        }),
    }]);
    let old_previews = stream.previews.clone();
    let index = build(before);
    let resolution = stream.delta("next");
    assert!(project_message_update(
        &index,
        resolution.message_update.as_ref().unwrap(),
        &old_previews,
        &stream.previews,
        32,
        1000,
        &mut AppendWork::default()
    )
    .is_none());
}

#[test]
fn a_delivery_bound_to_the_message_requires_reconciliation_without_an_admitted_entry() {
    let (mut stream, before) = seeded(2);
    let agent = before.histories.keys().next().unwrap().clone();
    let entry = before.histories[&agent].archive_order.back().unwrap();
    let contents = before.entry_contents(entry, None);
    let HistoryContent::Message(revision) =
        &contents.slots.values().next().unwrap().known().unwrap()[0]
    else {
        panic!("message binding missing");
    };
    let before = stream.facts(vec![Fact::DeliveryRevisionObserved {
        delivery: DeliveryRef::native(agent, "delivery", vec!["pending".into()]).unwrap(),
        revision: revision.clone(),
    }]);
    let old_previews = stream.previews.clone();
    let index = build(before);
    let resolution = stream.delta("next");
    assert!(project_message_update(
        &index,
        resolution.message_update.as_ref().unwrap(),
        &old_previews,
        &stream.previews,
        32,
        1000,
        &mut AppendWork::default()
    )
    .is_none());
}

#[test]
fn shared_part_appends_and_multipart_replacements_match_the_full_reader() {
    let (mut stream, before) = seeded(2);
    let agent = before.histories.keys().next().unwrap().clone();
    let initial_entry = before.histories[&agent].archive_order.back().unwrap();
    let contents = before.entry_contents(initial_entry, None);
    let HistoryContent::Message(initial_revision) =
        &contents.slots.values().next().unwrap().known().unwrap()[0]
    else {
        panic!("message binding missing");
    };
    let TraceObject::MessageRevision(initial) = before
        .object(&AnyObjectRef::MessageRevision(initial_revision.clone()))
        .unwrap()
        .declaration
        .known()
        .unwrap()
    else {
        panic!("revision missing");
    };
    let mut index = build(before.clone());
    let mut previous = initial_revision.clone();
    for (step, texts) in [
        (0, vec![" appended", "second part"]),
        (1, vec!["replacement only"]),
    ] {
        let old_previews = stream.previews.clone();
        let mut parts = vec![];
        for (part_index, text) in texts.iter().enumerate() {
            let payload = PayloadRef::bytes(blake3::hash(text.as_bytes()).to_hex()).unwrap();
            stream.previews.insert(
                payload.clone(),
                Arc::new(PayloadPreview {
                    preview: (*text).into(),
                    preview_limit: 2048,
                    total_bytes: Some(text.len() as u64),
                    availability: Some(ReadAvailability::Available),
                    ..Default::default()
                }),
            );
            let mut part = initial.parts[0].clone();
            if part_index > 0 {
                part.part_ref = MessagePartRef::native(
                    agent.clone(),
                    "multipart",
                    vec![part_index.to_string()],
                )
                .unwrap();
            }
            part.payload = payload;
            parts.push(part);
        }
        let revision =
            MessageRevisionRef::native(agent.clone(), "multipart", vec![step.to_string()]).unwrap();
        let entry = EntryRef::native(agent.clone(), "multipart", vec![step.to_string()]).unwrap();
        let predecessor = index.reduction.histories[&agent]
            .archive_order
            .back()
            .unwrap()
            .clone();
        let position = stream.capture().source_position;
        let resolution = stream.facts_resolution(vec![
            Fact::Declare {
                object: TraceObject::MessageRevision(MessageRevision {
                    revision_ref: revision.clone(),
                    message: initial.message.clone(),
                    representation: initial.representation,
                    form: if step == 0 {
                        RevisionForm::Append {
                            predecessor: Some(previous.clone()),
                        }
                    } else {
                        RevisionForm::Replacement { covers: None }
                    },
                    parts,
                }),
            },
            Fact::Declare {
                object: TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry.clone(),
                    containing_agent: agent.clone(),
                    predecessor: Some(predecessor.clone()),
                    original_owner: None,
                }),
            },
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry.clone(),
                    position,
                    "reader-multipart",
                    vec![HistoryContent::Message(revision.clone())],
                ),
            },
            Fact::Relation {
                relation: Relation::HistoryPredecessor { predecessor, entry },
            },
        ]);
        let proof = resolution.message_update.as_ref().unwrap();
        let projected = project_message_update(
            &index,
            proof,
            &old_previews,
            &stream.previews,
            32,
            1000,
            &mut AppendWork::default(),
        )
        .unwrap();
        compare_region(
            &projected.before,
            &project(proof.before(), &old_previews, 32),
        );
        compare_region(
            &projected.after,
            &project(proof.after(), &stream.previews, 32),
        );
        let ReadingContent::Message { parts, .. } =
            &projected.after.windows.values().next().unwrap()[0].content
        else {
            panic!("message row missing");
        };
        assert_eq!(parts.len(), texts.len());
        assert_eq!(parts[0].part_ref, initial.parts[0].part_ref);
        assert_eq!(
            parts[0].preview,
            if step == 0 {
                "A中 appended"
            } else {
                "replacement only"
            }
        );
        previous = revision;
        index = projected.index;
    }
}
