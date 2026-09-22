use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

const ROOT: &str = "01a083c2-1389-77c2-a16c-302aa2bbf884";
const CHILD: &str = "01a083fa-c22e-7dc3-9cc3-2b5c95bda28b";
const CHILD_TURNS: [&str; 2] = [
    "01a083fa-c28f-73e1-b4f0-3b021e45c9bd",
    "01a08457-6f7a-7641-b92f-af23f830f4ef",
];
const ROOT_BYTES: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/reuse/root.jsonl");
const CHILD_BYTES: &[u8] =
    include_bytes!("../../../../../../../fixtures/unified-trace/codex/reuse/child.jsonl");
fn owner(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{id}")).unwrap())
}
struct ReuseRecord {
    source: &'static str,
    seq: u64,
    line: u64,
    start: u64,
    end: u64,
    raw: &'static [u8],
}
fn records(order: SourceOrder) -> Vec<ReuseRecord> {
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../../../fixtures/unified-trace/codex/reuse/capture.json"
    ))
    .unwrap();
    let source = |name: &'static str, bytes: &'static [u8]| {
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            manifest["sources"][name]["retained_sha256"]
                .as_str()
                .unwrap()
        );
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .enumerate()
            .map(|(index, raw)| {
                let meta = &manifest["sources"][name]["records"][index];
                ReuseRecord {
                    source: name,
                    seq: index as u64 + 1,
                    line: meta["line"].as_u64().unwrap(),
                    start: meta["byte_start"].as_u64().unwrap(),
                    end: meta["byte_end"].as_u64().unwrap(),
                    raw,
                }
            })
            .collect::<Vec<_>>()
    };
    let root = source("root", ROOT_BYTES);
    let child = source("child", CHILD_BYTES);
    match order {
        SourceOrder::NativeFirst => root.into_iter().chain(child).collect(),
        SourceOrder::LiveFirst => child.into_iter().chain(root).collect(),
        SourceOrder::Alternating => {
            let mut root = root.into_iter();
            let mut child = child.into_iter();
            let mut all = vec![];
            loop {
                match (root.next(), child.next()) {
                    (None, None) => break,
                    (a, b) => all.extend(a.into_iter().chain(b)),
                }
            }
            all
        }
    }
}
async fn expected(fixture: &Fixture, store: &GenerationStore) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let objects: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| object.declaration.known())
        .collect();
    let agents: BTreeSet<_> = objects
        .iter()
        .filter_map(|object| match object {
            TraceObject::Agent(value) => Some(value.agent_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, [owner(ROOT), owner(CHILD)].into_iter().collect());
    let activations: Vec<_> = objects
        .iter()
        .filter_map(|object| match object {
            TraceObject::Activation(value) => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(activations.len(), 8);
    let child_activations: BTreeSet<_> = activations
        .iter()
        .filter(|value| {
            value.owner
                == OwnerRef::Agent {
                    agent: owner(CHILD),
                }
        })
        .map(|value| value.activation_ref.clone())
        .collect();
    assert_eq!(
        child_activations,
        CHILD_TURNS
            .iter()
            .map(
                |id| ActivationRef::native(owner(CHILD), "codex.turn", vec![(*id).into()]).unwrap()
            )
            .collect()
    );
    for activation in &child_activations {
        assert_eq!(
            reduced
                .execution(&AnyObjectRef::Activation(activation.clone()))
                .unwrap()
                .outcome
                .known(),
            Some(&TerminalOutcome::Succeeded),
        );
    }
    let routed: Vec<_> = objects
        .iter()
        .filter_map(|object| match object {
            TraceObject::Delivery(value)
                if matches!(value.route, DeliveryRoute::VendorMailbox { .. }) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect();
    assert_eq!(routed.len(), 11);
    let mut incoming = 0;
    for value in routed {
        let (from, to) = match &value.recipient {
            Recipient::Agent {
                agent: OwnerRef::Agent { agent },
            } if agent == &owner(CHILD) => {
                incoming += 1;
                (owner(ROOT), owner(CHILD))
            }
            Recipient::Agent {
                agent: OwnerRef::Agent { agent },
            } if agent == &owner(ROOT) => (owner(CHILD), owner(ROOT)),
            _ => panic!("recorded routing must resolve to the actual root or child"),
        };
        assert_eq!(
            value.sender,
            Author::Agent {
                agent: OwnerRef::Agent { agent: from }
            }
        );
        assert_eq!(
            value.recipient,
            Recipient::Agent {
                agent: OwnerRef::Agent { agent: to }
            }
        );
        assert_eq!(reduced.delivery_stages[&value.delivery_ref].len(), 1);
        assert!(matches!(
            reduced.delivery_stages[&value.delivery_ref][0].0,
            DeliveryStage::Admitted { .. }
        ));
    }
    assert_eq!(incoming, 3);
    let triggers: Vec<_> = reduced
        .relations
        .iter()
        .filter_map(|value| match &value.relation {
            Relation::Trigger { activation, .. } => Some(activation),
            _ => None,
        })
        .collect();
    assert_eq!(triggers.len(), 2);
    assert_eq!(
        triggers.into_iter().cloned().collect::<BTreeSet<_>>(),
        child_activations
    );
    for (id, body) in [
        (
            "msg_0fb98f7ef60eed6c016aa0ce3bf88887d1af7c283c81b1427c",
            b"SYLPH_REUSE_TEXT_bcca98f72684cb93".as_slice(),
        ),
        (
            "msg_0fb98f7ef60eed6c016aa0df30ef1887d18f8603769dcf8e9e",
            b"SYLPH_REUSE_TEXT_3c54ae6be34ed9df".as_slice(),
        ),
    ] {
        let message = MessageRef::native(owner(CHILD), "codex.message", vec![id.into()]).unwrap();
        let bodies = revision_bytes(fixture, store, &message).await;
        assert_eq!(bodies.len(), 2);
        assert!(bodies.iter().all(|value| value == body));
    }
    for id in [
        "amsg_01a083fa-c2a1-7803-b747-727a9b9b1734",
        "amsg_01a08457-6f87-7100-9fc5-9920c67256db",
    ] {
        let message = MessageRef::native(owner(CHILD), "codex.message", vec![id.into()]).unwrap();
        let bodies = revision_bytes(fixture, store, &message).await;
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].starts_with(b"SYLPH_REUSE_TEXT_bd1e5fafd732b198"));
    }
    assert!(!retained.iter().any(|claim| matches!(
        &claim.fact,
        Fact::DeliveryStage {
            stage: DeliveryStage::Included { .. },
            ..
        }
    )));
}
async fn replay(order: SourceOrder, incremental: bool) -> Projection {
    let fixture = Fixture::new().await;
    let (mut store, _) = fixture.generation("reuse").await;
    let mut captures = vec![];
    for record in records(order) {
        let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
        let capture = fixture
            .admit(
                ObservationRef {
                    issuer: issuer.clone(),
                    producer_id: record.source.into(),
                    producer_epoch: "retained".into(),
                    producer_seq: record.seq,
                },
                SourcePosition {
                    source: SourceRef {
                        issuer,
                        instance: record.source.into(),
                    },
                    generation: 0,
                    locator: SourceLocator::Jsonl {
                        byte_start: record.start,
                        byte_end: record.end,
                        record_ordinal: record.line - 1,
                    },
                },
                record.raw,
                TransportKind::NativeJsonl,
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
                    max_unit_observations: 1 + record.seq as usize % 3,
                    ..Default::default()
                })
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            if record.seq % 5 == 0 {
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
    let (fresh, _) = fixture.generation("reuse-rebuild").await;
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
async fn recorded_reusable_child_keeps_deliveries_and_activation_identity_across_source_order_and_restart()
 {
    let cold = replay(SourceOrder::NativeFirst, false).await;
    assert_projection(&replay(SourceOrder::LiveFirst, true).await, &cold);
    assert_projection(&replay(SourceOrder::Alternating, true).await, &cold);
}
