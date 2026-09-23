use super::*;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};

const NATIVE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/goal-context/native.jsonl");
const WIRE: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/goal-context/wire.jsonl");
const GOAL: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/goal-context/goal.json");
const PROVIDER: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/goal-context/provider.jsonl");
fn manifest() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../../../fixtures/unified-trace/codex/goal-context/capture.json"
    ))
    .unwrap()
}
fn owner() -> AgentRef {
    AgentRef::Session(
        SessionId::decode(&format!(
            "env7fk2:codex:{}",
            manifest()["session_id"].as_str().unwrap()
        ))
        .unwrap(),
    )
}
fn message(id: &str) -> MessageRef {
    MessageRef::native(owner(), "codex.message", vec![id.into()]).unwrap()
}
struct Record {
    source: &'static str,
    index: u64,
    offset: u64,
    kind: TransportKind,
    raw: Vec<u8>,
}
#[derive(Deserialize)]
struct Wire<'a> {
    direction: String,
    #[serde(borrow)]
    frame: &'a RawValue,
}
fn records(order: SourceOrder, native_only: bool) -> Vec<Record> {
    let manifest = manifest();
    for (name, raw) in [("native", NATIVE), ("wire", WIRE), ("provider", PROVIDER)] {
        assert_eq!(
            format!("{:x}", Sha256::digest(raw)),
            manifest["retained_sha256"][name].as_str().unwrap()
        );
    }
    assert_eq!(
        format!("{:x}", Sha256::digest(GOAL)),
        manifest["goal_snapshot"]["retained_sha256"]
            .as_str()
            .unwrap()
    );
    let mut offset = 0;
    let mut native: Vec<_> = NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .map(|(index, raw)| {
            let record = Record {
                source: "goal-context-native",
                index: index as u64,
                offset,
                kind: TransportKind::NativeJsonl,
                raw: raw.to_vec(),
            };
            offset += raw.len() as u64;
            record
        })
        .collect();
    native.push(Record {
        source: "goal-context-database",
        index: 0,
        offset: 0,
        kind: TransportKind::NativeDatabase,
        raw: GOAL.to_vec(),
    });
    let live: Vec<_> = if native_only {
        vec![]
    } else {
        WIRE.split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .enumerate()
            .map(|(index, raw)| {
                let frame: Wire<'_> = serde_json::from_slice(raw).unwrap();
                Record {
                    source: "goal-context-wire",
                    index: index as u64,
                    offset: 0,
                    kind: if frame.direction == "out" {
                        TransportKind::LiveStdin
                    } else {
                        TransportKind::LiveStdout
                    },
                    raw: frame.frame.get().as_bytes().to_vec(),
                }
            })
            .collect()
    };
    // Provider requests certify the experiment, but do not enter production
    // admission: the normal adapter cannot acquire that private metadata surface.
    let provider: Vec<Value> = PROVIDER
        .split(|byte| *byte == b'\n')
        .filter(|raw| !raw.is_empty())
        .map(|raw| serde_json::from_slice(raw).unwrap())
        .collect();
    let metadata: Vec<Value> = provider
        .iter()
        .map(|body| {
            serde_json::from_str(
                body["client_metadata"]["x-codex-turn-metadata"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(
        metadata
            .iter()
            .filter(|value| value["turn_trigger"] == "goal")
            .count(),
        2
    );
    assert!(metadata.iter().any(|value| value["turn_id"]
        == manifest["observed"]["literal_turn_id"]
        && value.get("turn_trigger").is_none()));
    match order {
        SourceOrder::NativeFirst => native.into_iter().chain(live).collect(),
        SourceOrder::LiveFirst => live.into_iter().chain(native).collect(),
        SourceOrder::Alternating => {
            let mut native = native.into_iter();
            let mut live = live.into_iter();
            let mut out = vec![];
            loop {
                match (native.next(), live.next()) {
                    (None, None) => break,
                    (a, b) => out.extend(a.into_iter().chain(b)),
                }
            }
            out
        }
    }
}
async fn admit(fixture: &Fixture, record: &Record) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: record.source.into(),
                producer_epoch: "retained".into(),
                producer_seq: record.index + 1,
            },
            SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: record.source.into(),
                },
                generation: 0,
                locator: if record.kind == TransportKind::NativeJsonl {
                    SourceLocator::Jsonl {
                        byte_start: record.offset,
                        byte_end: record.offset + record.raw.len() as u64,
                        record_ordinal: record.index,
                    }
                } else {
                    SourceLocator::Frame {
                        ordinal: record.index,
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
    let manifest = manifest();
    let observed = &manifest["observed"];
    let declaration = |id: &str| {
        let Some(TraceObject::Message(message)) = reduced.objects
            [&AnyObjectRef::Message(message(id))]
            .declaration
            .known()
        else {
            panic!("observed message disappeared")
        };
        message
    };
    for id in observed["goal_context_message_ids"].as_array().unwrap() {
        let value = declaration(id.as_str().unwrap());
        assert_eq!(value.protocol_role, Some(ProtocolRole::User));
        assert_eq!(
            value.author,
            Author::Harness {
                identity: Some("codex".into())
            }
        );
        assert_eq!(value.purpose, MessagePurpose::Context);
    }
    let notice = declaration("msg_01a0cbf1-b562-7321-b225-d745fec934ff");
    assert_eq!(notice.purpose, MessagePurpose::Notice);
    assert_eq!(
        notice.author,
        Author::Harness {
            identity: Some("codex".into())
        }
    );
    let typed_input = declaration("01a0cbf1-b578-74d0-b3f6-54757f23138a");
    assert_eq!(typed_input.purpose, MessagePurpose::TaskInput);
    assert_eq!(typed_input.author, Author::Unknown);
    let literal_id = observed["literal_message_id"].as_str().unwrap();
    assert_eq!(declaration(literal_id).purpose, MessagePurpose::TaskInput);
    assert_eq!(declaration(literal_id).author, Author::Unknown);
    let original = observed["equal_goal_text_message_ids"][0].as_str().unwrap();
    let goal_bytes = revision_bytes(fixture, store, &message(original)).await;
    let literal_bytes = revision_bytes(fixture, store, &message(literal_id)).await;
    assert!(!goal_bytes.is_empty());
    assert_eq!(
        literal_bytes, goal_bytes,
        "purpose cannot be selected by body text"
    );
    assert_ne!(message(original), message(literal_id));
    assert!(
        !retained.iter().any(|claim| matches!(
            claim.fact,
            Fact::Relation {
                relation: Relation::Trigger { .. }
            }
        )),
        "context annotations and private research witnesses cannot fabricate a goal trigger"
    );
    let turns = observed["native_turn_ids"].as_array().unwrap();
    assert_eq!(turns.len(), 3);
    for (id, expected) in turns.iter().zip([
        TerminalOutcome::Succeeded,
        TerminalOutcome::Interrupted,
        TerminalOutcome::Succeeded,
    ]) {
        let reference =
            ActivationRef::native(owner(), "codex.turn", vec![id.as_str().unwrap().into()])
                .unwrap();
        assert_eq!(
            reduced
                .execution(&AnyObjectRef::Activation(reference))
                .unwrap()
                .outcome
                .known(),
            Some(&expected)
        );
    }
    let snapshot: sylph_protocol::NativeDatabaseSnapshot = serde_json::from_slice(GOAL).unwrap();
    let sylph_protocol::NativeDatabaseValue::Text(id) = &snapshot.row["goal_id"] else {
        panic!("recorded goal identity")
    };
    let resource = ResourceRef::native(owner(), "codex.goal", vec![id.clone()]).unwrap();
    assert_eq!(
        reduced.resource_states[&resource].len(),
        1,
        "public goal states have no instance certificate and cannot join by objective"
    );
    assert!(reduced.resource_states[&resource].iter().any(|(state, _)| matches!(state, ResourceState::GoalUpdated { status, .. } if status == "paused")));
    let goal_tokens: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|value| match value.declaration.known() {
            Some(TraceObject::Measurement(value))
                if value.subject == AnyObjectRef::Resource(resource.clone())
                    && value.metric == Metric::TotalTokens =>
            {
                Some(value.value.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(goal_tokens, vec![serde_json::Number::from(11)]);
    let view = store.checkpoint().await.unwrap().snapshot;
    let detail = store
        .object(&view, &AnyObjectRef::Message(message(original)))
        .await
        .unwrap();
    assert!(matches!(
        detail.object,
        TraceObject::Message(Message {
            purpose: MessagePurpose::Context,
            ..
        })
    ));
}
async fn replay(order: SourceOrder, incremental: bool, native_only: bool) -> Projection {
    let fixture = Fixture::new().await;
    let (mut store, _) = fixture.generation("goal-context").await;
    let mut captures = vec![];
    for record in records(order, native_only) {
        let capture = admit(&fixture, &record).await;
        fixture
            .evidence
            .admit_batch(
                &capture.observation_ref.issuer,
                std::slice::from_ref(&capture),
            )
            .await
            .unwrap();
        assert_eq!(
            fixture
                .evidence
                .capture_payload(&capture.observation_ref, record.raw.len())
                .await
                .unwrap(),
            Some(record.raw)
        );
        captures.push(capture);
        if incremental {
            fixture
                .worker_with(WorkerLimits {
                    max_unit_observations: 1 + record.index as usize % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if record.index % 4 == 0 {
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
    let progress = fixture.worker().catch_up(&store, 1, cut).await.unwrap();
    assert_eq!(progress.suspended_sources, 0);
    expected(&fixture, &store).await;
    let before = projection(&fixture, &store).await;
    let (fresh, _) = fixture.generation("goal-context-rebuild").await;
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
async fn captured_goal_context_and_ordinary_copy_survive_public_native_order_restart_and_rebuild() {
    let cold = replay(SourceOrder::NativeFirst, false, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true, false).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true, false).await, &cold);
    replay(SourceOrder::NativeFirst, true, true).await;
}
