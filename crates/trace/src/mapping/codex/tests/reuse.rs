use super::*;

const CHILD: &str = "01a083fa-c22e-7dc3-9cc3-2b5c95bda28b";
const ROOT: &str = "01a083c2-1389-77c2-a16c-302aa2bbf884";
fn child_rows() -> Vec<Value> {
    include_str!("../../../../../../fixtures/unified-trace/codex/reuse/child.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn recorded_reused_child_keeps_root_delivery_identity_and_independent_activations() {
    let output = run(&child_rows(), false);
    let reduced = reduce(&output);
    let deliveries: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Delivery(delivery)) => Some(delivery),
            _ => None,
        })
        .collect();
    assert_eq!(deliveries.len(), 3);
    for delivery in deliveries {
        assert_eq!(
            delivery.sender,
            Author::Agent {
                agent: OwnerRef::Agent { agent: agent(ROOT) }
            }
        );
        assert_eq!(
            delivery.recipient,
            Recipient::Agent {
                agent: OwnerRef::Agent {
                    agent: agent(CHILD)
                }
            }
        );
        assert_eq!(reduced.delivery_stages[&delivery.delivery_ref].len(), 1);
        assert!(matches!(
            reduced.delivery_stages[&delivery.delivery_ref][0].0,
            DeliveryStage::Admitted { .. }
        ));
    }
    let activations: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::Activation(value)) => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(activations.len(), 2);
    assert!(activations.iter().all(|value| value.owner
        == OwnerRef::Agent {
            agent: agent(CHILD)
        }));
    let triggers: Vec<_> = reduced
        .relations
        .iter()
        .filter_map(|relation| match &relation.relation {
            Relation::Trigger { activation, .. } => Some(activation.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(triggers.len(), 2);
    assert_eq!(
        triggers.into_iter().collect::<std::collections::BTreeSet<_>>(),
        [
            activation_ref(&agent(CHILD), "01a083fa-c28f-73e1-b4f0-3b021e45c9bd"),
            activation_ref(&agent(CHILD), "01a08457-6f7a-7641-b92f-af23f830f4ef")
        ]
        .into_iter()
        .collect()
    );
}
#[test]
fn a_child_cannot_supply_a_missing_root_path_identity_or_override_an_evidenced_root() {
    for conflicting in [false, true] {
        let mut rows = child_rows();
        if conflicting {
            rows[0]["payload"]["agent_path"] = "/root".into();
        } else {
            rows[0]["payload"]
                .as_object_mut()
                .unwrap()
                .remove("session_id");
        }
        let output = run(&rows, false);
        let reduced = reduce(&output);
        for object in reduced.objects.values() {
            if let Some(TraceObject::Delivery(value)) = object.declaration.known() {
                assert!(
                    matches!(
                        &value.sender,
                        Author::Agent {
                            agent: OwnerRef::Unresolved { .. }
                        }
                    ),
                    "missing or contradictory root evidence cannot name the child as root"
                );
            }
        }
    }
}

#[test]
fn independent_route_certificates_refine_immutable_claims_in_either_arrival_order() {
    let header = serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"observer","session_id":"root","agent_path":"/root/observer"}});
    let initial = run(&[header], false).remove(0);
    let raw = serde_json::to_vec(&serde_json::json!({"type":"response_item","ordinal":1,"payload":{"type":"agent_message","id":"routed","author":"/root/sender","recipient":"/root/recipient","content":[{"type":"input_text","text":"same routed bytes"}]}})).unwrap();
    let observation = capture(2, false);
    let binding =
        |id: &str| correlation_key(&agent("root"), "codex.agent-path", &[format!("/root/{id}")]);
    let mut complete = ArchiveContext::default();
    for (id, sequence) in [("sender", 8), ("recipient", 9)] {
        complete
            .correlations
            .insert(binding(id), AnyObjectRef::Agent(agent(id)));
        complete
            .correlation_evidence
            .insert(binding(id), vec![capture(sequence, false).observation_ref]);
    }
    let map = |archive: &ArchiveContext| {
        CodexMapper.map(MapInput {
            capture: &observation,
            canonical_observation: &observation.observation_ref,
            raw: &raw,
            state: &initial.state,
            archive,
        })
    };
    let full = map(&complete);
    let expected: BTreeMap<_, _> = full
        .claims
        .iter()
        .map(|claim| (claim.claim_ref.clone(), claim.clone()))
        .collect();
    for first in ["sender", "recipient"] {
        let mut partial = ArchiveContext::default();
        partial.correlations.insert(
            binding(first),
            complete.correlations[&binding(first)].clone(),
        );
        partial.correlation_evidence.insert(
            binding(first),
            complete.correlation_evidence[&binding(first)].clone(),
        );
        let mut outputs = vec![
            initial.clone(),
            map(&ArchiveContext::default()),
            map(&partial),
            full.clone(),
        ];
        let all: BTreeMap<_, _> = outputs[1..]
            .iter()
            .flat_map(|output| {
                output
                    .claims
                    .iter()
                    .map(|claim| (claim.claim_ref.clone(), claim.clone()))
            })
            .collect();
        assert_eq!(
            all, expected,
            "cold replay retains every immutable refinement"
        );
        let reduced = reduce(&outputs);
        let messages: Vec<_> = reduced
            .objects
            .values()
            .filter_map(|object| match object.declaration.known() {
                Some(TraceObject::Message(value)) => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages.len(),
            1,
            "resolving authorship cannot move the message address"
        );
        assert_eq!(
            messages[0].author,
            Author::Agent {
                agent: OwnerRef::Agent {
                    agent: agent("sender")
                }
            }
        );
        let delivery = reduced
            .objects
            .values()
            .find_map(|object| match object.declaration.known() {
                Some(TraceObject::Delivery(value)) => Some(value),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            delivery.sender,
            Author::Agent {
                agent: OwnerRef::Agent {
                    agent: agent("sender")
                }
            }
        );
        assert_eq!(
            delivery.recipient,
            Recipient::Agent {
                agent: OwnerRef::Agent {
                    agent: agent("recipient")
                }
            }
        );
        assert!(
            full.claims
                .iter()
                .filter(|claim| !claim.supersedes.is_empty())
                .all(|claim| claim.evidence.len() >= 3)
        );
        outputs.reverse();
        assert_eq!(reduce(&outputs), reduced);
    }
}
