use super::*;
use sha2::{Digest, Sha256};

const CORPUS: Corpus = Corpus {
    vendor: "claude",
    session: "87a03655-be7b-432b-bca4-5cba20392b03",
    native: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/schedule/native.jsonl"
    ),
    live: include_bytes!(
        "../../../../../../../fixtures/unified-trace/claude/schedule/stdout.jsonl"
    ),
    namespace: "claude.record",
    question_id: "a8e854c7-2de9-433e-9d6c-c28fcfb53d9a",
    answer_id: "15fbcde4-8735-40cc-adb5-680333b4a692",
    question: b"Run the controlled local fixture tool and return the marker.",
    answer: b"SYLPH_CLAUDE_TOOL_OK",
};
fn operation(id: &str) -> OperationRef {
    OperationRef::native(CORPUS.owner(), "claude.tool-call", vec![id.into()]).unwrap()
}
fn schedule() -> ResourceRef {
    ResourceRef::native(CORPUS.owner(), "claude.schedule", vec!["9b30bd4d".into()]).unwrap()
}
async fn expected(fixture: &Fixture, store: &GenerationStore) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let resources: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|o| match o.declaration.known() {
            Some(TraceObject::Resource(resource)) if resource.kind == ResourceKind::Schedule => {
                Some(resource)
            }
            _ => None,
        })
        .collect();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].resource_ref, schedule());
    assert_eq!(resources[0].creator, Some(operation("toolu_schedule_1")));
    assert!(resources[0].host.is_none());
    let states = &reduced.resource_states[&schedule()];
    assert_eq!(states.len(), 6);
    assert_eq!(
        states
            .iter()
            .filter(|(state, _)| matches!(state, ResourceState::ScheduleRegistered { .. }))
            .count(),
        4
    );
    assert_eq!(
        states
            .iter()
            .filter(|(state, _)| matches!(state, ResourceState::ScheduleCancelled))
            .count(),
        2
    );
    assert!(!states
        .iter()
        .any(|(state, _)| matches!(state, ResourceState::ScheduleFired { .. })));
    assert!(reduced
        .execution(&AnyObjectRef::Resource(schedule()))
        .is_none());
    let creator = reduced
        .object(&AnyObjectRef::Operation(operation("toolu_schedule_1")))
        .unwrap()
        .declaration
        .known()
        .unwrap();
    let TraceObject::Operation(creator) = creator else {
        panic!("creation operation");
    };
    let bytes = fixture
        .evidence
        .payload_bytes(creator.input_revision.as_ref().unwrap(), 4096)
        .await
        .unwrap();
    let input: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(input["cron"], "0 0 1 1 *");
    assert_eq!(
        input["prompt"],
        "Controlled inert fixture; no external actions."
    );
    assert_eq!(input["durable"], false);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Operation(operation("toolu_schedule_5")))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Failed)
    );
    let mut saw_registration = false;
    let mut saw_listing = false;
    for (state, _) in states {
        if let ResourceState::ScheduleRegistered { registration } = state {
            let value: serde_json::Value = serde_json::from_slice(
                &fixture
                    .evidence
                    .payload_bytes(registration, 4096)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(value["id"], "9b30bd4d");
            assert_eq!(value["durable"], false);
            if value.get("prompt").is_some() {
                saw_listing = true;
                assert_eq!(
                    value["prompt"],
                    "Controlled inert fixture; no external actions."
                );
            } else {
                saw_registration = true;
                assert_eq!(value["recurring"], false);
            }
        }
    }
    assert!(saw_registration && saw_listing);
    for (id, expected) in [
        (CORPUS.question_id, CORPUS.question),
        (CORPUS.answer_id, CORPUS.answer),
    ] {
        let bytes = revision_bytes(fixture, store, &CORPUS.message(id)).await;
        assert!(!bytes.is_empty());
        assert!(bytes.iter().all(|bytes| bytes == expected));
    }
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let reference = AnyObjectRef::Resource(schedule());
    let row = store
        .read_row(
            &snapshot,
            &read_scope(&CORPUS.owner(), None, "objects"),
            &reference.encode(),
        )
        .await
        .unwrap();
    let detail: ReadObjectDetail = serde_json::from_value(row.body).unwrap();
    assert!(matches!(
        detail.content,
        ReadObjectContent::Resource {
            resource_kind: ResourceKind::Schedule,
            ..
        }
    ));
    let execution = detail.execution.as_ref().unwrap();
    assert_eq!(execution.state, ReadExecutionState::Unavailable);
    assert!(execution.durations.is_empty());
}
async fn replay(order: SourceOrder, incremental: bool) -> Projection {
    let fixture = Fixture::for_vendor("claude").await;
    let (mut store, _) = fixture.generation("claude-schedules").await;
    for (index, record) in records(&CORPUS, order).iter().enumerate() {
        let capture = admit_record(&fixture, record).await;
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
            if index % 5 == 0 {
                store = reopened(store).await;
            }
        }
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("claude-schedules-rebuilt").await;
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
async fn recorded_schedules_keep_registration_cancellation_and_failure_separate_across_replay() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.native)),
        "d88d5b71e73c0dcb81bebf73a5eead6fd9769bcf248c587056cc505e30898a7b"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.live)),
        "62d3fb9f968b9f3d0740de61514381552a1f01b833d0c536279f69f7092d0638"
    );
    let cold = replay(SourceOrder::NativeFirst, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true).await, &cold);
}

#[tokio::test]
async fn a_schedule_result_waits_for_its_retained_builtin_invocation_across_restart() {
    let fixture = Fixture::for_vendor("claude").await;
    let (mut store, _) = fixture.generation("late-schedule-call").await;
    let source_records = records(&CORPUS, SourceOrder::NativeFirst);
    let result = source_records
        .iter()
        .find(|record| {
            record.source == "corpus-native"
                && serde_json::from_slice::<serde_json::Value>(record.raw)
                    .unwrap()
                    .get("toolUseResult")
                    .is_some_and(|value| value["id"] == "9b30bd4d")
        })
        .unwrap();
    let invocation = source_records
        .iter()
        .find(|record| {
            record.source == "corpus-native"
                && serde_json::from_slice::<serde_json::Value>(record.raw)
                    .unwrap()
                    .pointer("/message/content/0/id")
                    .is_some_and(|value| value == "toolu_schedule_1")
        })
        .unwrap();
    for (name, record) in [("early-result", result), ("late-invocation", invocation)] {
        let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
        fixture
            .admit(
                ObservationRef {
                    issuer: issuer.clone(),
                    producer_id: name.into(),
                    producer_epoch: "captured".into(),
                    producer_seq: 1,
                },
                SourcePosition {
                    source: SourceRef {
                        issuer,
                        instance: name.into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        record_ordinal: record.ordinal,
                        byte_start: record.offset,
                        byte_end: record.offset + record.raw.len() as u64,
                    },
                },
                record.raw,
                TransportKind::NativeJsonl,
            )
            .await;
        fixture
            .worker()
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        if name == "early-result" {
            assert!(!reduction(&claims(&store).await)
                .objects
                .values()
                .any(|object| matches!(
                    object.declaration.known(),
                    Some(TraceObject::Resource(_))
                )));
            store = reopened(store).await;
        }
    }
    let reduced = reduction(&claims(&store).await);
    assert!(reduced
        .object(&AnyObjectRef::Resource(schedule()))
        .is_some());
    assert_eq!(reduced.resource_states[&schedule()].len(), 1);
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    let (fresh, _) = fixture.generation("late-schedule-rebuilt").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    assert_projection(
        &projection(&fixture, &fresh).await,
        &projection(&fixture, &store).await,
    );
}
