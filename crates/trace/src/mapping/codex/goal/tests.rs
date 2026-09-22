use super::*;
use crate::mapping::{ArchiveContext, MapperState};
use crate::reducer::Reducer;
use sylph_protocol::EnvironmentId;

const RAW: &str = include_str!("../../../../../../fixtures/unified-trace/codex/goal/goal.json");
const THREAD: &str = "01a0c6ff-3eff-7972-aefa-f4ad63d06308";
const GOAL: &str = "a22392d7-2781-496a-b85a-07f9192533e0";
fn owner() -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env:codex:{THREAD}")).unwrap())
}
fn snapshot() -> NativeDatabaseSnapshot {
    serde_json::from_str(RAW).unwrap()
}
fn capture(seq: u64, kind: TransportKind) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env").unwrap());
    Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: "goal".into(),
            producer_epoch: "captured".into(),
            producer_seq: seq,
        },
        source_position: SourcePosition {
            source: SourceRef {
                issuer,
                instance: if kind == TransportKind::NativeDatabase {
                    "goal-database"
                } else {
                    "rollout"
                }
                .into(),
            },
            generation: if kind == TransportKind::NativeDatabase {
                seq - 1
            } else {
                0
            },
            locator: if kind == TransportKind::NativeDatabase {
                SourceLocator::Snapshot {
                    revision: (seq - 1).to_string(),
                }
            } else {
                SourceLocator::Jsonl {
                    byte_start: seq * 100,
                    byte_end: seq * 100 + 100,
                    record_ordinal: seq - 1,
                }
            },
        },
        ingestion_authorization_epoch: IngestionEpoch::new("test").unwrap(),
        declared_subject_refs: vec![AgentRef::Session(
            SessionId::decode("env:codex:wrong-routing-hint").unwrap(),
        )],
        captured_at: TimeEvidence {
            domain: "receipt".into(),
            value: seq as i64,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::ServerReceipt,
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
fn map(seq: u64, snapshot: &NativeDatabaseSnapshot) -> MapOutput {
    let capture = capture(seq, TransportKind::NativeDatabase);
    CodexMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(snapshot).unwrap(),
        state: &MapperState::default(),
        archive: &ArchiveContext::default(),
    })
}
fn objects(output: &MapOutput) -> impl Iterator<Item = &TraceObject> {
    output.claims.iter().filter_map(|claim| match &claim.fact {
        Fact::Declare { object } => Some(object),
        _ => None,
    })
}
fn resource(output: &MapOutput) -> ResourceRef {
    objects(output)
        .find_map(|object| match object {
            TraceObject::Resource(value) => Some(value.resource_ref.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn recorded_goal_keeps_native_instance_and_reported_counter_units_without_execution_or_zero_budget()
{
    let output = map(1, &snapshot());
    let reference = resource(&output);
    assert_eq!(
        reference,
        ResourceRef::native(owner(), "codex.goal", vec![GOAL.into()]).unwrap()
    );
    assert!(objects(&output)
        .any(|object| matches!(object,TraceObject::Agent(agent)if agent.agent_ref==owner())));
    assert!(!objects(&output).any(|object| matches!(
        object,
        TraceObject::Activation(_) | TraceObject::Message(_) | TraceObject::Delivery(_)
    )));
    assert!(!output
        .claims
        .iter()
        .any(|claim| matches!(claim.fact, Fact::Execution { .. })));
    let measurements: Vec<_> = objects(&output)
        .filter_map(|object| match object {
            TraceObject::Measurement(value) => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(
        measurements.len(),
        2,
        "a null budget is not a zero-valued measurement"
    );
    for (metric, unit, value) in [
        (Metric::TotalTokens, MeasurementUnit::Tokens, 2331556),
        (Metric::ReportedDuration, MeasurementUnit::Seconds, 22870),
    ] {
        let measurement = measurements.iter().find(|m| m.metric == metric).unwrap();
        assert_eq!(measurement.value.as_u64(), Some(value));
        assert_eq!(measurement.unit, unit);
        assert_eq!(
            measurement.subject,
            AnyObjectRef::Resource(reference.clone())
        );
        assert_eq!(
            measurement.aggregation_scope,
            AggregationScope::NamedSubject
        );
        assert!(
            matches!(&measurement.mode,MeasurementMode::Cumulative{counter_domain:Some(domain),reset_epoch:Some(epoch),sample_position}if domain.contains(GOAL)&&epoch==GOAL&&sample_position==&capture(1,TransportKind::NativeDatabase).source_position)
        );
        assert!(
            measurement.components.is_empty(),
            "no inferred zero sample or disjointness certificate"
        );
    }
    assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::ResourceState{resource,state:ResourceState::GoalUpdated{status,detail:Some(detail)}}if resource==&reference&&status=="active"&&detail==&capture(1,TransportKind::NativeDatabase).payload_ref)));
    assert!(output.diagnostics.is_empty());
}

#[test]
fn replacing_a_goal_with_equal_objective_keeps_two_resources_and_distinct_counter_domains() {
    let original = snapshot();
    let mut renamed = original.clone();
    renamed.row.insert(
        "objective".into(),
        NativeDatabaseValue::Text("Changed objective, same native goal".into()),
    );
    assert_eq!(resource(&map(1, &original)), resource(&map(2, &renamed)));
    let mut replacement = original.clone();
    replacement.row.insert(
        "goal_id".into(),
        NativeDatabaseValue::Text("replacement-goal".into()),
    );
    replacement
        .row
        .insert("token_budget".into(), NativeDatabaseValue::Integer(800));
    replacement
        .row
        .insert("tokens_used".into(), NativeDatabaseValue::Integer(5));
    let outputs = [map(1, &original), map(2, &replacement)];
    let claims: Vec<_> = outputs.iter().flat_map(|o| o.claims.clone()).collect();
    let evidence = claims.iter().flat_map(|c| c.evidence.clone()).collect();
    let mut reducer = Reducer::default();
    reducer.apply(&claims, &evidence, 100).unwrap();
    let reduced = reducer.resolve();
    assert_eq!(
        reduced
            .objects
            .values()
            .filter(|object| matches!(object.declaration.known(), Some(TraceObject::Resource(_))))
            .count(),
        2
    );
    assert!(reduced.executions.is_empty());
    let counters: Vec<_> = outputs
        .iter()
        .flat_map(objects)
        .filter_map(|o| match o {
            TraceObject::Measurement(m) if m.metric == Metric::TotalTokens => Some(m),
            _ => None,
        })
        .collect();
    assert_ne!(counters[0].subject, counters[1].subject);
    let (
        MeasurementMode::Cumulative {
            counter_domain: first_domain,
            reset_epoch: first_reset,
            ..
        },
        MeasurementMode::Cumulative {
            counter_domain: second_domain,
            reset_epoch: second_reset,
            ..
        },
    ) = (&counters[0].mode, &counters[1].mode)
    else {
        panic!("both are cumulative native counters");
    };
    assert_ne!(first_domain, second_domain);
    assert_eq!(first_reset.as_deref(), Some(GOAL));
    assert_eq!(second_reset.as_deref(), Some("replacement-goal"));
    assert!(objects(&outputs[1]).any(|o|matches!(o,TraceObject::Measurement(m)if m.value.as_u64()==Some(800)&&m.mode==MeasurementMode::Gauge)));
}

#[test]
fn malformed_goal_fields_remain_unknown_without_fabricated_status_counters_or_identity() {
    for value in [
        None,
        Some(NativeDatabaseValue::Null),
        Some(NativeDatabaseValue::Integer(42)),
        Some(NativeDatabaseValue::Text(String::new())),
    ] {
        let mut raw = snapshot();
        raw.row.remove("status");
        if let Some(value) = value {
            raw.row.insert("status".into(), value);
        }
        raw.row
            .insert("tokens_used".into(), NativeDatabaseValue::Text("42".into()));
        raw.row
            .insert("time_used_seconds".into(), NativeDatabaseValue::Integer(-1));
        let output = map(1, &raw);
        assert!(output.claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::ResourceState {
                state: ResourceState::Unknown { .. },
                ..
            }
        )));
        assert!(!objects(&output).any(|object| matches!(object, TraceObject::Measurement(_))));
        assert!(output
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == DiagnosticCode::DecodeFailure));
        assert_eq!(output.diagnostics.len(), 3);
    }
    for field in ["goal_id", "thread_id"] {
        let mut raw = snapshot();
        raw.row
            .insert(field.into(), NativeDatabaseValue::Text(String::new()));
        let output = map(1, &raw);
        assert!(output.claims.is_empty());
        assert!(output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingOwner));
    }
    for status in [
        NativeDatabaseStatus::MissingDatabase,
        NativeDatabaseStatus::MissingRow,
        NativeDatabaseStatus::Unreadable {
            reason: "held by vendor".into(),
        },
    ] {
        let mut raw = snapshot();
        raw.status = status;
        raw.row.clear();
        let output = map(1, &raw);
        assert!(output.claims.is_empty());
        assert!(output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::MissingSource));
    }
}

#[test]
fn public_goal_state_keeps_its_missing_instance_even_when_a_database_goal_exists() {
    let snapshot = snapshot();
    let NativeDatabaseValue::Text(objective) = &snapshot.row["objective"] else {
        panic!("recorded goal objective");
    };
    let recorded = map(1, &snapshot);
    let mut archive = ArchiveContext::default();
    archive.correlations.extend(recorded.correlations);
    archive
        .correlation_evidence
        .extend(recorded.correlation_evidence);
    archive
        .observations
        .insert(capture(1, TransportKind::NativeDatabase).observation_ref);
    let capture_header = capture(1, TransportKind::NativeJsonl);
    let header = serde_json::json!({"type":"session_meta","payload":{"id":THREAD}});
    let initial = CodexMapper.map(MapInput {
        capture: &capture_header,
        canonical_observation: &capture_header.observation_ref,
        raw: &serde_json::to_vec(&header).unwrap(),
        state: &MapperState::default(),
        archive: &archive,
    });
    for (outer, inner, expected) in [
        (THREAD, Some(THREAD), DiagnosticCode::MissingSource),
        (
            "another-thread",
            Some(THREAD),
            DiagnosticCode::ConflictingOwner,
        ),
        (
            THREAD,
            Some("another-thread"),
            DiagnosticCode::ConflictingOwner,
        ),
        (THREAD, None, DiagnosticCode::DecodeFailure),
    ] {
        let capture = capture(2, TransportKind::NativeJsonl);
        let goal = inner.map(|inner| serde_json::json!({"threadId":inner,"objective":objective,"status":"complete"})).unwrap_or(Value::Null);
        let value = serde_json::json!({"type":"event_msg","payload":{"type":"thread_goal_updated","threadId":outer,"goal":goal}});
        let output = CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(&value).unwrap(),
            state: &initial.state,
            archive: &archive,
        });
        assert!(!objects(&output).any(|object| matches!(
            object,
            TraceObject::Resource(_) | TraceObject::Activation(_)
        )));
        assert!(!output.claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::ResourceState { .. } | Fact::Execution { .. }
        )));
        assert!(output.claims.iter().any(|claim|matches!(&claim.fact,Fact::Diagnostic{diagnostic}if diagnostic.code==expected)),"expected {expected:?}");
    }
}
