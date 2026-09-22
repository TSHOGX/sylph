use super::*;
use sha2::{Digest, Sha256};
use sylph_protocol::{
    NativeDatabaseColumn, NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue,
};

const RAW: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/goal/goal.json");
const THREAD: &str = "01a0c6ff-3eff-7972-aefa-f4ad63d06308";
const GOAL: &str = "a22392d7-2781-496a-b85a-07f9192533e0";
fn owner() -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{THREAD}")).unwrap())
}
fn goal(id: &str) -> ResourceRef {
    ResourceRef::native(owner(), "codex.goal", vec![id.into()]).unwrap()
}
struct GoalRecord {
    source: &'static str,
    seq: u64,
    offset: u64,
    kind: TransportKind,
    raw: Vec<u8>,
}
fn records(order: SourceOrder) -> Vec<GoalRecord> {
    let original: NativeDatabaseSnapshot = serde_json::from_slice(RAW).unwrap();
    // Only the first row is a retained vendor capture. These subsequent rows are
    // explicit adversarial state changes of that recorded schema, not recordings.
    let mut paused = original.clone();
    paused
        .row
        .insert("status".into(), NativeDatabaseValue::Text("paused".into()));
    paused
        .row
        .insert("tokens_used".into(), NativeDatabaseValue::Integer(2331656));
    paused.row.insert(
        "time_used_seconds".into(),
        NativeDatabaseValue::Integer(22872),
    );
    paused.columns.push(NativeDatabaseColumn {
        name: "vendor_extension".into(),
        declared_type: "BLOB".into(),
    });
    paused.row.insert(
        "vendor_extension".into(),
        NativeDatabaseValue::Blob(vec![0, 255, 5]),
    );
    let mut replacement = original.clone();
    replacement.row.insert(
        "goal_id".into(),
        NativeDatabaseValue::Text("replacement-goal".into()),
    );
    replacement
        .row
        .insert("tokens_used".into(), NativeDatabaseValue::Integer(5));
    replacement
        .row
        .insert("time_used_seconds".into(), NativeDatabaseValue::Integer(2));
    replacement
        .row
        .insert("token_budget".into(), NativeDatabaseValue::Integer(800));
    let mut absent = original.clone();
    absent.status = NativeDatabaseStatus::MissingRow;
    absent.row.clear();
    let mut database = vec![GoalRecord {
        source: "goal-database",
        seq: 1,
        offset: 0,
        kind: TransportKind::NativeDatabase,
        raw: RAW.to_vec(),
    }];
    for (i, snapshot) in [paused, replacement, absent].iter().enumerate() {
        database.push(GoalRecord {
            source: "goal-database",
            seq: i as u64 + 2,
            offset: 0,
            kind: TransportKind::NativeDatabase,
            raw: serde_json::to_vec(snapshot).unwrap(),
        });
    }
    let NativeDatabaseValue::Text(objective) = &original.row["objective"] else {
        panic!("recorded goal objective");
    };
    let mut offset = 0;
    let public=[serde_json::json!({"type":"session_meta","payload":{"id":THREAD}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"thread_goal_updated","threadId":THREAD,"goal":{"threadId":THREAD,"objective":objective,"status":"complete","tokensUsed":2331656,"timeUsedSeconds":22872}}})]
        .iter().enumerate().map(|(i,value)| {
            let mut raw=serde_json::to_vec(value).unwrap(); raw.push(b'\n');
            let record=GoalRecord{source:"goal-public",seq:i as u64+1,offset,kind:TransportKind::NativeJsonl,raw};
            offset+=record.raw.len() as u64; record
        }).collect::<Vec<_>>();
    match order {
        SourceOrder::NativeFirst => database.into_iter().chain(public).collect(),
        SourceOrder::LiveFirst => public.into_iter().chain(database).collect(),
        SourceOrder::Alternating => {
            let mut a = database.into_iter();
            let mut b = public.into_iter();
            let mut all = vec![];
            loop {
                match (a.next(), b.next()) {
                    (None, None) => break,
                    (a, b) => all.extend(a.into_iter().chain(b)),
                }
            }
            all
        }
    }
}
async fn admit(fixture: &Fixture, record: &GoalRecord) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: record.source.into(),
                producer_epoch: "captured".into(),
                producer_seq: record.seq,
            },
            SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: record.source.into(),
                },
                generation: if record.kind == TransportKind::NativeDatabase {
                    record.seq - 1
                } else {
                    0
                },
                locator: if record.kind == TransportKind::NativeDatabase {
                    SourceLocator::Snapshot {
                        revision: (record.seq - 1).to_string(),
                    }
                } else {
                    SourceLocator::Jsonl {
                        record_ordinal: record.seq - 1,
                        byte_start: record.offset,
                        byte_end: record.offset + record.raw.len() as u64,
                    }
                },
            },
            &record.raw,
            record.kind,
        )
        .await
}
async fn expected(fixture: &Fixture, store: &GenerationStore) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|o| matches!(
                o.declaration.known(),
                Some(TraceObject::Resource(Resource {
                    kind: ResourceKind::Goal,
                    ..
                }))
            ))
            .count(),
        2
    );
    assert!(reduced.executions.is_empty());
    assert!(!reduced.objects.values().any(|o| matches!(
        o.declaration.known(),
        Some(TraceObject::Activation(_) | TraceObject::Message(_) | TraceObject::Delivery(_))
    )));
    let first_states = &reduced.resource_states[&goal(GOAL)];
    assert_eq!(first_states.len(), 2);
    let mut statuses = vec![];
    let mut saw_exact_capture = false;
    let mut saw_unknown_column = false;
    for (state, _) in first_states {
        let ResourceState::GoalUpdated {
            status,
            detail: Some(detail),
        } = state
        else {
            panic!("explicit native goal state");
        };
        statuses.push(status.as_str());
        let bytes = fixture
            .evidence
            .payload_bytes(detail, 64 * 1024)
            .await
            .unwrap();
        if bytes == RAW {
            saw_exact_capture = true;
        }
        let snapshot: NativeDatabaseSnapshot = serde_json::from_slice(&bytes).unwrap();
        if let Some(value) = snapshot.row.get("vendor_extension") {
            assert_eq!(value, &NativeDatabaseValue::Blob(vec![0, 255, 5]));
            saw_unknown_column = true;
        }
    }
    statuses.sort();
    assert_eq!(statuses, ["active", "paused"]);
    assert!(saw_exact_capture && saw_unknown_column);
    assert_eq!(reduced.resource_states[&goal("replacement-goal")].len(), 1);
    assert!(retained.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==DiagnosticCode::MissingSource)));
    let measurements: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|o| match o.declaration.known() {
            Some(TraceObject::Measurement(m)) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(measurements.len(), 7);
    let mut tokens = vec![];
    let mut seconds = vec![];
    for measurement in measurements {
        assert_eq!(
            measurement.aggregation_scope,
            AggregationScope::NamedSubject
        );
        assert!(measurement.components.is_empty());
        if measurement.metric == Metric::TotalTokens {
            tokens.push(measurement.value.as_i64().unwrap());
            assert_eq!(measurement.unit, MeasurementUnit::Tokens);
        }
        if measurement.metric == Metric::ReportedDuration {
            seconds.push(measurement.value.as_i64().unwrap());
            assert_eq!(measurement.unit, MeasurementUnit::Seconds);
        }
        if let MeasurementMode::Cumulative {
            reset_epoch: Some(epoch),
            ..
        } = &measurement.mode
        {
            assert_eq!(measurement.subject, AnyObjectRef::Resource(goal(epoch)));
        }
    }
    tokens.sort();
    seconds.sort();
    assert_eq!(tokens, [5, 2331556, 2331656]);
    assert_eq!(seconds, [2, 22870, 22872]);
    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&owner(), None, "objects"),
            None,
            None,
            None,
            32,
            false,
        )
        .await
        .unwrap();
    let details: Vec<ReadObjectDetail> = page
        .rows
        .iter()
        .map(|row| serde_json::from_value(row.body.clone()).unwrap())
        .collect();
    assert_eq!(
        details
            .iter()
            .filter(|detail| matches!(
                detail.content,
                ReadObjectContent::Resource {
                    resource_kind: ResourceKind::Goal,
                    ..
                }
            ))
            .count(),
        2
    );
    let resource = AnyObjectRef::Resource(goal(GOAL));
    let states = store
        .read_page(
            &snapshot,
            &read_scope(
                &owner(),
                None,
                &format!("object-observations:{}", resource.encode()),
            ),
            None,
            None,
            None,
            8,
            false,
        )
        .await
        .unwrap();
    let observations: Vec<ReadResourceObservation> = states
        .rows
        .into_iter()
        .map(|row| serde_json::from_value(row.body).unwrap())
        .collect();
    assert_eq!(observations.len(), 2);
    assert_eq!(
        observations
            .iter()
            .map(|row| match &row.state {
                ReadResourceState::GoalUpdated { status } => status.as_str(),
                _ => panic!("goal state kind"),
            })
            .collect::<Vec<_>>(),
        ["active", "paused"]
    );
    for observation in &observations {
        assert_eq!(observation.payloads.len(), 1);
        assert_eq!(observation.payloads[0].via_ref.object(), resource);
        assert_eq!(
            observation.payloads[0].body_revision,
            observation.claim_ref.encode()
        );
        assert!(observation.payloads[0].total_bytes.unwrap() > 0);
    }
    let mut reader_seconds: Vec<_> = details
        .iter()
        .filter_map(|detail| match &detail.content {
            ReadObjectContent::Measurement {
                metric: Metric::ReportedDuration,
                unit: MeasurementUnit::Seconds,
                value,
                mode: MeasurementMode::Cumulative { .. },
                ..
            } => value.as_i64(),
            _ => None,
        })
        .collect();
    reader_seconds.sort();
    assert_eq!(reader_seconds, seconds);
    let header = store
        .read_page(
            &snapshot,
            &read_scope(&owner(), None, "header"),
            None,
            None,
            None,
            1,
            false,
        )
        .await
        .unwrap();
    assert_eq!(header.rows.len(), 1);
}
async fn replay(order: SourceOrder, incremental: bool) -> Projection {
    let fixture = Fixture::for_vendor("codex").await;
    let (mut store, _) = fixture.generation("codex-goal").await;
    let mut captures = vec![];
    for (index, record) in records(order).iter().enumerate() {
        let capture = admit(&fixture, record).await;
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
                    max_unit_observations: 1 + index % 2,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            store = reopened(store).await;
        }
    }
    for mut capture in captures {
        capture.observation_ref.producer_id.push_str("-reread");
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
    }
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    expected(&fixture, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("codex-goal-rebuilt").await;
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
async fn native_goal_instances_and_counters_survive_public_events_reread_restart_and_rebuild() {
    assert_eq!(
        format!("{:x}", Sha256::digest(RAW)),
        "db8d4346e45813685a1909484a36874e3fb3a360f53807986a5e9250d5500879"
    );
    let cold = replay(SourceOrder::NativeFirst, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true).await, &cold);
}
