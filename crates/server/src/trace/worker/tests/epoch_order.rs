use super::*;
use crate::trace::reading_store::read_scope;
use serde_json::json;

fn participant(name: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{name}")).unwrap())
}

fn source(name: &str) -> SourceRef {
    SourceRef {
        issuer: Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap()),
        instance: format!("epoch-order-{name}"),
    }
}

// Protocol regressions derived from the retained native header/spawn shapes.
// These minimal records are not presented as a new vendor capture certificate.
fn records(name: &str) -> Vec<Vec<u8>> {
    let values = if name == "parent" {
        vec![
            json!({"ordinal":0,"type":"session_meta","payload":{
                "id":"parent","session_id":"parent","cli_version":"0.147.0","source":"cli",
            }}),
            json!({"ordinal":1,"type":"event_msg","payload":{
                "type":"collab_agent_spawn_end","sender_thread_id":"parent","call_id":"spawn-1",
                "new_thread_id":"child","new_agent_nickname":"Child",
            }}),
        ]
    } else {
        vec![
            json!({"ordinal":0,"type":"session_meta","payload":{
                "id":"child","session_id":"parent","cli_version":"0.147.0","agent_nickname":"Child",
                "source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1}}},
            }}),
            json!({"ordinal":1,"type":"response_item","payload":{
                "type":"message","id":"child-answer","role":"assistant",
                "content":[{"type":"output_text","text":"Child-owned evidence"}],
            }}),
        ]
    };
    values
        .into_iter()
        .map(|value| {
            let mut raw = serde_json::to_vec(&value).unwrap();
            raw.push(b'\n');
            raw
        })
        .collect()
}

async fn replay(child_first: bool) -> (Vec<Claim>, Reduction, ReaderProjection) {
    let fixture = Fixture::new().await;
    let parent = participant("parent");
    let child = participant("child");
    let mut epochs = BTreeMap::new();
    for participant in [&parent, &child] {
        let epoch = fixture
            .evidence
            .authorize_scope(
                &IngestionScope::Agent(participant.clone()),
                std::slice::from_ref(participant),
            )
            .await
            .unwrap();
        epochs.insert(participant.clone(), epoch);
    }
    assert_ne!(epochs[&parent], epochs[&child]);
    let (generation, _) = fixture.generation("epoch-order").await;
    let worker = fixture.worker();
    let order = if child_first {
        ["child", "parent"]
    } else {
        ["parent", "child"]
    };
    for name in order {
        let owner = participant(name);
        let source = source(name);
        let mut offset = 0;
        for (ordinal, raw) in records(name).into_iter().enumerate() {
            let capture = Capture {
                observation_ref: ObservationRef {
                    issuer: source.issuer.clone(),
                    producer_id: source.instance.clone(),
                    producer_epoch: "retained-shape".into(),
                    producer_seq: ordinal as u64 + 1,
                },
                source_position: SourcePosition {
                    source: source.clone(),
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        byte_start: offset,
                        byte_end: offset + raw.len() as u64,
                        record_ordinal: ordinal as u64,
                    },
                },
                ingestion_authorization_epoch: epochs[&owner].clone(),
                declared_subject_refs: vec![owner.clone()],
                captured_at: TimeEvidence {
                    domain: "fixture.wall".into(),
                    value: 100,
                    unit: TimeUnit::Milliseconds,
                    precision: Some(1),
                    basis: TimeBasis::SourceWall,
                },
                source_clock: None,
                transport_kind: TransportKind::NativeJsonl,
                vendor_build: Some("0.147.0".into()),
                schema_fingerprint: None,
                payload_ref: fixture.stage(&raw).await,
                attachments: vec![],
                capture_diagnostics: vec![],
            };
            fixture
                .evidence
                .admit_batch(&source.issuer, &[capture])
                .await
                .unwrap();
            offset += raw.len() as u64;
            worker
                .catch_up(
                    &generation,
                    1,
                    fixture.evidence.evidence_cut().await.unwrap(),
                )
                .await
                .unwrap();
        }
    }
    let retained = claims(&generation).await;
    let mut cold = Reducer::default();
    cold.apply(
        &retained,
        &retained
            .iter()
            .flat_map(|claim| claim.evidence.iter().cloned())
            .collect(),
        1000,
    )
    .unwrap();
    let reduction = cold.resolve();
    let references: BTreeSet<_> = reduction
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Agent(agent)) => Some(agent.agent_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(references, BTreeSet::from([parent.clone(), child.clone()]));
    assert!(reduction.relations.iter().any(|relation| matches!(&relation.relation,
        Relation::Spawn { parent: actual_parent, child: actual_child, .. } if actual_parent == &parent && actual_child == &child)));
    let reader = reader_projection(&fixture, &generation).await;
    let child_history = read_scope(&child, None, "history");
    let child_rows: Vec<_> = reader
        .rows
        .iter()
        .filter(|((scope, _), _)| scope == &child_history)
        .collect();
    assert_eq!(child_rows.len(), 1);
    assert_eq!(
        child_rows[0].1.source_epoch,
        OracleEpoch::Exact(epochs[&child].clone())
    );
    let unit: ReadingUnit = serde_json::from_value(child_rows[0].1.body.clone()).unwrap();
    let ReadingContent::Message {
        author: ReadAuthor::Agent {
            agent: Some(author),
            ..
        },
        parts,
        ..
    } = unit.content
    else {
        panic!("the child message must retain its actual author")
    };
    assert_eq!(author.address.agent(), child);
    assert_eq!(parts[0].preview, "Child-owned evidence");
    let parent_directory = read_scope(&parent, None, "relations");
    let directory: Vec<_> = reader
        .rows
        .iter()
        .filter(|((scope, _), _)| scope == &parent_directory)
        .collect();
    assert_eq!(directory.len(), 1);
    assert_eq!(
        directory[0].1.source_epoch,
        OracleEpoch::Exact(epochs[&parent].clone())
    );
    (
        retained,
        reduction,
        reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    )
}

#[tokio::test]
#[ignore = "known failing acceptance gate: merged declaration provenance uses first-arriving epoch"]
async fn distinct_parent_and_child_capture_scopes_keep_reader_authorization_independent_of_source_order()
 {
    let parent_first = replay(false).await;
    let child_first = replay(true).await;
    assert_eq!(
        parent_first.0, child_first.0,
        "both schedules retain identical source-backed claims"
    );
    assert_eq!(
        parent_first.1, child_first.1,
        "scope order cannot change the resolved semantic graph"
    );
    assert_eq!(
        parent_first.2, child_first.2,
        "reader source/target epoch provenance must not be selected by first-arriving declaration"
    );
}
