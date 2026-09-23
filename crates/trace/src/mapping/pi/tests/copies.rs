use super::*;
use crate::mapping::pi::copies::COPY_PAGE_WORK;
use crate::mapping::{inventory::InventoryMapper, owner_resolution};

#[derive(Default)]
struct Harness {
    archive: ArchiveContext,
    states: BTreeMap<(String, u64), MapperState>,
    pages: usize,
    last_requests: Vec<ArchiveRequest>,
}
impl Harness {
    fn apply(&mut self, capture: &Capture, output: &MapOutput) {
        self.archive.observation_sources.insert(
            capture.observation_ref.clone(),
            capture.source_position.clone(),
        );
        for claim in &output.claims {
            if let Some(old) = self
                .archive
                .claims
                .insert(claim.claim_ref.clone(), claim.clone())
            {
                assert_eq!(old, *claim);
            }
        }
        for source in &output.source_owners {
            self.archive.source_owners.insert(
                (source.source.clone(), source.generation),
                source.owner.clone(),
            );
        }
        for source in &output.source_definitions {
            self.archive.pi_copies.retain_source(source.clone());
        }
        for witness in &output.pi_copy_witnesses {
            self.archive.pi_copies.retain_witness(witness.clone());
        }
        for plan in &output.pi_copy_plans {
            self.archive.pi_copies.retain_plan(plan.clone());
        }
        for plan in &output.owner_resolution_plans {
            owner_resolution::retain_plan(&mut self.archive.owner_resolution_plans, plan.clone());
        }
        for (key, target) in &output.correlations {
            self.archive
                .correlations
                .insert(key.clone(), target.clone());
            if !output.corroborating_correlations.contains(key) {
                self.archive.correlation_proofs.remove(key);
            }
            self.archive
                .retain_correlation_evidence(key.clone(), output.correlation_evidence[key].clone());
        }
    }
    fn record(&mut self, source: &str, ordinal: u64, value: Value, inventory: bool) {
        self.record_at(source, 0, ordinal, value, inventory);
    }
    fn record_at(
        &mut self,
        source: &str,
        generation: u64,
        ordinal: u64,
        value: Value,
        inventory: bool,
    ) {
        let mut observation = capture(ordinal, false);
        observation.observation_ref.producer_id = source.into();
        observation.source_position.source.instance = source.into();
        observation.source_position.generation = generation;
        if generation > 0 {
            observation.observation_ref.producer_epoch = format!("generation-{generation}");
        }
        let state_key = (source.to_owned(), generation);
        if inventory {
            observation.transport_kind = TransportKind::SourceInventory;
        }
        let raw = serde_json::to_vec(&value).unwrap();
        let state = self.states.get(&state_key).cloned().unwrap_or_default();
        let mapper: &dyn SemanticMapper = if inventory {
            &InventoryMapper
        } else {
            &PiMapper
        };
        let mut output = mapper.map(MapInput {
            capture: &observation,
            canonical_observation: &observation.observation_ref,
            raw: &raw,
            state: &state,
            archive: &self.archive,
        });
        self.states.insert(state_key, output.state.clone());
        loop {
            self.apply(&observation, &output);
            self.pages += 1;
            assert!(self.pages < 10000, "copy continuations did not progress");
            if !output.requests.iter().any(ArchiveRequest::is_continuation) {
                self.last_requests = output.requests.clone();
                break;
            }
            let next = output
                .continuation_state
                .clone()
                .expect("a bounded pass needs a cursor");
            output = mapper.map(MapInput {
                capture: &observation,
                canonical_observation: &observation.observation_ref,
                raw: &raw,
                state: &next,
                archive: &self.archive,
            });
            assert!(
                output.payloads.is_empty(),
                "a metadata continuation restaged the vendor body"
            );
            assert!(
                output.pi_copy_witnesses.is_empty()
                    && output.pi_copy_plans.is_empty()
                    && output.source_definitions.is_empty(),
                "a continuation repeated acquisition interpretation"
            );
            assert!(output.claims.len() <= COPY_PAGE_WORK + owner_resolution::OWNER_PAGE_WORK);
        }
    }
    fn original(&mut self) {
        self.record(
            "parent",
            0,
            serde_json::json!({"type":"session","id":"original"}),
            false,
        );
    }
    fn fork(&mut self) {
        self.record("child",0,serde_json::json!({"type":"session","id":"fork","parentSession":"/recorded/original.jsonl"}),false);
    }
    fn inventory(&mut self) {
        self.inventory_on("inventory");
    }
    fn inventory_on(&mut self, name: &str) {
        let native = ObservationRef {
            issuer: Issuer::Environment(EnvironmentId::parse("env").unwrap()),
            producer_id: "parent".into(),
            producer_epoch: "epoch".into(),
            producer_seq: 1,
        };
        self.record(
            name,
            0,
            serde_json::to_value(SourceDefinition {
                source: SourceRef {
                    issuer: native.issuer.clone(),
                    instance: "parent".into(),
                },
                generation: 0,
                locator: "/recorded/original.jsonl".into(),
                owner: AgentRef::Session(SessionId::decode("env:pi:original").unwrap()),
                basis: SourceOwnerBasis::NativeIdentity {
                    observation: native,
                },
            })
            .unwrap(),
            true,
        );
    }
    fn links(&self) -> Vec<(EntryRef, EntryRef)> {
        self.archive
            .claims
            .values()
            .filter_map(|claim| match &claim.fact {
                Fact::Relation {
                    relation:
                        Relation::ContextCopy {
                            original, copied, ..
                        },
                } => Some((original.clone(), copied.clone())),
                _ => None,
            })
            .collect()
    }
}
fn row(id: &str, parent: Option<&str>, text: &str) -> Value {
    serde_json::json!({"type":"message","id":id,"parentId":parent,"timestamp":"2026-09-09T00:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":text}]},"opaque":{"kept":true}})
}

#[test]
fn pi_copy_matching_keeps_native_ids_content_and_rewritten_predecessors_distinct() {
    let mut harness = Harness::default();
    harness.original();
    harness.fork();
    harness.inventory();
    harness.record(
        "parent",
        1,
        row("same-id", Some("removed-label"), "Retained copy"),
        false,
    );
    let mut changed = row("same-id", None, "Changed content");
    harness.record("child", 1, changed.clone(), false);
    assert!(harness.links().is_empty());
    changed = row("different-id", None, "Retained copy");
    harness.record("child", 2, changed, false);
    assert!(
        harness.links().is_empty(),
        "equal content is not native identity"
    );
    let mut different_unknown = row("same-id", None, "Retained copy");
    different_unknown["opaque"]["kept"] = false.into();
    harness.record("child", 3, different_unknown, false);
    assert!(
        harness.links().is_empty(),
        "unknown fields remain part of the copy witness"
    );
    harness.record("child", 4, row("same-id", None, "Retained copy"), false);
    let links = harness.links();
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].0.scope().anchor_session().native_id.as_str(),
        "original"
    );
    assert_eq!(
        links[0].1.scope().anchor_session().native_id.as_str(),
        "fork"
    );
    let copied = harness
        .archive
        .claims
        .values()
        .find_map(|claim| match &claim.fact {
            Fact::Declare {
                object: TraceObject::HistoryEntry(entry),
            } if entry.entry_ref == links[0].1 => Some(entry),
            _ => None,
        })
        .unwrap();
    assert!(copied.predecessor.is_none());
    let proof = harness
        .archive
        .claims
        .values()
        .find(|claim| {
            matches!(
                claim.fact,
                Fact::Relation {
                    relation: Relation::ContextCopy { .. }
                }
            )
        })
        .unwrap();
    assert_eq!(proof.evidence.len(), 5);
    let Fact::Relation {
        relation:
            Relation::ContextCopy {
                original_position,
                copied_position,
                ..
            },
    } = &proof.fact
    else {
        unreachable!()
    };
    assert!(matches!(
        original_position.locator,
        SourceLocator::Jsonl {
            record_ordinal: 1,
            ..
        }
    ));
    assert!(matches!(
        copied_position.locator,
        SourceLocator::Jsonl {
            record_ordinal: 4,
            ..
        }
    ));
    assert!(
        proof.supersedes.is_empty(),
        "optional lineage cannot replace the child's content declaration"
    );
}

#[test]
fn late_pi_copy_witnesses_and_locator_certificates_produce_identical_claims() {
    let mut expected = None;
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut harness = Harness::default();
        harness.original();
        harness.fork();
        for action in order {
            match action {
                0 => harness.record("parent", 1, row("copied", None, "Copied text"), false),
                1 => harness.record("child", 1, row("copied", None, "Copied text"), false),
                _ => harness.inventory(),
            }
        }
        assert_eq!(harness.links().len(), 1);
        if let Some(expected) = &expected {
            assert_eq!(&harness.archive.claims, expected);
        } else {
            expected = Some(harness.archive.claims);
        }
    }
}

#[test]
fn pi_copy_continuations_finish_large_catalogues_without_reinterpreting_bodies() {
    let mut harness = Harness::default();
    harness.original();
    harness.fork();
    let count = COPY_PAGE_WORK * 2;
    for index in 0..count {
        let id = format!("entry-{index}");
        let value = row(&id, None, "Body stays independently retained");
        harness.record("parent", index as u64 + 1, value.clone(), false);
        harness.record("child", index as u64 + 1, value, false);
    }
    let before = harness.pages;
    harness.inventory();
    assert!(harness.pages > before + 1);
    assert_eq!(harness.links().len(), count);
}

#[test]
fn pi_copy_witnesses_must_belong_to_the_exact_located_source_generation() {
    for other_file in [false, true] {
        let mut harness = Harness::default();
        harness.original();
        harness.fork();
        harness.inventory();
        let source = if other_file { "other-file" } else { "parent" };
        let generation = u64::from(!other_file);
        harness.record_at(
            source,
            generation,
            0,
            serde_json::json!({"type":"session","id":"original"}),
            false,
        );
        harness.record_at(
            source,
            generation,
            1,
            row("copied", None, "Same record"),
            false,
        );
        harness.record("child", 1, row("copied", None, "Same record"), false);
        assert!(
            harness.links().is_empty(),
            "a matching logical owner cannot replace the source incarnation certificate"
        );
        harness.record("parent", 1, row("copied", None, "Same record"), false);
        assert_eq!(harness.links().len(), 1);
    }
}

#[test]
fn only_compaction_kept_boundaries_are_excluded_from_pi_copy_comparison() {
    for kind in ["compaction", "custom"] {
        let mut harness = Harness::default();
        harness.original();
        harness.fork();
        harness.inventory();
        let original = serde_json::json!({"type":kind,"id":"entry","parentId":"label","summary":"Retained summary","firstKeptEntryId":"label"});
        let copied = serde_json::json!({"firstKeptEntryId":"kept-message","summary":"Retained summary","parentId":null,"id":"entry","type":kind});
        harness.record("parent", 1, original, false);
        harness.record("child", 1, copied, false);
        assert_eq!(harness.links().len(), usize::from(kind == "compaction"));
    }
}

#[test]
fn copy_continuations_preserve_unresolved_tool_dependencies_after_metadata_work_finishes() {
    let mut harness = Harness::default();
    harness.original();
    for index in 0..COPY_PAGE_WORK {
        harness.inventory_on(&format!("inventory-{index}"));
    }
    harness.fork();
    let original = serde_json::json!({"type":"message","id":"result","parentId":null,"message":{"role":"toolResult","toolCallId":"late","content":[{"type":"text","text":"Retained result"}]}});
    harness.record("parent", 1, original.clone(), false);
    let mut copied = original;
    copied["parentId"] = "unobserved-invocation".into();
    let before = harness.pages;
    harness.record("child", 1, copied, false);
    assert!(
        harness.pages > before + 1,
        "the copy's own continuation was not exercised"
    );
    assert_eq!(harness.links().len(), COPY_PAGE_WORK);
    assert!(harness.last_requests.iter().any(|request|matches!(request,ArchiveRequest::Correlation {namespace,key,..} if namespace=="pi.tool-at-entry" && key==&["unobserved-invocation","late"])),"finishing an independent copy join lost the tool-result dependency");
}

#[test]
fn malformed_pi_entry_ids_report_a_decode_gap_without_creating_copy_witnesses() {
    for id in [Value::Null, Value::String(String::new()), Value::from(17)] {
        let output = run(
            &[
                header(),
                serde_json::json!({"type":"message","id":id,"parentId":null,"message":{"role":"assistant","content":[{"type":"text","text":"Retain invalid identity"}]}}),
            ],
            false,
        );
        assert!(output[1].pi_copy_witnesses.is_empty());
        assert!(output[1].claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::Diagnostic {
                diagnostic: TraceDiagnostic {
                    code: DiagnosticCode::DecodeFailure,
                    ..
                }
            }
        )));
    }
}

#[test]
fn copy_proofs_keep_the_inventory_header_witness_separate_from_the_entry_header_witness() {
    let mut harness = Harness::default();
    harness.original();
    harness.fork();
    harness.record("parent", 1, row("entry", None, "Retained copy"), false);
    harness.record("child", 1, row("entry", None, "Retained copy"), false);
    let mut reobserved = capture(0, false);
    reobserved.observation_ref.producer_id = "independent-header".into();
    reobserved.source_position.source.instance = "parent".into();
    let output = PiMapper.map(MapInput {
        capture: &reobserved,
        canonical_observation: &reobserved.observation_ref,
        raw: br#"{"type":"session","id":"original"}"#,
        state: &MapperState::default(),
        archive: &harness.archive,
    });
    harness.apply(&reobserved, &output);
    harness.record(
        "inventory",
        0,
        serde_json::to_value(SourceDefinition {
            source: reobserved.source_position.source.clone(),
            generation: 0,
            locator: "/recorded/original.jsonl".into(),
            owner: AgentRef::Session(SessionId::decode("env:pi:original").unwrap()),
            basis: SourceOwnerBasis::NativeIdentity {
                observation: reobserved.observation_ref.clone(),
            },
        })
        .unwrap(),
        true,
    );
    let proof = harness
        .archive
        .claims
        .values()
        .find(|claim| {
            matches!(
                claim.fact,
                Fact::Relation {
                    relation: Relation::ContextCopy { .. }
                }
            )
        })
        .unwrap();
    assert!(proof.evidence.contains(&reobserved.observation_ref));
    assert!(proof.evidence.iter().any(|observation|observation.producer_id=="parent" && observation.producer_seq==1));
    assert_eq!(harness.links().len(), 1);
}
