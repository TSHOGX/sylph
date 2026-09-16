use super::*;
use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue};

const PARENT: &str = "11111111-1111-4111-8111-111111111111";
const CHILD: &str = "22222222-2222-4222-8222-222222222222";

fn source(physical: &str) -> SourceRef {
    SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        instance: physical.into(),
    }
}
fn observation(physical: &str, seq: u64) -> ObservationRef {
    ObservationRef {
        issuer: source(physical).issuer,
        producer_id: physical.into(),
        producer_epoch: "incarnation".into(),
        producer_seq: seq,
    }
}
fn agent(native: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{native}")).unwrap())
}

async fn native(
    fixture: &Fixture,
    physical: &str,
    native: &str,
    base: Option<serde_json::Value>,
    ordinal: u64,
    messages: usize,
) -> u64 {
    let mut rows = vec![
        serde_json::json!({"type":"session_meta","ordinal":ordinal,"payload":{"id":native,"history_mode":"paginated","history_base":base}}),
    ];
    rows.extend((0..messages).map(|index|serde_json::json!({"type":"response_item","ordinal":ordinal+index as u64+1,"payload":{"type":"message","id":format!("{physical}-{index}"),"role":"assistant","content":[{"type":"output_text","text":format!("frozen prefix {index}")}]}})));
    let mut offset = 0;
    for (index, row) in rows.iter().enumerate() {
        let mut bytes = serde_json::to_vec(row).unwrap();
        bytes.push(b'\n');
        fixture
            .admit(
                observation(physical, index as u64 + 1),
                SourcePosition {
                    source: source(physical),
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        byte_start: offset,
                        byte_end: offset + bytes.len() as u64,
                        record_ordinal: index as u64,
                    },
                },
                &bytes,
                TransportKind::NativeJsonl,
            )
            .await;
        offset += bytes.len() as u64;
    }
    let definition = SourceDefinition {
        source: source(physical),
        generation: 0,
        locator: format!("/retained/rollout-2026-09-16T00-00-00-{physical}.jsonl"),
        owner: agent(native),
        basis: SourceOwnerBasis::NativeIdentity {
            observation: observation(physical, 1),
        },
    };
    fixture
        .admit(
            observation(&format!("inventory-{physical}"), 1),
            SourcePosition {
                source: source(&format!("inventory-{physical}")),
                generation: 0,
                locator: SourceLocator::Occurrence {
                    key: "definition".into(),
                },
            },
            &serde_json::to_vec(&definition).unwrap(),
            TransportKind::SourceInventory,
        )
        .await;
    offset
}
async fn selection(fixture: &Fixture, physical: &str, native: &str, count: u64) {
    let snapshot = NativeDatabaseSnapshot {
        database_locator: "/vendor/state_5.sqlite".into(),
        table: "threads".into(),
        columns: vec![],
        status: NativeDatabaseStatus::Available,
        row: [("id".into(), NativeDatabaseValue::Text(native.into()))].into(),
        selected_source: Some(source(physical)),
        selected_generation: Some(0),
        selected_record_count: Some(count),
        selected_pending_bytes: Some(0),
    };
    fixture
        .admit(
            observation("selected-database", 1),
            SourcePosition {
                source: source("selected-database"),
                generation: 0,
                locator: SourceLocator::Occurrence {
                    key: "snapshot".into(),
                },
            },
            &serde_json::to_vec(&snapshot).unwrap(),
            TransportKind::NativeDatabase,
        )
        .await;
}

#[tokio::test]
async fn frozen_parent_aliases_resume_after_restart_at_the_same_evidence_cut() {
    let fixture = Fixture::new().await;
    let count = 110;
    let bytes = native(&fixture, PARENT, "parent", None, 0, count).await;
    native(&fixture,CHILD,"child",Some(serde_json::json!({"thread_id":PARENT,"end_ordinal_exclusive":count+1,"end_byte_offset":bytes})),count as u64+1,1).await;
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    let (generation, _) = fixture.generation("frozen-history").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, cut).await.unwrap();
    selection(&fixture, CHILD, "child", 2).await;
    let through = fixture.evidence.evidence_cut().await.unwrap();
    // Commit exactly the first bounded alias page, then discard all worker RAM.
    // The latest admitted cut is already visible: resumption must use the durable
    // continuation checkpoint rather than wait for another capture to arrive.
    let captures = fixture
        .evidence
        .admissions_after(cut, 1)
        .await
        .unwrap()
        .captures;
    let held = worker
        .states
        .lock()
        .await
        .get("frozen-history")
        .unwrap()
        .clone();
    let state = held.lock().await;
    let unit = worker
        .prepare_unit(&generation, &state, &captures, through)
        .await
        .unwrap();
    assert!(unit
        .next
        .pending
        .values()
        .any(|pending| pending.requests.contains(&ArchiveRequest::ContinueHistory)));
    worker.commit_effects(&generation, &unit).await.unwrap();
    generation.apply(1, cut, &unit.batch).await.unwrap();
    drop(state);
    drop(held);
    drop(worker);
    let first = generation.checkpoint().await.unwrap().snapshot;
    let restarted = fixture.worker();
    assert!(restarted.has_continuations(&generation).await.unwrap());
    let progress = restarted.catch_up(&generation, 1, through).await.unwrap();
    assert_eq!(progress.pending_mappings, 0);
    assert!(!restarted.has_continuations(&generation).await.unwrap());
    let final_view = generation.checkpoint().await.unwrap().snapshot;
    assert_eq!(final_view.evidence_cut, first.evidence_cut);
    assert!(final_view.view_revision > first.view_revision);
    let (_, reduction, _) = restarted.projected_snapshot(&generation).await.unwrap();
    let entries = reduction.histories[&agent("child")]
        .selected_entries
        .as_ref()
        .unwrap();
    assert_eq!(entries.len(), count + 1);
    for entry in &entries[..count] {
        let TraceObject::HistoryEntry(entry) = reduction
            .object(&AnyObjectRef::HistoryEntry(entry.clone()))
            .unwrap()
            .declaration
            .known()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(entry.containing_agent, agent("child"));
        assert_eq!(
            entry.original_owner,
            Some(OwnerRef::Agent {
                agent: agent("parent")
            })
        );
    }
    assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), through);
    super::super::super::publication::ProjectionCatchUp::validate_generation(
        &restarted,
        &generation,
        through,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn selection_waits_for_late_physical_binding_and_never_accepts_only_the_child_suffix() {
    let fixture = Fixture::new().await;
    // The expected byte extent is read from an independently serialized fixture,
    // not a count shared with the mapper's coordinate conversion.
    let header = serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"parent","history_mode":"paginated","history_base":null}});
    let message = serde_json::json!({"type":"response_item","ordinal":1,"payload":{"type":"message","id":format!("{PARENT}-0"),"role":"assistant","content":[{"type":"output_text","text":"frozen prefix 0"}]}});
    let expected = (serde_json::to_vec(&header).unwrap().len()
        + serde_json::to_vec(&message).unwrap().len()
        + 2) as u64;
    native(&fixture,CHILD,"child",Some(serde_json::json!({"thread_id":PARENT,"end_ordinal_exclusive":2,"end_byte_offset":expected})),2,1).await;
    selection(&fixture, CHILD, "child", 2).await;
    let (generation, _) = fixture.generation("late-frozen-source").await;
    let initial = fixture
        .worker()
        .catch_up(
            &generation,
            1,
            fixture.evidence.evidence_cut().await.unwrap(),
        )
        .await
        .unwrap();
    assert!(initial.pending_mappings > 0);
    let bytes = native(&fixture, PARENT, "parent", None, 0, 1).await;
    assert_eq!(bytes, expected);
    let worker = fixture.worker();
    worker
        .catch_up(
            &generation,
            1,
            fixture.evidence.evidence_cut().await.unwrap(),
        )
        .await
        .unwrap();
    let (_, reduction, _) = worker.projected_snapshot(&generation).await.unwrap();
    assert_eq!(
        reduction.histories[&agent("child")]
            .selected_entries
            .as_ref()
            .unwrap()
            .len(),
        2
    );
}
