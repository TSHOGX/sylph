use super::*;

const SESSION: &str = "e9b9839b-607e-4acd-b5a8-f4b1d8c3b38f";
const CHILD: &str = "a834a416191ccc0e0";
const NATIVE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/delegation/native.jsonl");
const LIVE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/delegation/stdout.jsonl");
const CHILD_NATIVE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/delegation/child.jsonl");
const META: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/claude/delegation/child.meta.json");
fn root() -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:claude:{SESSION}")).unwrap())
}
fn child() -> AgentRef {
    AgentRef::Embedded {
        anchor_session: root().anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: CHILD.into(),
    }
}
fn source(name: &str) -> SourceRef {
    SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        instance: name.into(),
    }
}
struct Record {
    source: String,
    ordinal: u64,
    offset: u64,
    kind: TransportKind,
    raw: Vec<u8>,
}
fn records(order: SourceOrder) -> Vec<Record> {
    let mut groups = vec![];
    for (name, kind, raw) in [
        ("root", TransportKind::NativeJsonl, NATIVE),
        ("child", TransportKind::NativeJsonl, CHILD_NATIVE),
        ("sdk", TransportKind::LiveStdout, LIVE),
        ("metadata", TransportKind::NativeSnapshot, META),
    ] {
        let mut offset = 0;
        groups.push(
            raw.split_inclusive(|byte| *byte == b'\n')
                .enumerate()
                .map(|(index, raw)| {
                    let record = Record {
                        source: name.into(),
                        ordinal: index as u64,
                        offset,
                        kind,
                        raw: raw.to_vec(),
                    };
                    offset += raw.len() as u64;
                    record
                })
                .collect::<Vec<_>>(),
        );
    }
    let definition = SourceDefinition {
        source: source("metadata"),
        generation: 0,
        locator: format!("/workspace/claude-capture/subagents/agent-{CHILD}.meta.json"),
        owner: child(),
        basis: SourceOwnerBasis::CompanionTranscript {
            source: source("child"),
            generation: 0,
        },
    };
    groups.push(vec![Record {
        source: "inventory".into(),
        ordinal: 0,
        offset: 0,
        kind: TransportKind::SourceInventory,
        raw: serde_json::to_vec(&definition).unwrap(),
    }]);
    match order {
        SourceOrder::NativeFirst => groups.into_iter().flatten().collect(),
        SourceOrder::LiveFirst => groups.into_iter().rev().flatten().collect(),
        SourceOrder::Alternating => {
            let mut groups: Vec<_> = groups.into_iter().map(Vec::into_iter).collect();
            let mut all = vec![];
            loop {
                let next: Vec<_> = groups.iter_mut().filter_map(Iterator::next).collect();
                if next.is_empty() {
                    break;
                }
                all.extend(next);
            }
            all
        }
    }
}
async fn expected(fixture: &Fixture, store: &GenerationStore) {
    let reduced = reduction(&claims(store).await);
    let objects: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|value| value.declaration.known())
        .collect();
    let agents: BTreeSet<_> = objects
        .iter()
        .filter_map(|value| match value {
            TraceObject::Agent(value) => Some(value.agent_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, [root(), child()].into_iter().collect());
    let tasks: Vec<_> = objects.iter().filter_map(|value| match value { TraceObject::Resource(value) if matches!(&value.kind, ResourceKind::Extension { namespace, name } if namespace == "claude.task" && name == "local_agent") => Some(value), _ => None }).collect();
    assert_eq!(tasks.len(), 2);
    for task in &tasks {
        let states = &reduced.resource_states[&task.resource_ref];
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| *state == ResourceState::Running)
                .count(),
            1
        );
        assert_eq!(
            states
                .iter()
                .filter(|(state, _)| matches!(
                    state,
                    ResourceState::Closed {
                        outcome: TerminalOutcome::Succeeded,
                        ..
                    }
                ))
                .count(),
            2
        );
    }
    let invocation =
        OperationRef::native(root(), "claude.tool-call", vec!["toolu_delegate_2".into()]).unwrap();
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(invocation))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let starts: BTreeSet<_> = reduced
        .relations
        .iter()
        .filter_map(|value| match &value.relation {
            Relation::Spawn {
                parent,
                child,
                operation,
            } => Some((parent.clone(), child.clone(), operation.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts.len(),
        1,
        "resuming the same child is not another spawn"
    );
    for (id, marker) in [
        (
            "6d1c6750-13e9-4dd9-b5ee-d130917407f7",
            b"SYLPH_CLAUDE_CHILD_1".as_slice(),
        ),
        (
            "8b9d3e11-ad09-4d75-907a-1760d0aa0232",
            b"SYLPH_CLAUDE_CHILD_2".as_slice(),
        ),
    ] {
        let message = MessageRef::native(child(), "claude.record", vec![id.into()]).unwrap();
        let bodies = revision_bytes(fixture, store, &message).await;
        assert!(!bodies.is_empty());
        assert!(bodies.iter().all(|body| body == marker));
    }
    let measurements: Vec<_> = objects
        .iter()
        .filter_map(|value| match value {
            TraceObject::Measurement(value)
                if matches!(value.subject, AnyObjectRef::Resource(_)) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        measurements
            .iter()
            .filter(|value| value.metric == Metric::TotalTokens && value.value.as_i64() == Some(6))
            .count(),
        2
    );
    assert!(
        measurements
            .iter()
            .filter(|value| value.metric == Metric::ReportedDuration)
            .all(|value| value.unit == MeasurementUnit::Milliseconds)
    );
    let mut durations: Vec<_> = measurements
        .iter()
        .filter(|value| value.metric == Metric::ReportedDuration)
        .map(|value| value.value.as_i64().unwrap())
        .collect();
    durations.sort();
    assert_eq!(durations, [13, 34]);
}
async fn replay(order: SourceOrder, incremental: bool) -> Projection {
    let fixture = Fixture::for_vendor("claude").await;
    let (mut store, _) = fixture.generation("delegation").await;
    let mut captures = vec![];
    for record in records(order) {
        let src = source(&record.source);
        let capture = fixture
            .admit(
                ObservationRef {
                    issuer: src.issuer.clone(),
                    producer_id: record.source.clone(),
                    producer_epoch: "retained".into(),
                    producer_seq: record.ordinal + 1,
                },
                SourcePosition {
                    source: src,
                    generation: 0,
                    locator: if record.kind == TransportKind::NativeJsonl {
                        SourceLocator::Jsonl {
                            byte_start: record.offset,
                            byte_end: record.offset + record.raw.len() as u64,
                            record_ordinal: record.ordinal,
                        }
                    } else {
                        SourceLocator::Frame {
                            ordinal: record.ordinal,
                        }
                    },
                },
                &record.raw,
                record.kind,
            )
            .await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        captures.push(capture);
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + record.ordinal as usize % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if record.ordinal % 5 == 0 {
                store = reopened(store).await;
            }
        }
    }
    for mut capture in captures {
        capture.observation_ref.producer_id.push_str("-reread");
        fixture
            .evidence
            .admit_batch(&capture.observation_ref.issuer.clone(), &[capture])
            .await
            .unwrap();
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("delegation-rebuild").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    expected(&fixture, &fresh).await;
    assert_projection(&projection(&fixture, &fresh).await, &before);
    Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    }
}
#[tokio::test]
async fn recorded_claude_child_reuse_keeps_episode_states_and_answers_through_replay() {
    let cold = replay(SourceOrder::NativeFirst, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true).await, &cold);
}
