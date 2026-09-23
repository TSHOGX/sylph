use super::*;
use crate::mapping::{RecordedSource, inventory::InventoryMapper};

const ROOT: &str = "9074a8da-e264-4e3a-87c3-95211e986faf";
const RUN: &str = "wf_8078fad7-6fc";
const JOURNAL_PATH: &str = "/workspace/claude/projects/9074a8da-e264-4e3a-87c3-95211e986faf/subagents/workflows/wf_8078fad7-6fc/journal.jsonl";
const ROOT_BYTES: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/workflow/root.jsonl");
const MANIFEST: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/workflow/manifest.json");
const JOURNAL: &str =
    include_str!("../../../../../../fixtures/unified-trace/claude/workflow/journal.jsonl");

#[derive(Default)]
struct Harness {
    archive: ArchiveContext,
    states: BTreeMap<String, MapperState>,
    ordinals: BTreeMap<String, u64>,
    output: Vec<MapOutput>,
}
impl Harness {
    fn feed(&mut self, source: &str, kind: TransportKind, value: Value) -> &MapOutput {
        let ordinal = self.ordinals.entry(source.into()).or_default();
        let mut capture = capture(*ordinal, false);
        capture.observation_ref.producer_id = source.into();
        capture.source_position.source.instance = source.into();
        capture.transport_kind = kind;
        capture.declared_subject_refs = vec![owner(ROOT)];
        capture.vendor_build = Some("2.1.238".into());
        *ordinal += 1;
        let state = self.states.entry(source.into()).or_default();
        let raw = serde_json::to_vec(&value).unwrap();
        let input = MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state,
            archive: &self.archive,
        };
        let output = if kind == TransportKind::SourceInventory {
            InventoryMapper.map(input)
        } else {
            ClaudeMapper.map(input)
        };
        *state = output.state.clone();
        self.archive
            .observations
            .insert(capture.observation_ref.clone());
        self.archive
            .observation_sources
            .insert(capture.observation_ref, capture.source_position);
        for item in &output.source_owners {
            self.archive
                .source_owners
                .insert((item.source.clone(), item.generation), item.owner.clone());
        }
        for item in &output.source_definitions {
            self.archive.source_definitions.insert(
                (item.definition.source.clone(), item.definition.generation),
                item.clone(),
            );
        }
        self.archive
            .correlations
            .extend(output.correlations.clone());
        self.archive
            .extend_correlation_evidence(output.correlation_evidence.clone());
        self.output.push(output);
        self.output.last().unwrap()
    }
    fn lines(&mut self, source: &str, raw: &str) {
        for line in raw.lines() {
            self.feed(
                source,
                TransportKind::NativeJsonl,
                serde_json::from_str(line).unwrap(),
            );
        }
    }
    fn inventory(&mut self, source: &str, locator: &str) {
        let issuer = capture(0, false).observation_ref.issuer;
        self.feed(
            &format!("inventory-{source}"),
            TransportKind::SourceInventory,
            serde_json::to_value(SourceDefinition {
                source: SourceRef {
                    issuer: issuer.clone(),
                    instance: source.into(),
                },
                generation: 0,
                locator: locator.into(),
                owner: owner(ROOT),
                basis: SourceOwnerBasis::CompanionTranscript {
                    source: SourceRef {
                        issuer,
                        instance: "root".into(),
                    },
                    generation: 0,
                },
            })
            .unwrap(),
        );
    }
    fn bundle() -> Self {
        let mut harness = Self::default();
        harness.lines("root", ROOT_BYTES);
        harness.inventory("manifest", "/workspace/claude/projects/workflow.json");
        harness.inventory("journal", JOURNAL_PATH);
        harness
    }
}

#[test]
fn recorded_workflow_keeps_one_host_two_real_children_and_independent_invocation_results() {
    let mut harness = Harness::bundle();
    harness.feed(
        "manifest",
        TransportKind::NativeSnapshot,
        serde_json::from_str(MANIFEST).unwrap(),
    );
    harness.lines("journal", JOURNAL);
    harness.lines("child-a", include_str!("../../../../../../fixtures/unified-trace/claude/workflow/agent-ab4e2619af6457242.jsonl"));
    harness.lines("child-b", include_str!("../../../../../../fixtures/unified-trace/claude/workflow/agent-aa28b7d5605fea7e9.jsonl"));
    let reduced = reduce(&harness.output);
    let agents: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Agent(agent)) => Some(&agent.agent_ref),
            _ => None,
        })
        .collect();
    assert_eq!(agents.len(), 3);
    for id in ["ab4e2619af6457242", "aa28b7d5605fea7e9"] {
        assert!(agents.contains(&&Context::child(&owner(ROOT), id)));
    }
    let resources: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Resource(resource)) => Some(resource),
            _ => None,
        })
        .collect();
    assert_eq!(resources.len(), 3);
    let host = ResourceRef::native(owner(ROOT), "claude.workflow", vec![RUN.into()]).unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Resource(host.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let host_execution = reduced
        .execution(&AnyObjectRef::Resource(host.clone()))
        .unwrap();
    assert!(host_execution
        .boundaries
        .iter()
        .any(|(boundary, _)| matches!(
            boundary,
            ExecutionBoundary::Started {
                time: Some(TimeEvidence {
                    value: 1787646738475,
                    unit: TimeUnit::Milliseconds,
                    basis: TimeBasis::SourceWall,
                    ..
                })
            }
        )));
    assert!(host_execution
        .boundaries
        .iter()
        .all(|(boundary, _)| !matches!(
            boundary,
            ExecutionBoundary::Terminated { time: Some(_), .. }
        )));
    let invocations: Vec<_> = reduced.executions.iter().filter(|(id,_)|matches!(id,AnyObjectRef::Operation(op) if matches!(op.address(),ObjectAddress::Native{namespace,..} if namespace=="claude.workflow-invocation"))).collect();
    assert_eq!(invocations.len(), 2);
    assert!(invocations
        .iter()
        .all(|(_, execution)| execution.started.len() == 1
            && !execution.terminated.is_empty()
            && execution.outcome.known() == Some(&TerminalOutcome::Succeeded)));
    assert!(!objects(&harness.output).any(|object| matches!(object, TraceObject::Activation(_))));
    let payloads: Vec<_> = harness
        .output
        .iter()
        .flat_map(|output| &output.payloads)
        .map(|payload| payload.bytes.as_slice())
        .collect();
    for bytes in [b"\"hello\"".as_slice(), b"\"world\"", b"hello", b"world"] {
        assert!(payloads.contains(&bytes));
    }
    let measurements: Vec<_> = objects(&harness.output)
        .filter_map(|object| match object {
            TraceObject::Measurement(measurement) if measurement.metric == Metric::TotalTokens => {
                Some(measurement)
            }
            _ => None,
        })
        .collect();
    assert_eq!(measurements.len(), 3);
    assert!(measurements
        .iter()
        .any(|m| m.subject == AnyObjectRef::Resource(host.clone())
            && m.value.as_u64() == Some(58563)));
    assert_eq!(
        measurements
            .iter()
            .map(|m| m.subject.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert!(harness
        .output
        .iter()
        .all(|out| out.requests.is_empty() && out.diagnostics.is_empty()));
}

#[test]
fn failed_workflow_and_queued_progress_never_finish_an_unreturned_invocation_or_invent_a_child() {
    let mut harness = Harness::bundle();
    harness.lines("journal", JOURNAL.lines().next().unwrap());
    let mut manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    manifest["status"] = "failed".into();
    manifest["agentCount"] = 999.into();
    manifest["workflowProgress"][1]["state"] = "error".into();
    manifest["workflowProgress"].as_array_mut().unwrap().push(serde_json::json!({"type":"workflow_agent","index":3,"state":"start","label":"not yet created"}));
    harness.feed("manifest", TransportKind::NativeSnapshot, manifest);
    let reduced = reduce(&harness.output);
    let host = ResourceRef::native(owner(ROOT), "claude.workflow", vec![RUN.into()]).unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Resource(host))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    let journal=reduced.executions.iter().find(|(id,_)|matches!(id,AnyObjectRef::Operation(op) if matches!(op.address(),ObjectAddress::Native{namespace,..}if namespace=="claude.workflow-invocation"))).unwrap().1;
    assert_eq!(journal.started.len(), 1);
    assert!(journal.terminated.is_empty());
    assert!(journal.outcome.known().is_none());
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|o| matches!(o.declaration.known(), Some(TraceObject::Agent(_))))
            .count(),
        3
    );
    assert!(!objects(&harness.output).any(|o| matches!(o, TraceObject::Activation(_))));
    let second = ResourceRef::native(
        owner(ROOT),
        "claude.workflow-activity",
        vec![RUN.into(), "2".into(), "1".into()],
    )
    .unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Resource(second))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
}

#[test]
fn workflow_joins_require_retained_owner_launch_and_exact_journal_locator() {
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    let mut harness = Harness::default();
    let orphan = harness.feed("manifest", TransportKind::NativeSnapshot, manifest.clone());
    assert!(
        orphan.claims.is_empty(),
        "routing subjects cannot mint workflow ownership"
    );
    assert!(!orphan.requests.is_empty());
    harness.lines("root", ROOT_BYTES.lines().next().unwrap());
    harness.inventory("manifest", "/captured/manifest.json");
    let unlaunched = harness.feed("manifest", TransportKind::NativeSnapshot, manifest.clone());
    assert!(unlaunched.claims.is_empty());
    assert!(unlaunched.requests.iter().any(|request|matches!(request,ArchiveRequest::Correlation{namespace,..}if namespace=="claude.workflow-run")));
    harness.lines("root", ROOT_BYTES.lines().nth(1).unwrap());
    harness.feed("manifest", TransportKind::NativeSnapshot, manifest);
    harness.inventory("journal", "/wrong/workflow/journal.jsonl");
    let unmatched = harness.feed(
        "journal",
        TransportKind::NativeJsonl,
        serde_json::from_str(JOURNAL.lines().next().unwrap()).unwrap(),
    );
    assert!(unmatched.claims.is_empty());
    assert!(unmatched.requests.iter().any(|request|matches!(request,ArchiveRequest::Correlation{namespace,..}if namespace=="claude.workflow-journal")));
    let source = SourceRef {
        issuer: capture(0, false).observation_ref.issuer,
        instance: "journal".into(),
    };
    let RecordedSource { definition, .. } = harness
        .archive
        .source_definitions
        .get_mut(&(source, 0))
        .unwrap();
    definition.locator = JOURNAL_PATH.into();
    let mut missing: Value = serde_json::from_str(JOURNAL.lines().nth(2).unwrap()).unwrap();
    missing.as_object_mut().unwrap().remove("result");
    let output = harness.feed("journal", TransportKind::NativeJsonl, missing);
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic} if diagnostic.code==DiagnosticCode::DecodeFailure)));
    assert!(!output.claims.iter().any(|claim| matches!(
        claim.fact,
        Fact::Execution {
            boundary: ExecutionBoundary::Outcome { .. },
            ..
        }
    )));
}
