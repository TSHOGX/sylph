use super::*;
use sylph_protocol::SourceAttachmentEvent;

fn issuer() -> Issuer {
    Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap())
}
fn source(name: &str) -> SourceRef {
    SourceRef {
        issuer: issuer(),
        instance: name.into(),
    }
}
fn observation(name: &str, seq: u64) -> ObservationRef {
    ObservationRef {
        issuer: issuer(),
        producer_id: name.into(),
        producer_epoch: "incarnation".into(),
        producer_seq: seq,
    }
}
fn owner() -> AgentRef {
    AgentRef::Session(SessionId::decode("env7fk2:codex:runtime-owner").unwrap())
}
fn runtime(kind: &str, slot: &str) -> serde_json::Value {
    serde_json::json!({"schema_version":1,"rollout_id":"runtime-incarnation","thread_id":"runtime-owner","codex_turn_id":"turn-runtime","payload":{"type":kind,"tool_call_id":"exec-a","status":"completed","runtime_payload":{"raw_payload_id":slot,"path":format!("{slot}.json"),"kind":{"type":"tool_runtime_event"}}}})
}
async fn frame(fixture: &Fixture, sequence: u64, value: &serde_json::Value) -> Capture {
    fixture
        .admit(
            observation("runtime", sequence),
            SourcePosition {
                source: source("runtime"),
                generation: 0,
                locator: SourceLocator::Frame { ordinal: sequence },
            },
            &serde_json::to_vec(value).unwrap(),
            TransportKind::LiveStdout,
        )
        .await
}
async fn attachment(fixture: &Fixture, parent: &Capture, name: &str, value: &serde_json::Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    let payload = fixture.stage(&bytes).await;
    let attachment = CaptureAttachment {
        locator: format!("{name}.json"),
        source_position: None,
        observed_version: None,
        payload_ref: Some(payload),
        availability: AttachmentAvailability::Captured,
        relation: "codex.raw.runtime_payload".into(),
    };
    let event = SourceAttachmentEvent {
        parent_observation: parent.observation_ref.clone(),
        parent_slot: "/payload/runtime_payload".into(),
        attachment: attachment.clone(),
    };
    let mut capture = parent.clone();
    capture.observation_ref = observation(name, 1);
    capture.source_position = SourcePosition {
        source: source(name),
        generation: 0,
        locator: SourceLocator::Occurrence { key: name.into() },
    };
    capture.transport_kind = TransportKind::SourceAttachment;
    capture.payload_ref = fixture.stage(&serde_json::to_vec(&event).unwrap()).await;
    capture.attachments = vec![attachment];
    fixture
        .evidence
        .admit_batch(&issuer(), &[capture])
        .await
        .unwrap();
}
fn begin() -> serde_json::Value {
    serde_json::json!({"call_id":"exec-a","turn_id":"turn-runtime","process_id":"7","source":"unified_exec_startup","command":["cat"],"cwd":"/workspace","parsed_cmd":[]})
}
fn end() -> serde_json::Value {
    serde_json::json!({"call_id":"exec-a","turn_id":"turn-runtime","process_id":"7","source":"unified_exec_startup","command":["cat"],"cwd":"/workspace","parsed_cmd":[],"exit_code":0,"aggregated_output":"retained output","status":"completed"})
}
async fn step(fixture: &Fixture, worker: &ProjectionWorker, generation: &GenerationStore) {
    worker
        .catch_up(
            generation,
            1,
            fixture.evidence.evidence_cut().await.unwrap(),
        )
        .await
        .unwrap();
}
async fn run(delayed: bool, restart: bool) -> (Vec<TraceObject>, Vec<serde_json::Value>) {
    let fixture = Fixture::new().await;
    let (generation, _) = fixture.generation("delayed-runtime").await;
    let mut worker = fixture.worker();
    frame(&fixture,1,&serde_json::json!({"schema_version":1,"rollout_id":"runtime-incarnation","thread_id":"runtime-owner","payload":{"type":"rollout_started"}})).await;
    let start = frame(&fixture, 2, &runtime("tool_call_runtime_started", "start")).await;
    step(&fixture, &worker, &generation).await;
    if !delayed {
        attachment(&fixture, &start, "start", &begin()).await;
        step(&fixture, &worker, &generation).await;
    }
    let finish = frame(&fixture, 3, &runtime("tool_call_runtime_ended", "end")).await;
    step(&fixture, &worker, &generation).await;
    if delayed {
        // An independent source continues to project while this runtime waits.
        fixture
            .admit(
                observation("peer", 1),
                SourcePosition {
                    source: source("peer"),
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        byte_start: 0,
                        byte_end: 100,
                        record_ordinal: 0,
                    },
                },
                br#"{"type":"session_meta","payload":{"id":"unrelated-peer"}}"#,
                TransportKind::NativeJsonl,
            )
            .await;
        step(&fixture, &worker, &generation).await;
        let (_, reduction, _) = worker.projected_snapshot(&generation).await.unwrap();
        assert!(reduction
            .object(&AnyObjectRef::Agent(AgentRef::Session(
                SessionId::decode("env7fk2:codex:unrelated-peer").unwrap()
            )))
            .is_some());
        if restart {
            drop(worker);
            worker = fixture.worker();
        }
        attachment(&fixture, &finish, "end", &end()).await;
        step(&fixture, &worker, &generation).await;
        let (_, blocked, _) = worker.projected_snapshot(&generation).await.unwrap();
        assert!(!blocked.objects.values().filter_map(|value|value.declaration.known()).any(|object|matches!(object,TraceObject::Operation(operation) if operation.operation_ref.scope()==&owner())),"successor cannot publish an end-derived operation before its start resolves");
        attachment(&fixture, &start, "start", &begin()).await;
        step(&fixture, &worker, &generation).await;
    } else {
        attachment(&fixture, &finish, "end", &end()).await;
        step(&fixture, &worker, &generation).await;
    }
    let (_, reduction, _) = worker.projected_snapshot(&generation).await.unwrap();
    let incarnation = format!("{}:0", source("runtime").encode());
    let reference = OperationRef::native(
        owner(),
        "codex.exec-runtime",
        vec![incarnation, "exec-a".into()],
    )
    .unwrap();
    let declaration = &reduction
        .object(&AnyObjectRef::Operation(reference.clone()))
        .expect("runtime operation must exist")
        .declaration;
    let TraceObject::Operation(operation) = declaration
        .known()
        .expect("deferred begin/end must preserve one unconflicted operation")
    else {
        panic!()
    };
    let input = operation
        .input_revision
        .as_ref()
        .expect("launch input must be retained");
    assert_eq!(
        fixture.evidence.payload_bytes(input, 4096).await.unwrap(),
        serde_json::to_vec(&begin()).unwrap()
    );
    assert!(
        !reduction.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.code,
            DiagnosticCode::ConflictingOwner
                | DiagnosticCode::ConflictingOutcome
                | DiagnosticCode::InvalidRelation
        )),
        "delayed source replay must not create correction conflicts: {:?}",
        reduction.diagnostics
    );
    assert_eq!(
        reduction
            .execution(&ExecutionRef::Operation(reference).object_ref())
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    let objects = reduction
        .objects
        .values()
        .filter_map(|value| value.declaration.known())
        .filter(|object| object.object_ref().scope() == &owner())
        .cloned()
        .collect();
    let states = generation
        .mapper_states()
        .await
        .unwrap()
        .into_iter()
        .filter(|(key, _)| {
            key.starts_with("source:")
                && key.contains("runtime")
                && !key.contains("start")
                && !key.contains("end")
        })
        .map(|(_, state)| state)
        .collect();
    (objects, states)
}

#[tokio::test]
async fn delayed_runtime_dependencies_replay_the_source_in_order_across_restart() {
    let immediate = run(false, false).await;
    assert_eq!(immediate, run(true, false).await);
    assert_eq!(immediate, run(true, true).await);
}

#[tokio::test]
async fn ordered_successors_resume_in_bounded_same_cut_pages_after_restart() {
    let fixture = Fixture::new().await;
    let (generation, _) = fixture.generation("bounded-source-tail").await;
    let limits = WorkerLimits {
        max_unit_observations: 2,
        ..Default::default()
    };
    let worker = fixture.worker_with(limits.clone());
    frame(&fixture,1,&serde_json::json!({"schema_version":1,"rollout_id":"runtime-incarnation","thread_id":"runtime-owner","payload":{"type":"rollout_started"}})).await;
    let start = frame(&fixture, 2, &runtime("tool_call_runtime_started", "start")).await;
    step(&fixture, &worker, &generation).await;
    let mut tails = Vec::new();
    for index in 0..7 {
        let slot = format!("tail-{index}");
        tails.push((
            frame(
                &fixture,
                index + 3,
                &runtime("tool_call_runtime_ended", &slot),
            )
            .await,
            slot,
        ));
    }
    step(&fixture, &worker, &generation).await;
    for (tail, slot) in &tails {
        attachment(&fixture, tail, slot, &end()).await;
    }
    step(&fixture, &worker, &generation).await;
    let before = fixture.evidence.evidence_cut().await.unwrap();
    attachment(&fixture, &start, "start", &begin()).await;
    let through = fixture.evidence.evidence_cut().await.unwrap();
    let captured = fixture
        .evidence
        .admissions_after(before, 1)
        .await
        .unwrap()
        .captures;
    let held = worker
        .states
        .lock()
        .await
        .get("bounded-source-tail")
        .unwrap()
        .clone();
    let state = held.lock().await;
    let unit = worker
        .prepare_unit(&generation, &state, &captured, through)
        .await
        .unwrap();
    let advanced = unit
        .next
        .pending
        .values()
        .filter(|pending| pending.source_order.is_some())
        .count();
    assert!(
        advanced >= tails.len() - limits.max_unit_observations,
        "one projection unit must preserve the remaining bounded tail"
    );
    worker.commit_effects(&generation, &unit).await.unwrap();
    generation.apply(1, before, &unit.batch).await.unwrap();
    drop(state);
    drop(held);
    drop(worker);
    let revision = generation
        .checkpoint()
        .await
        .unwrap()
        .snapshot
        .view_revision;
    let restarted = fixture.worker_with(limits.clone());
    assert!(restarted.has_continuations(&generation).await.unwrap());
    let result = restarted.catch_up(&generation, 1, through).await.unwrap();
    assert_eq!(result.pending_mappings, 0);
    let final_snapshot = generation.checkpoint().await.unwrap().snapshot;
    assert_eq!(final_snapshot.evidence_cut, through);
    assert!(final_snapshot.view_revision >= revision + 3);
    let (_, reduction, _) = restarted.projected_snapshot(&generation).await.unwrap();
    let results=reduction.objects.values().filter_map(|value|value.declaration.known()).filter(|object|matches!(object,TraceObject::Result(result) if result.representation==Representation::Runtime)).count();
    assert_eq!(results, tails.len() * 2);
}

#[tokio::test]
async fn blocked_source_pressure_stops_projection_without_losing_admitted_bytes() {
    let fixture = Fixture::new().await;
    let (generation, _) = fixture.generation("blocked-source-pressure").await;
    let worker = fixture.worker_with(WorkerLimits {
        max_pending: 2,
        ..Default::default()
    });
    frame(&fixture, 1, &runtime("tool_call_runtime_started", "start")).await;
    step(&fixture, &worker, &generation).await;
    let checkpoint = generation.checkpoint().await.unwrap().snapshot;
    frame(&fixture, 2, &runtime("tool_call_runtime_ended", "end-a")).await;
    frame(&fixture, 3, &runtime("tool_call_runtime_ended", "end-b")).await;
    let evidence_cut = fixture.evidence.evidence_cut().await.unwrap();
    assert!(matches!(
        worker.catch_up(&generation, 1, evidence_cut).await,
        Err(WorkerError::Limit("pending mappings"))
    ));
    assert_eq!(generation.checkpoint().await.unwrap().snapshot, checkpoint);
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), evidence_cut);
}
