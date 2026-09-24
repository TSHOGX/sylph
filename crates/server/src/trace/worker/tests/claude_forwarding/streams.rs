use super::*;
use serde_json::{Value, json};

fn stream(host: Option<&str>, event: Value) -> Value {
    json!({"type":"stream_event","session_id":"forward-root","parent_tool_use_id":host,"event":event})
}
fn frames() -> Vec<Value> {
    vec![
        stream(
            Some("host"),
            json!({"type":"message_start","message":{"id":"child-model","role":"assistant","content":[]}}),
        ),
        stream(
            None,
            json!({"type":"message_start","message":{"id":"root-model","role":"assistant","content":[]}}),
        ),
        stream(
            Some("host"),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"A"}}),
        ),
        stream(
            None,
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"ROOT"}}),
        ),
        stream(
            Some("host"),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"A"}}),
        ),
        stream(
            Some("host"),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"B"}}),
        ),
        stream(None, json!({"type":"message_stop"})),
        stream(Some("host"), json!({"type":"message_stop"})),
        json!({"type":"result","session_id":"forward-root","parent_tool_use_id":"host","subtype":"success","is_error":false}),
    ]
}
async fn texts(store: &GenerationStore, owner: &AgentRef) -> Vec<String> {
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    store
        .read_page(
            &snapshot,
            &read_scope(owner, None, "history"),
            None,
            None,
            None,
            64,
            false,
        )
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| {
            let unit: ReadingUnit = serde_json::from_value(row.body).unwrap();
            if let ReadingContent::Message { parts, .. } = unit.content {
                Some(parts.into_iter().map(|part| part.preview).collect())
            } else {
                None
            }
        })
        .collect()
}
async fn run(late: bool, same_source: bool) -> Vec<Claim> {
    let f = Fixture::for_vendor("claude").await;
    let (mut store, _) = f.generation("routed-stream").await;
    let mut observed = Vec::new();
    if !late {
        certify(&f, "proof").await;
    }
    if same_source {
        let mut call: Value = serde_json::from_slice(CALL).unwrap();
        let object = call.as_object_mut().unwrap();
        let root = object.remove("sessionId").unwrap();
        object.insert("session_id".into(), root);
        observed.push(
            admit(
                &f,
                "sdk",
                1,
                &serde_json::to_vec(&call).unwrap(),
                true,
                &f.epoch,
            )
            .await,
        );
        catch_up(&f, &store).await;
    }
    for (index, value) in frames().into_iter().enumerate() {
        observed.push(
            admit(
                &f,
                "sdk",
                index as u64 + 1 + u64::from(same_source),
                &serde_json::to_vec(&value).unwrap(),
                true,
                &f.epoch,
            )
            .await,
        );
        catch_up(&f, &store).await;
        if index == 6 {
            assert!(
                texts(&store, &root()).await.contains(&"ROOT".into()),
                "waiting child frames blocked the parent stream"
            );
        }
        if index == 4 {
            let path = store.path().to_owned();
            let manifest = store.manifest().clone();
            let limits = store.limits().clone();
            store.close().await;
            store = GenerationStore::open(path, manifest, limits).await.unwrap();
        }
    }
    if late {
        if same_source {
            let mut result: Value = serde_json::from_slice(RESULT).unwrap();
            let object = result.as_object_mut().unwrap();
            let root = object.remove("sessionId").unwrap();
            object.insert("session_id".into(), root);
            let details = object.remove("toolUseResult").unwrap();
            object.insert("tool_use_result".into(), details);
            observed.push(
                admit(
                    &f,
                    "sdk",
                    11,
                    &serde_json::to_vec(&result).unwrap(),
                    true,
                    &f.epoch,
                )
                .await,
            );
        } else {
            certify(&f, "proof").await;
        }
        catch_up(&f, &store).await;
    }
    assert_eq!(texts(&store,&child()).await,["AAB"],"late route lost or reordered the observed stream prefix (late={late}, same_source={same_source})");
    let values = claims(&store).await;
    let (_, reduced, _) = f.worker().projected_snapshot(&store).await.unwrap();
    let owned: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Activation(activation))
                if activation.activation_ref.scope() == &child() =>
            {
                Some(activation)
            }
            _ => None,
        })
        .collect();
    assert_eq!(owned.len(), 1);
    assert_eq!(
        reduced
            .execution(&AnyObjectRef::Activation(owned[0].activation_ref.clone()))
            .unwrap()
            .outcome
            .known(),
        Some(&TerminalOutcome::Succeeded)
    );
    for mut captured in observed {
        captured.observation_ref.producer_id.push_str("-reread");
        f.evidence
            .admit_batch(
                &captured.observation_ref.issuer,
                std::slice::from_ref(&captured),
            )
            .await
            .unwrap();
    }
    catch_up(&f, &store).await;
    assert_eq!(
        values,
        claims(&store).await,
        "reread replayed stream deltas"
    );
    let (fresh, _) = f.generation("routed-stream-rebuild").await;
    catch_up(&f, &fresh).await;
    assert_eq!(texts(&fresh, &child()).await, ["AAB"]);
    assert_eq!(values, claims(&fresh).await);
    values
}
#[tokio::test]
async fn a_late_child_route_preserves_interleaved_stream_state_across_restart_and_rebuild() {
    assert_eq!(run(false, false).await, run(true, false).await);
}
#[tokio::test]
async fn a_child_route_returned_later_in_the_same_sdk_stream_cannot_deadlock_or_drop_child_deltas()
{
    run(true, true).await;
}

async fn bounded_tail(by_bytes: bool) {
    let f = Fixture::for_vendor("claude").await;
    let (store, _) = f.generation("long-sdk-tail").await;
    let events = frames();
    for (seq, value) in [(1, &events[0]), (2, &events[2])] {
        admit(
            &f,
            "sdk",
            seq,
            &serde_json::to_vec(value).unwrap(),
            true,
            &f.epoch,
        )
        .await;
    }
    let count = 32;
    for index in 0..count {
        // The skipped parent must exceed the following child frame so omitting
        // its byte credit actually opens the next-read window in the byte test.
        let root = json!({"type":"user","session_id":"forward-root","uuid":format!("root-{index}"),"message":{"role":"user","content":"ROOT"},"padding":"x".repeat(512)});
        admit(
            &f,
            "sdk",
            3 + index * 2,
            &serde_json::to_vec(&root).unwrap(),
            true,
            &f.epoch,
        )
        .await;
        admit(
            &f,
            "sdk",
            4 + index * 2,
            &serde_json::to_vec(&events[4]).unwrap(),
            true,
            &f.epoch,
        )
        .await;
    }
    admit(
        &f,
        "sdk",
        3 + count * 2,
        &serde_json::to_vec(&events[7]).unwrap(),
        true,
        &f.epoch,
    )
    .await;
    let initial = f.evidence.admissions_after(1, 2).await.unwrap().captures;
    let mut byte_limit = 0;
    for record in &initial {
        byte_limit += f
            .evidence
            .payload_length(&record.capture.payload_ref)
            .await
            .unwrap()
            .unwrap() as usize;
    }
    let defaults = WorkerLimits::default();
    let limits = WorkerLimits {
        max_pending: 2,
        max_unit_observations: 4,
        max_unit_input_bytes: if by_bytes {
            byte_limit
        } else {
            defaults.max_unit_input_bytes
        },
        ..defaults
    };
    let worker = f.worker_with(limits.clone());
    let before = f.evidence.evidence_cut().await.unwrap();
    let progress = worker.catch_up(&store, 1, before).await.unwrap();
    assert_eq!(progress.pending_mappings, 1);
    assert_eq!(texts(&store, &root()).await.len(), count as usize);
    admit(&f, "proof", 1, CALL, false, &f.epoch).await;
    worker.catch_up(&store, 1, before + 1).await.unwrap();
    admit(&f, "proof", 2, RESULT, false, &f.epoch).await;
    let checkpoint = store.checkpoint().await.unwrap();
    let (state, _) = worker
        .load_state(&store, &checkpoint.snapshot)
        .await
        .unwrap();
    assert_eq!(state.source_replays.len(), 1);
    let captures = f
        .evidence
        .admissions_after(before + 1, 1)
        .await
        .unwrap()
        .captures;
    let unit = worker
        .prepare_unit(&store, &state, &captures, before + 2)
        .await
        .unwrap();
    let scanned = f
        .evidence
        .admissions_after(1, limits.max_unit_observations)
        .await
        .unwrap()
        .captures;
    assert_eq!(
        unit.next.source_replays.values().next().unwrap().after,
        if by_bytes {
            initial[1].admission_seq
        } else {
            scanned.last().unwrap().admission_seq
        },
        "skipped parent frames consumed no replay work (byte bound={by_bytes})"
    );
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    let path = store.path().to_owned();
    let manifest = store.manifest().clone();
    let query_limits = store.limits().clone();
    store.close().await;
    let store = GenerationStore::open(path, manifest, query_limits)
        .await
        .unwrap();
    let progress = f
        .worker_with(limits)
        .catch_up(&store, 1, before + 2)
        .await
        .unwrap();
    assert_eq!(progress.pending_mappings, 0);
    assert_eq!(
        texts(&store, &child()).await,
        ["A".repeat(count as usize + 1)]
    );
}

#[tokio::test]
async fn a_blocked_sdk_lane_keeps_one_head_and_charges_skipped_parent_frames_to_replay_work() {
    bounded_tail(false).await;
}

#[tokio::test]
async fn skipped_parent_frames_consume_the_blocked_lanes_replay_byte_budget() {
    bounded_tail(true).await;
}

#[tokio::test]
async fn an_unclassifiable_sdk_record_fences_every_lane_without_poisoning_another_source() {
    let invalid_route = br#"{"type":"stream_event","session_id":"forward-root","parent_tool_use_id":42,"event":{"type":"message_start","message":{"id":"wrong-owner"}}}"#;
    for raw in [b"{".as_slice(), invalid_route] {
        let f = Fixture::for_vendor("claude").await;
        let (mut store, _) = f.generation("bad-sdk-route").await;
        let events = frames();
        for (seq, value) in [
            (1, &events[1]),
            (2, &events[3]),
            (3, &events[0]),
            (4, &events[2]),
        ] {
            admit(
                &f,
                "sdk",
                seq,
                &serde_json::to_vec(value).unwrap(),
                true,
                &f.epoch,
            )
            .await;
            catch_up(&f, &store).await;
        }
        let invalid = admit(&f, "sdk", 5, raw, true, &f.epoch).await;
        let progress = f
            .worker()
            .catch_up(&store, 1, f.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert!(progress.suspended_sources > 0);
        let path = store.path().to_owned();
        let manifest = store.manifest().clone();
        let limits = store.limits().clone();
        store.close().await;
        store = GenerationStore::open(path, manifest, limits).await.unwrap();
        let after = stream(
            None,
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"AFTER GAP"}}),
        );
        admit(
            &f,
            "sdk",
            6,
            &serde_json::to_vec(&after).unwrap(),
            true,
            &f.epoch,
        )
        .await;
        certify(&f, "proof").await;
        admit(
            &f,
            "peer",
            1,
            br#"{"type":"system","subtype":"init","session_id":"peer"}"#,
            true,
            &f.epoch,
        )
        .await;
        let progress = f
            .worker()
            .catch_up(&store, 1, f.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_eq!(progress.pending_mappings, 0);
        let root_text = texts(&store, &root()).await;
        assert!(root_text.contains(&"ROOT".into()));
        assert!(!root_text.iter().any(|text| text.contains("AFTER GAP")));
        assert_eq!(
            texts(&store, &child()).await,
            ["A"],
            "a routing gap erased an earlier retained child prefix"
        );
        let (_, reduced, _) = f.worker().projected_snapshot(&store).await.unwrap();
        assert_eq!(reduced.objects.values().filter(|value| matches!(value.declaration.known(), Some(TraceObject::Activation(activation)) if activation.activation_ref.scope() == &root())).count(), 1, "bad routing created another root activation");
        assert_eq!(reduced.objects.values().filter(|value|matches!(value.declaration.known(),Some(TraceObject::Activation(activation)) if activation.activation_ref.scope()==&child())).count(),1,"a later routing gap erased the valid earlier child start");
        assert!(reduced
            .object(&AnyObjectRef::Agent(AgentRef::Session(
                SessionId::decode("env7fk2:claude:peer").unwrap()
            )))
            .is_some());
        assert_eq!(
            f.evidence
                .payload_bytes(&invalid.payload_ref, raw.len())
                .await
                .unwrap(),
            raw
        );
    }
}

#[tokio::test]
async fn sdk_control_envelopes_without_session_ids_do_not_interrupt_content_lanes() {
    let f = Fixture::for_vendor("claude").await;
    let (store, _) = f.generation("control-and-content").await;
    let events = frames();
    let records = vec![
        json!({"type":"control_response","response":{"subtype":"success","request_id":"initialize","response":{}}}),
        events[1].clone(),
        events[3].clone(),
        json!({"type":"control_request","request_id":"permission","request":{"subtype":"can_use_tool","tool_name":"Read","input":{}}}),
        json!({"type":"keep_alive"}),
        stream(
            None,
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"A"}}),
        ),
        json!({"type":"control_cancel_request","request_id":"permission"}),
        events[6].clone(),
        json!({"type":"result","session_id":"forward-root","subtype":"success","is_error":false}),
    ];
    for (index, record) in records.into_iter().enumerate() {
        admit(
            &f,
            "sdk",
            index as u64 + 1,
            &serde_json::to_vec(&record).unwrap(),
            true,
            &f.epoch,
        )
        .await;
        catch_up(&f, &store).await;
    }
    assert_eq!(texts(&store, &root()).await, ["ROOTA"]);
    let (_, reduced, _) = f.worker().projected_snapshot(&store).await.unwrap();
    assert_eq!(
        reduced
            .executions
            .values()
            .filter(|execution| execution.outcome.known() == Some(&TerminalOutcome::Succeeded))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_route_discovered_while_replaying_a_child_tail_wakes_its_grandchild_without_new_capture()
{
    let f = Fixture::for_vendor("claude").await;
    let (mut store, _) = f.generation("nested-late-routes").await;
    let call = json!({"type":"assistant","session_id":"forward-root","uuid":"root-call","message":{"role":"assistant","content":[{"type":"tool_use","id":"host","name":"Agent","input":{}}]}});
    let child_call = json!({"type":"assistant","session_id":"forward-root","parent_tool_use_id":"host","uuid":"child-call","message":{"role":"assistant","content":[{"type":"tool_use","id":"nested","name":"Agent","input":{}}]}});
    let child_return = json!({"type":"user","session_id":"forward-root","parent_tool_use_id":"host","uuid":"child-return","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"nested","content":"nested handback"}]},"tool_use_result":{"agentId":"grandchild","status":"completed"}});
    let root_return = json!({"type":"user","session_id":"forward-root","uuid":"root-return","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"host","content":"child handback"}]},"tool_use_result":{"agentId":"forward-child","status":"completed"}});
    let records = vec![
        call,
        stream(
            Some("nested"),
            json!({"type":"message_start","message":{"id":"grandchild-model","role":"assistant","content":[]}}),
        ),
        stream(
            Some("nested"),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"GRANDCHILD"}}),
        ),
        child_call,
        child_return,
        stream(Some("nested"), json!({"type":"message_stop"})),
        root_return,
    ];
    for (index, value) in records.into_iter().enumerate() {
        admit(
            &f,
            "sdk",
            index as u64 + 1,
            &serde_json::to_vec(&value).unwrap(),
            true,
            &f.epoch,
        )
        .await;
        if index == 6 {
            let worker = f.worker();
            let checkpoint = store.checkpoint().await.unwrap();
            let (state, _) = worker
                .load_state(&store, &checkpoint.snapshot)
                .await
                .unwrap();
            let captures = f
                .evidence
                .admissions_after(checkpoint.snapshot.evidence_cut, 1)
                .await
                .unwrap()
                .captures;
            let unit = worker
                .prepare_unit(
                    &store,
                    &state,
                    &captures,
                    f.evidence.evidence_cut().await.unwrap(),
                )
                .await
                .unwrap();
            worker.commit_effects(&store, &unit).await.unwrap();
            crate::trace::worker::reconciliation::apply(
                &store,
                1,
                &checkpoint.snapshot,
                &unit.batch,
            )
            .await
            .unwrap();
            drop(worker);
            let path = store.path().to_owned();
            let manifest = store.manifest().clone();
            let limits = store.limits().clone();
            store.close().await;
            store = GenerationStore::open(path, manifest, limits).await.unwrap();
            assert!(
                f.worker().has_continuations(&store).await.unwrap(),
                "committed ready work disappeared on restart"
            );
        }
        let progress = f
            .worker()
            .catch_up(&store, 1, f.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_eq!(progress.suspended_sources, 0);
        if index == 6 {
            assert_eq!(
                progress.pending_mappings, 0,
                "a ready lane was left asleep at the completed evidence cut"
            );
        }
    }
    let grandchild = AgentRef::Embedded {
        anchor_session: root().anchor_session().clone(),
        namespace: "claude.agent".into(),
        native_child_id: "grandchild".into(),
    };
    assert_eq!(texts(&store, &grandchild).await, ["GRANDCHILD"]);
}

#[tokio::test]
async fn a_stale_ready_flag_returns_to_waiting_without_spinning_at_the_same_cut() {
    let f = Fixture::for_vendor("claude").await;
    let (store, _) = f.generation("stale-readiness").await;
    forward(&f, 0).await;
    catch_up(&f, &store).await;
    let worker = f.worker();
    let checkpoint = store.checkpoint().await.unwrap();
    let (state, _) = worker
        .load_state(&store, &checkpoint.snapshot)
        .await
        .unwrap();
    let mut unit = ProjectionUnit::new(&state, checkpoint.snapshot.evidence_cut);
    unit.batch.continuation = true;
    // Readiness is a retry hint, not a durable identity certificate. Model a
    // wakeup whose required route is no longer available at the next attempt.
    let mut pending = state.pending.values().next().unwrap().as_ref().clone();
    pending.ready = true;
    worker.save_pending(&mut unit, pending).unwrap();
    worker.commit_effects(&store, &unit).await.unwrap();
    crate::trace::worker::reconciliation::apply(&store, 1, &checkpoint.snapshot, &unit.batch)
        .await
        .unwrap();
    drop(worker);
    let worker = f.worker();
    assert!(worker.has_continuations(&store).await.unwrap());
    let progress = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        worker.catch_up(&store, 1, checkpoint.snapshot.evidence_cut),
    )
    .await
    .expect("a stale wakeup spun without new dependency progress")
    .unwrap();
    assert_eq!(progress.pending_mappings, 1);
    assert!(!worker.has_continuations(&store).await.unwrap());
    assert!(texts(&store, &child()).await.is_empty());
}
