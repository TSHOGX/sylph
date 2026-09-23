use super::*;
use sha2::{Digest, Sha256};

const SESSION: &str = "9074a8da-e264-4e3a-87c3-95211e986faf";
const RUN: &str = "wf_8078fad7-6fc";
const JOURNAL_PATH: &str = "/workspace/claude/projects/9074a8da-e264-4e3a-87c3-95211e986faf/subagents/workflows/wf_8078fad7-6fc/journal.jsonl";
const ROOT: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/workflow/root.jsonl");
const MANIFEST: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/workflow/manifest.json");
const JOURNAL: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/workflow/journal.jsonl");
const CHILD_A: &[u8] = include_bytes!(
    "../../../../../../../fixtures/unified-trace/claude/workflow/agent-ab4e2619af6457242.jsonl"
);
const CHILD_B: &[u8] = include_bytes!(
    "../../../../../../../fixtures/unified-trace/claude/workflow/agent-aa28b7d5605fea7e9.jsonl"
);

const FAILED_ROOT: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/workflow/failed-root.jsonl");
const FAILED_MANIFEST: &[u8] = include_bytes!(
    "../../../../../../../fixtures/unified-trace/claude/workflow/failed-manifest.json"
);
const FAILED_JOURNAL: &[u8] = include_bytes!(
    "../../../../../../../fixtures/unified-trace/claude/workflow/failed-journal.jsonl"
);
const FAILED_PATH: &str = "/workspace/claude/projects/f0795606-660f-4d7d-9146-448198093b59/subagents/workflows/wf_39a54a67-df2/journal.jsonl";
fn failed_root() -> AgentRef {
    AgentRef::Session(
        SessionId::decode("env7fk2:claude:f0795606-660f-4d7d-9146-448198093b59").unwrap(),
    )
}

fn root() -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:claude:{SESSION}")).unwrap())
}
fn child(id: &str) -> AgentRef {
    AgentRef::Embedded {
        anchor_session: root().anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: id.into(),
    }
}
fn source(name: &str) -> SourceRef {
    SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        instance: name.into(),
    }
}

struct WorkflowRecord {
    source: String,
    ordinal: u64,
    offset: u64,
    kind: TransportKind,
    raw: Vec<u8>,
}
fn groups(failed: bool) -> Vec<Vec<WorkflowRecord>> {
    let mut groups = vec![];
    for (name, kind, raw) in [
        (
            "root",
            TransportKind::NativeJsonl,
            if failed { FAILED_ROOT } else { ROOT },
        ),
        (
            "manifest",
            TransportKind::NativeSnapshot,
            if failed { FAILED_MANIFEST } else { MANIFEST },
        ),
        (
            "journal",
            TransportKind::NativeJsonl,
            if failed { FAILED_JOURNAL } else { JOURNAL },
        ),
        (
            "child-a",
            TransportKind::NativeJsonl,
            if failed { b"" } else { CHILD_A },
        ),
        (
            "child-b",
            TransportKind::NativeJsonl,
            if failed { b"" } else { CHILD_B },
        ),
    ] {
        let mut offset = 0;
        groups.push(
            raw.split_inclusive(|byte| *byte == b'\n')
                .enumerate()
                .map(|(i, line)| {
                    let record = WorkflowRecord {
                        source: name.into(),
                        ordinal: i as u64,
                        offset,
                        kind,
                        raw: line.to_vec(),
                    };
                    offset += line.len() as u64;
                    record
                })
                .collect(),
        );
    }
    for (name, locator) in [
        ("manifest", "/workspace/claude/projects/workflow.json"),
        ("journal", if failed { FAILED_PATH } else { JOURNAL_PATH }),
    ] {
        let definition = SourceDefinition {
            source: source(name),
            generation: 0,
            locator: locator.into(),
            owner: if failed { failed_root() } else { root() },
            basis: SourceOwnerBasis::CompanionTranscript {
                source: source("root"),
                generation: 0,
            },
        };
        groups.push(vec![WorkflowRecord {
            source: format!("inventory-{name}"),
            ordinal: 0,
            offset: 0,
            kind: TransportKind::SourceInventory,
            raw: serde_json::to_vec(&definition).unwrap(),
        }]);
    }
    groups
}
fn records(order: SourceOrder, failed: bool) -> Vec<WorkflowRecord> {
    let mut groups = groups(failed);
    match order {
        SourceOrder::NativeFirst => groups.into_iter().flatten().collect(),
        SourceOrder::LiveFirst => {
            groups.reverse();
            groups.into_iter().flatten().collect()
        }
        SourceOrder::Alternating => {
            let mut groups: Vec<_> = groups.into_iter().map(Vec::into_iter).collect();
            let mut records = vec![];
            loop {
                let before = records.len();
                for group in &mut groups {
                    records.extend(group.next());
                }
                if before == records.len() {
                    break;
                }
            }
            records
        }
    }
}
async fn admit(fixture: &Fixture, record: &WorkflowRecord) -> Capture {
    fixture
        .admit(
            ObservationRef {
                issuer: source(&record.source).issuer.clone(),
                producer_id: record.source.clone(),
                producer_epoch: "workflow-capture".into(),
                producer_seq: record.ordinal + 1,
            },
            SourcePosition {
                source: source(&record.source),
                generation: 0,
                locator: match record.kind {
                    TransportKind::NativeJsonl => SourceLocator::Jsonl {
                        byte_start: record.offset,
                        byte_end: record.offset + record.raw.len() as u64,
                        record_ordinal: record.ordinal,
                    },
                    TransportKind::NativeSnapshot => SourceLocator::Snapshot {
                        revision: "observed-manifest".into(),
                    },
                    _ => SourceLocator::Frame {
                        ordinal: record.ordinal,
                    },
                },
            },
            &record.raw,
            record.kind,
        )
        .await
}

async fn expected(fixture: &Fixture, store: &GenerationStore, failed: bool) {
    if failed {
        expected_failure(fixture, store).await;
        return;
    }
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|object| matches!(object.declaration.known(), Some(TraceObject::Agent(_))))
            .count(),
        3
    );
    assert!(!reduced
        .objects
        .values()
        .any(|object| matches!(object.declaration.known(), Some(TraceObject::Activation(_)))));
    let host = ResourceRef::native(root(), "claude.workflow", vec![RUN.into()]).unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Resource(host.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    assert!(reduced
        .execution(&AnyObjectRef::Resource(host.clone()))
        .unwrap()
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
    let invocation_results:Vec<_>=reduced.objects.values().filter_map(|object|match object.declaration.known(){
        Some(TraceObject::Result(result)) if matches!(result.operation.address(),ObjectAddress::Native{namespace,..}if namespace=="claude.workflow-invocation")=>Some(result),_=>None}).collect();
    assert_eq!(invocation_results.len(), 2);
    let mut answers = vec![];
    for result in invocation_results {
        assert_eq!(result.representation, ResultRepresentation::Runtime);
        let state = reduced
            .execution(&AnyObjectRef::Operation(result.operation.clone()))
            .unwrap();
        assert!(!state.started.is_empty() && !state.terminated.is_empty());
        assert_eq!(state.outcome.known(), Some(&TerminalOutcome::Succeeded));
        answers.push(
            fixture
                .evidence
                .payload_bytes(result.payload.as_ref().unwrap(), 1024)
                .await
                .unwrap(),
        );
    }
    answers.sort();
    assert_eq!(answers, [b"\"hello\"".to_vec(), b"\"world\"".to_vec()]);
    for (id, raw, answer) in [
        ("ab4e2619af6457242", CHILD_A, b"hello".as_slice()),
        ("aa28b7d5605fea7e9", CHILD_B, b"world".as_slice()),
    ] {
        let last: serde_json::Value = serde_json::from_slice(
            raw.split(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .unwrap(),
        )
        .unwrap();
        let message = MessageRef::native(
            child(id),
            "claude.record",
            vec![last["uuid"].as_str().unwrap().into()],
        )
        .unwrap();
        let bodies = revision_bytes(fixture, store, &message).await;
        assert!(!bodies.is_empty());
        assert!(bodies.iter().all(|body| body == answer));
    }
    let measurements: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Measurement(value)) if value.metric == Metric::TotalTokens => {
                Some(value)
            }
            _ => None,
        })
        .collect();
    assert_eq!(measurements.len(), 3);
    assert!(measurements
        .iter()
        .any(|m| m.subject == AnyObjectRef::Resource(host.clone())
            && m.value.as_u64() == Some(58563)));
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    for agent in [
        root(),
        child("ab4e2619af6457242"),
        child("aa28b7d5605fea7e9"),
    ] {
        let header = store
            .read_page(
                &snapshot,
                &read_scope(&agent, None, "header"),
                None,
                None,
                None,
                2,
                false,
            )
            .await
            .unwrap();
        assert_eq!(header.rows.len(), 1);
        let activations = store
            .read_page(
                &snapshot,
                &read_scope(&agent, None, "activations"),
                None,
                None,
                None,
                2,
                false,
            )
            .await
            .unwrap();
        assert!(activations.rows.is_empty());
    }
}

async fn replay(order: SourceOrder, incremental: bool, failed: bool) -> Projection {
    let fixture = Fixture::for_vendor("claude").await;
    let (mut store, _) = fixture.generation("claude-workflow").await;
    for (index, record) in records(order, failed).iter().enumerate() {
        let capture = admit(&fixture, record).await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + index % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if index % 3 == 0 {
                store = reopened(store).await;
            }
        }
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store, failed).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("claude-workflow-rebuilt").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(&fixture, &fresh, failed).await;
    super::assert_projection(&projection(&fixture, &fresh).await, &before);
    Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    }
}

#[tokio::test]
async fn retained_workflow_replays_early_manifests_and_journals_through_late_joins_restart_and_rebuild(
) {
    for (raw, digest) in [
        (
            FAILED_ROOT,
            "cb14893e77aa93f928122969eb83900597931d380812d76521a70c5c6c8d5512",
        ),
        (
            FAILED_MANIFEST,
            "191b775ac8666a14d1ea72c5e931f4c08226a5d09e9b76bdfdde8fd6a92eec22",
        ),
        (
            FAILED_JOURNAL,
            "b3075d4cee418a51d04f929abb5613766c7128d3a8cf689a3706769035f271b9",
        ),
        (
            ROOT,
            "dc34f128b5c5c4a8ccfd7f3564d2369f35a1f58e5698a07d41dce6bea0792a59",
        ),
        (
            MANIFEST,
            "71b76c40a74b8d60a4f193acc98001982403a6a0d3baa4752ae1f9f5e9ce3c7c",
        ),
        (
            JOURNAL,
            "fdb33fe3445518003b1d8626a0a75eb1c21f8094ef07081e2a253fb20881e0f8",
        ),
        (
            CHILD_A,
            "9e7b1eb7d23e75080b973198e55988bc6746abd01485dc3da5cd77e1c847e0e7",
        ),
        (
            CHILD_B,
            "7e6aa709b2ace2634a68a359397a11b59b2fdd84d9846c6bae9ccc392fce766d",
        ),
    ] {
        assert_eq!(format!("{:x}", Sha256::digest(raw)), digest);
    }
    for failed in [false, true] {
        let cold = replay(SourceOrder::NativeFirst, false, failed).await;
        super::assert_projection(
            &replay(SourceOrder::LiveFirst, true, failed).await,
            &cold,
        );
        super::assert_projection(
            &replay(SourceOrder::Alternating, true, failed).await,
            &cold,
        );
    }
}

async fn expected_failure(fixture: &Fixture, store: &GenerationStore) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let host = ResourceRef::native(
        failed_root(),
        "claude.workflow",
        vec!["wf_39a54a67-df2".into()],
    )
    .unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Resource(host))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    let invocations: Vec<_> = reduced.executions.iter().filter(|(id,_)| matches!(id,AnyObjectRef::Operation(op) if matches!(op.address(),ObjectAddress::Native{namespace,..}if namespace=="claude.workflow-invocation"))).collect();
    assert_eq!(invocations.len(), 2);
    assert!(invocations
        .iter()
        .all(|(_, state)| !state.started.is_empty()
            && state.terminated.is_empty()
            && state.outcome.known().is_none()));
    assert!(!reduced
        .objects
        .values()
        .any(|object| matches!(object.declaration.known(), Some(TraceObject::Activation(_)))));
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|object| matches!(object.declaration.known(), Some(TraceObject::Agent(_))))
            .count(),
        3
    );
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    for id in ["aee4f29ec3fde665b", "a497b71445bde2908"] {
        let child = AgentRef::Embedded {
            anchor_session: failed_root().anchor_session().clone(),
            namespace: "claude.agent".into(),
            native_child_id: id.into(),
        };
        assert!(reduced
            .object(&AnyObjectRef::Agent(child.clone()))
            .is_some());
        let page = store
            .read_page(
                &snapshot,
                &read_scope(&child, None, "header"),
                None,
                None,
                None,
                2,
                false,
            )
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
    }
    let runtime: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Result(result))
                if result.representation == ResultRepresentation::Runtime =>
            {
                Some(result)
            }
            _ => None,
        })
        .collect();
    let mut retained_error = false;
    for result in runtime {
        let bytes = fixture
            .evidence
            .payload_bytes(result.payload.as_ref().unwrap(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if value.get("error").is_some() {
            assert!(value["error"]
                .as_str()
                .unwrap()
                .starts_with("TypeError: parallel() expects an array of functions"));
            assert_eq!(value["result"], serde_json::Value::Null);
            retained_error = true;
        }
    }
    assert!(retained_error);
}
