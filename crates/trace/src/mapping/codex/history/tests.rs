use super::*;
use crate::mapping::codex::CodexMapper;
use crate::mapping::{ArchiveContext, SemanticMapper};
use crate::reducer::Reducer;
use sylph_protocol::{EnvironmentId, NativeDatabaseStatus};

const ROOT: &str = "11111111-1111-4111-8111-111111111111";
const REVERT: &str = "22222222-2222-4222-8222-222222222222";
const CHILD: &str = "33333333-3333-4333-8333-333333333333";

fn agent(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env:codex:{id}")).unwrap())
}
fn source(id: &str) -> SourceRef {
    SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env").unwrap()),
        instance: id.into(),
    }
}
fn capture(id: &str, seq: u64, locator: SourceLocator, kind: TransportKind) -> Capture {
    Capture {
        observation_ref: ObservationRef {
            issuer: source(id).issuer.clone(),
            producer_id: id.into(),
            producer_epoch: "incarnation".into(),
            producer_seq: seq,
        },
        source_position: SourcePosition {
            source: source(id),
            generation: 0,
            locator,
        },
        ingestion_authorization_epoch: IngestionEpoch::new("epoch").unwrap(),
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "fixture".into(),
            value: 0,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::SourceWall,
        },
        source_clock: None,
        transport_kind: kind,
        vendor_build: None,
        schema_fingerprint: None,
        payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
        attachments: vec![],
        capture_diagnostics: vec![],
    }
}
#[derive(Default)]
struct Fixture {
    archive: ArchiveContext,
    claims: Vec<Claim>,
    evidence: BTreeSet<ObservationRef>,
    offsets: BTreeMap<String, Vec<u64>>,
}
impl Fixture {
    fn apply(&mut self, output: MapOutput) {
        for owner in output.source_owners {
            self.archive
                .source_owners
                .insert((owner.source, owner.generation), owner.owner);
        }
        for record in output.history_records {
            let SourceLocator::Jsonl { record_ordinal, .. } = record.position.locator else {
                panic!()
            };
            self.archive
                .history_records
                .entry((record.position.source.clone(), record.position.generation))
                .or_default()
                .insert(record_ordinal, record);
        }
        for claim in &output.claims {
            self.evidence.extend(claim.evidence.iter().cloned());
            match &claim.fact {
                Fact::Declare {
                    object: TraceObject::HistoryEntry(entry),
                } => {
                    self.archive
                        .history_entries
                        .insert(entry.entry_ref.clone(), entry.clone());
                    self.archive.object_evidence.insert(
                        AnyObjectRef::HistoryEntry(entry.entry_ref.clone()),
                        claim.evidence.clone(),
                    );
                }
                Fact::HistoryMappingComplete { extent } => self
                    .archive
                    .source_prefixes
                    .entry(extent.source.clone())
                    .or_default()
                    .push(extent.clone()),
                _ => {}
            }
        }
        self.claims.extend(output.claims);
    }
    fn native(
        &mut self,
        physical: &str,
        logical: &str,
        base: Option<Value>,
        start: u64,
        messages: usize,
    ) {
        let mut values = vec![
            serde_json::json!({"type":"session_meta","ordinal":start,"payload":{"id":logical,"history_mode":"paginated","history_base":base}}),
        ];
        values.extend((0..messages).map(|index|serde_json::json!({"type":"response_item","ordinal":start+index as u64+1,"payload":{"type":"message","id":format!("{physical}-{index}"),"role":"assistant","content":[{"type":"output_text","text":format!("message {physical} {index}")}]}})));
        let mut state = MapperState::default();
        let mut offset = 0;
        let mut ends = vec![];
        for (index, value) in values.iter().enumerate() {
            let mut raw = serde_json::to_vec(value).unwrap();
            raw.push(b'\n');
            let capture = capture(
                physical,
                index as u64 + 1,
                SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + raw.len() as u64,
                    record_ordinal: index as u64,
                },
                TransportKind::NativeJsonl,
            );
            let output = CodexMapper.map(MapInput {
                capture: &capture,
                canonical_observation: &capture.observation_ref,
                raw: &raw,
                state: &state,
                archive: &self.archive,
            });
            state = output.state.clone();
            self.apply(output);
            offset += raw.len() as u64;
            ends.push(offset);
        }
        let header = self.archive.history_records[&(source(physical), 0)][&0].clone();
        self.archive.source_definitions.insert(
            (source(physical), 0),
            RecordedSource {
                definition: SourceDefinition {
                    source: source(physical),
                    generation: 0,
                    locator: format!("/vendor/rollout-2026-09-16T00-00-00-{physical}.jsonl"),
                    owner: agent(logical),
                    basis: SourceOwnerBasis::NativeIdentity {
                        observation: header.observation.clone(),
                    },
                },
                observation: header.observation,
            },
        );
        self.offsets.insert(physical.into(), ends);
    }
    fn base(&self, physical: &str, local_end: usize) -> Value {
        let first = &self.archive.history_records[&(source(physical), 0)][&0];
        serde_json::json!({"thread_id":physical,"end_ordinal_exclusive":first.vendor_ordinal.unwrap()+local_end as u64,"end_byte_offset":self.offsets[physical][local_end-1]})
    }
    fn map(&self, physical: &str, logical: &str, state: &MapperState) -> MapOutput {
        let capture = capture(
            "database",
            1,
            SourceLocator::Occurrence {
                key: "selected".into(),
            },
            TransportKind::NativeDatabase,
        );
        let snapshot = NativeDatabaseSnapshot {
            database_locator: "/vendor/state_5.sqlite".into(),
            table: "threads".into(),
            columns: vec![],
            status: NativeDatabaseStatus::Available,
            row: [("id".into(), NativeDatabaseValue::Text(logical.into()))].into(),
            selected_source: Some(source(physical)),
            selected_generation: Some(0),
            selected_record_count: Some(self.offsets[physical].len() as u64),
            selected_pending_bytes: Some(0),
        };
        CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&snapshot).unwrap(),
            state,
            archive: &self.archive,
        })
    }
    fn select(&mut self, physical: &str, logical: &str) -> usize {
        let mut state = MapperState::default();
        let mut passes = 0;
        loop {
            let output = self.map(physical, logical, &state);
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            let continuation = output.continuation_state.clone();
            let requests = output.requests.clone();
            self.apply(output);
            passes += 1;
            if let Some(next) = continuation {
                state = next;
            } else {
                assert!(requests.is_empty(), "{requests:?}");
                break;
            }
            assert!(passes < 100);
        }
        passes
    }
    fn selected(&self, logical: &str) -> Vec<HistoryEntry> {
        let mut reducer = Reducer::default();
        reducer
            .apply(&self.claims, &self.evidence, 100_000)
            .unwrap();
        let reduced = reducer.resolve();
        reduced.histories[&agent(logical)]
            .selected_entries
            .as_ref()
            .expect("selection must resolve")
            .iter()
            .map(|entry| self.archive.history_entries[entry].clone())
            .collect()
    }
}

#[test]
fn frozen_revert_uses_recorded_ordinal_214_and_exact_byte_boundary_not_line_number() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "logical", None, 0, 215);
    let base = fixture.base(ROOT, 214);
    fixture.native(REVERT, "logical", Some(base), 214, 1);
    fixture.select(REVERT, "logical");
    let selected = fixture.selected("logical");
    assert_eq!(selected.len(), 214);
    assert_eq!(
        selected.last().unwrap().entry_ref.scope(),
        &agent("logical")
    );
    assert!(
        matches!(selected.last().unwrap().entry_ref.address(),ObjectAddress::Occurrence {position,..} if position.source==source(REVERT))
    );
    assert!(selected.iter().all(|entry| !matches!(entry.entry_ref.address(),ObjectAddress::Occurrence {position,..} if position.source==source(ROOT)&&matches!(position.locator,SourceLocator::Jsonl {record_ordinal:214..,..}))));
}

#[test]
fn referenced_parent_context_is_child_scoped_and_expands_in_bounded_pages() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "parent", None, 0, 110);
    let base = fixture.base(ROOT, 111);
    fixture.native(CHILD, "child", Some(base), 111, 1);
    assert!(fixture.select(CHILD, "child") > 1);
    let selected = fixture.selected("child");
    assert_eq!(selected.len(), 111);
    for entry in &selected[..110] {
        assert_eq!(entry.containing_agent, agent("child"));
        assert_eq!(entry.entry_ref.scope(), &agent("child"));
        assert_eq!(
            entry.original_owner,
            Some(OwnerRef::Agent {
                agent: agent("parent")
            })
        );
        assert!(
            matches!(&entry.content[0],HistoryContent::Message(revision) if revision.scope()==&agent("parent"))
        );
    }
    assert!(selected.last().unwrap().original_owner.is_none());
}

#[test]
fn chained_physical_bases_keep_each_frozen_cut_and_exclude_abandoned_suffixes() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "parent", None, 0, 4);
    let base = fixture.base(ROOT, 3);
    fixture.native(REVERT, "parent", Some(base), 3, 3);
    let base = fixture.base(REVERT, 2);
    fixture.native(CHILD, "child", Some(base), 5, 1);
    fixture.select(CHILD, "child");
    assert_eq!(
        fixture
            .archive
            .history_entries
            .values()
            .filter(|entry| entry.containing_agent == agent("child"))
            .count(),
        4,
        "abandoned suffixes must not be copied into the child archive"
    );
    let selected = fixture.selected("child");
    assert_eq!(selected.len(), 4);
    assert_eq!(
        selected
            .iter()
            .filter(|entry| entry.original_owner.is_some())
            .count(),
        3
    );
}

#[test]
fn mismatched_prefix_bytes_or_ordinals_never_publish_a_plausible_selection() {
    for defect in ["byte", "ordinal", "missing-ordinal"] {
        let mut fixture = Fixture::default();
        fixture.native(ROOT, "parent", None, 0, 3);
        let mut base = fixture.base(ROOT, 3);
        if defect == "byte" {
            base["end_byte_offset"] = serde_json::json!(17);
        }
        if defect == "ordinal" {
            base["end_ordinal_exclusive"] = serde_json::json!(4);
        }
        fixture.native(CHILD, "child", Some(base), 3, 1);
        if defect == "missing-ordinal" {
            fixture
                .archive
                .history_records
                .get_mut(&(source(ROOT), 0))
                .unwrap()
                .get_mut(&1)
                .unwrap()
                .vendor_ordinal = None;
        }
        let output = fixture.map(CHILD, "child", &MapperState::default());
        assert!(!output.diagnostics.is_empty());
        assert!(
            output
                .claims
                .iter()
                .all(|claim| !matches!(claim.fact, Fact::SelectHistory { .. })),
            "{defect}"
        );
    }
}

#[test]
fn missing_or_ambiguous_physical_bindings_and_cycles_remain_explicit() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "parent", None, 0, 1);
    let base = fixture.base(ROOT, 2);
    fixture.native(CHILD, "child", Some(base), 2, 1);
    let definition = fixture
        .archive
        .source_definitions
        .remove(&(source(ROOT), 0))
        .unwrap();
    let missing = fixture.map(CHILD, "child", &MapperState::default());
    assert!(missing
        .requests
        .iter()
        .any(|request| matches!(request, ArchiveRequest::HistoryLocator { .. })));
    fixture
        .archive
        .source_definitions
        .insert((source(ROOT), 0), definition.clone());
    let mut duplicate = definition;
    duplicate.definition.source = source("duplicate");
    fixture
        .archive
        .source_definitions
        .insert((source("duplicate"), 0), duplicate);
    let ambiguous = fixture.map(CHILD, "child", &MapperState::default());
    assert_eq!(
        ambiguous.diagnostics[0].code,
        DiagnosticCode::ConflictingIdentity
    );
    fixture
        .archive
        .source_definitions
        .remove(&(source("duplicate"), 0));
    let self_base = fixture.base(ROOT, 1);
    let header = fixture
        .archive
        .history_records
        .get_mut(&(source(ROOT), 0))
        .unwrap()
        .get_mut(&0)
        .unwrap()
        .header
        .as_mut()
        .unwrap();
    header.history_base = Some(self_base);
    // A cycle is rejected even if its fabricated ordinal baseline is internally consistent.
    let cycle = fixture.map(CHILD, "child", &MapperState::default());
    assert_eq!(cycle.diagnostics[0].code, DiagnosticCode::CyclicHistory);
    assert!(cycle.claims.is_empty());
}

#[test]
fn an_unmapped_prefix_never_becomes_a_complete_inherited_selection() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "parent", None, 0, 2);
    let base = fixture.base(ROOT, 3);
    fixture.native(CHILD, "child", Some(base), 3, 1);
    fixture
        .archive
        .source_prefixes
        .get_mut(&source(ROOT))
        .unwrap()
        .retain(|extent| {
            extent.coordinate != HistoryCoordinate::LocalRecordIndex || extent.start_inclusive != 1
        });
    let output = fixture.map(CHILD, "child", &MapperState::default());
    assert!(output.claims.is_empty());
    assert_eq!(output.diagnostics[0].code, DiagnosticCode::CaptureGap);
    assert!(!output.requests.is_empty());
}

#[test]
fn standalone_legacy_history_uses_lexical_coordinates_without_inventing_vendor_ordinals() {
    let mut fixture = Fixture::default();
    fixture.native(ROOT, "parent", None, 0, 2);
    for record in fixture
        .archive
        .history_records
        .get_mut(&(source(ROOT), 0))
        .unwrap()
        .values_mut()
    {
        record.vendor_ordinal = None;
    }
    fixture.select(ROOT, "parent");
    let selected = fixture.selected("parent");
    assert_eq!(selected.len(), 2);
}

#[test]
fn history_base_requires_its_declared_rollout_filename_contract() {
    assert!(physical_id(
        "/vendor/rollout-2026-09-16T00-00-00-11111111-1111-4111-8111-111111111111.jsonl"
    )
    .is_some());
    assert!(physical_id("/vendor/random-11111111-1111-4111-8111-111111111111.jsonl").is_none());
    assert!(physical_id("/vendor/rollout-2026-09-16T00-00-00-not-a-uuid.jsonl").is_none());
}
