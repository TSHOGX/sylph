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
        complete.retain_correlation_evidence(
            binding(id),
            vec![capture(sequence, false).observation_ref],
        );
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
        partial.correlation_proofs.insert(
            binding(first),
            complete.correlation_proofs[&binding(first)].clone(),
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
        for object in reduced.objects.values() {
            if let Some(TraceObject::HistoryEntry(entry)) = object.declaration.known() {
                assert!(
                    entry.original_owner.is_none(),
                    "late sender certificates cannot manufacture copied-history lineage"
                );
            }
        }
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

#[test]
fn corroborating_route_proofs_cannot_reuse_an_immutable_claim_identity() {
    let header = serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"observer","session_id":"root","agent_path":"/root/observer"}});
    let initial = run(&[header], false).remove(0);
    let raw = serde_json::to_vec(&serde_json::json!({"type":"response_item","ordinal":1,"payload":{"type":"agent_message","id":"corroborated","author":"/root/sender","recipient":"/root","content":[{"type":"input_text","text":"independently proved sender"}]}})).unwrap();
    let observation = capture(2, false);
    let key = correlation_key(&agent("root"), "codex.agent-path", &["/root/sender".into()]);
    for order in [[8, 9], [9, 8]] {
        let mut archive = ArchiveContext::default();
        let mut retained = BTreeMap::new();
        for sequence in order {
            archive
                .correlations
                .insert(key.clone(), AnyObjectRef::Agent(agent("sender")));
            archive.retain_correlation_evidence(
                key.clone(),
                vec![capture(sequence, false).observation_ref],
            );
            let output = CodexMapper.map(MapInput {
                capture: &observation,
                canonical_observation: &observation.observation_ref,
                raw: &raw,
                state: &initial.state,
                archive: &archive,
            });
            for claim in output.claims {
                if let Some(prior) = retained.insert(claim.claim_ref.clone(), claim.clone()) {
                    assert_eq!(
                        prior, claim,
                        "a corroborating identity proof changed an already retained claim"
                    );
                }
            }
        }
    }
}

#[test]
fn routing_certificate_pairs_page_without_remapping_bodies_or_skipping_alternative_support() {
    use crate::mapping::owner_resolution::{retain_plan, OWNER_PAGE_WORK};
    let initial = run(&[serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"observer","session_id":"root","agent_path":"/root/observer"}})], false).remove(0);
    let raw = serde_json::to_vec(&serde_json::json!({"type":"response_item","ordinal":1,"payload":{"type":"agent_message","id":"paged-certificates","author":"/root/sender","recipient":"/root/recipient","content":[{"type":"input_text","text":"retain this body only once"}]}})).unwrap();
    let observation = capture(2, false);
    let count = OWNER_PAGE_WORK / 2 + 3;
    let mut archive = ArchiveContext::default();
    for (id, start) in [("sender", 100), ("recipient", 1000)] {
        let key = correlation_key(&agent("root"), "codex.agent-path", &[format!("/root/{id}")]);
        archive
            .correlations
            .insert(key.clone(), AnyObjectRef::Agent(agent(id)));
        for sequence in start..start + count as u64 {
            archive.retain_correlation_evidence(
                key.clone(),
                vec![capture(sequence, false).observation_ref],
            );
        }
    }
    let mut state = initial.state;
    let mut pages = 0;
    let mut bodies = 0;
    let mut retained = BTreeMap::new();
    loop {
        let mut output = CodexMapper.map(MapInput {
            capture: &observation,
            canonical_observation: &observation.observation_ref,
            raw: &raw,
            state: &state,
            archive: &archive,
        });
        if pages > 0 {
            assert!(
                output.payloads.is_empty(),
                "a metadata continuation restaged the original body"
            );
            assert!(
                output.claims.len() <= OWNER_PAGE_WORK,
                "one routing continuation exceeded its work budget"
            );
        }
        bodies += usize::from(!output.payloads.is_empty());
        pages += 1;
        assert!(
            pages < count * count,
            "routing continuation failed to advance"
        );
        for claim in output.claims {
            if let Some(prior) = retained.insert(claim.claim_ref.clone(), claim.clone()) {
                assert_eq!(
                    prior, claim,
                    "a paged certificate changed an immutable claim"
                );
            }
            archive.claims.insert(claim.claim_ref.clone(), claim);
        }
        for plan in output.owner_resolution_plans {
            retain_plan(&mut archive.owner_resolution_plans, plan);
        }
        if !output.requests.iter().any(ArchiveRequest::is_continuation) {
            break;
        }
        state = output
            .continuation_state
            .take()
            .expect("continuation needs its exact next coordinate");
    }
    assert!(pages > 1);
    assert_eq!(bodies, 1);
    // Message authorship has one axis; delivery sender/recipient has both.
    // Local history ownership is independent and needs no routing refinements.
    assert_eq!(
        retained
            .values()
            .filter(|claim| !claim.supersedes.is_empty())
            .count(),
        count * count + 3 * count
    );
    for claim in retained
        .values()
        .filter(|claim| !claim.supersedes.is_empty())
    {
        for prior in &claim.supersedes {
            assert!(
                retained.contains_key(prior),
                "a joint certificate superseded an unrecorded subset"
            );
        }
    }
}

#[test]
fn a_malformed_routing_checkpoint_reports_a_decode_failure_instead_of_panicking() {
    let observation = capture(2, false);
    let mut state = MapperState::default();
    state.values.insert(
        "owner-resolution-continuation".into(),
        serde_json::json!({"after":"damaged"}),
    );
    let output = CodexMapper.map(MapInput {
        capture: &observation,
        canonical_observation: &observation.observation_ref,
        raw: b"{}",
        state: &state,
        archive: &ArchiveContext::default(),
    });
    assert!(output.claims.is_empty());
    assert!(output.diagnostics.iter().any(|diagnostic| diagnostic.code
        == DiagnosticCode::DecodeFailure
        && diagnostic.evidence.contains(&observation.observation_ref)));
}

#[test]
fn received_messages_after_the_inherited_prefix_do_not_become_inherited_entries() {
    let rows = child_rows();
    let boundary = rows[0]["payload"]["subagent_history_start_ordinal"]
        .as_u64()
        .unwrap();
    let received: Vec<_> = rows
        .iter()
        .filter(|row| row["payload"]["type"] == "agent_message")
        .collect();
    assert!(
        received
            .iter()
            .all(|row| row["ordinal"].as_u64().unwrap() >= boundary)
    );
    let reduced = reduce(&run(&rows, false));
    let mut admitted = 0;
    for stages in reduced.delivery_stages.values() {
        for (stage, _) in stages {
            let DeliveryStage::Admitted { entry: Some(entry) } = stage else {
                continue;
            };
            let Some(TraceObject::HistoryEntry(value)) = reduced.objects
                [&AnyObjectRef::HistoryEntry(entry.clone())]
                .declaration
                .known()
            else {
                panic!("received entry must remain readable")
            };
            assert_eq!(value.containing_agent, agent(CHILD));
            assert!(
                value.original_owner.is_none(),
                "sender identity is not copied-history lineage"
            );
            admitted += 1;
        }
    }
    assert_eq!(admitted, received.len());
}

#[test]
fn copied_routed_content_keeps_lineage_without_becoming_the_childs_delivery_or_authorship() {
    let mut received: Value =
        include_str!("../../../../../../fixtures/unified-trace/codex/reuse/child.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|value| value["payload"]["type"] == "agent_message")
            .unwrap();
    let id = received["payload"]["id"].as_str().unwrap().to_owned();
    let path = received["payload"]["author"].clone();
    let body = received["payload"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    received["ordinal"] = 2.into();
    let values = [
        serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"copy-reader","session_id":"copy-reader","agent_path":path,"forked_from_id":CHILD,"subagent_history_start_ordinal":3}}),
        serde_json::json!({"type":"inter_agent_communication_metadata","ordinal":1,"payload":{"trigger_turn":true}}),
        received,
    ];
    let outputs = run(&values, false);
    let reduced = reduce(&outputs);
    let reference = MessageRef::native(agent(CHILD), "codex.message", vec![id]).unwrap();
    let Some(TraceObject::Message(message)) = reduced
        .object(&AnyObjectRef::Message(reference))
        .and_then(|value| value.declaration.known())
    else {
        panic!("copied routed bytes disappeared from normalized history")
    };
    assert_eq!(
        message.author,
        Author::Unknown,
        "the new child's path cannot identify an inherited speaker"
    );
    assert_eq!(message.purpose, MessagePurpose::TaskInput);
    assert!(
        outputs
            .iter()
            .flat_map(|output| &output.payloads)
            .any(|payload| payload.bytes == body)
    );
    let entries: Vec<_> = reduced
        .objects
        .values()
        .filter_map(|value| match value.declaration.known() {
            Some(TraceObject::HistoryEntry(entry)) => Some(entry),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].containing_agent, agent("copy-reader"));
    assert_eq!(
        entries[0].original_owner,
        Some(OwnerRef::Agent {
            agent: agent(CHILD)
        })
    );
    assert!(reduced.delivery_stages.is_empty());
    assert!(reduced.executions.is_empty());
    assert!(
        !reduced
            .objects
            .values()
            .any(|value| matches!(value.declaration.known(), Some(TraceObject::Delivery(_))))
    );
    assert!(
        !reduced
            .relations
            .iter()
            .any(|value| matches!(value.relation, Relation::Trigger { .. }))
    );
}

#[test]
fn communication_trigger_metadata_requires_the_immediately_preceding_retained_record() {
    for gap in [false, true] {
        let initial = run(&[
            serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"root","session_id":"root","agent_path":"/root"}}),
            serde_json::json!({"type":"event_msg","ordinal":1,"payload":{"type":"task_started","turn_id":"active"}}),
        ], false).pop().unwrap();
        let metadata_capture = capture(3, false);
        let metadata = CodexMapper.map(MapInput { capture: &metadata_capture, canonical_observation: &metadata_capture.observation_ref,
            raw: br#"{"type":"inter_agent_communication_metadata","ordinal":2,"payload":{"trigger_turn":true}}"#, state: &initial.state, archive: &ArchiveContext::default() });
        let mut state = metadata.state;
        if !gap {
            let header_capture = capture(4, false);
            state = CodexMapper.map(MapInput { capture: &header_capture, canonical_observation: &header_capture.observation_ref,
                raw: br#"{"type":"session_meta","ordinal":3,"payload":{"id":"root","session_id":"root"}}"#, state: &state, archive: &ArchiveContext::default() }).state;
        }
        let message_capture = capture(5, false);
        let output = CodexMapper.map(MapInput { capture: &message_capture, canonical_observation: &message_capture.observation_ref,
            raw: br#"{"type":"response_item","ordinal":4,"payload":{"type":"agent_message","id":"not-adjacent","author":"/root","recipient":"/root","content":[{"type":"input_text","text":"unproved trigger"}]}}"#, state: &state, archive: &ArchiveContext::default() });
        let message = output
            .claims
            .iter()
            .find_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::Message(message),
                } => Some(message),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            message.purpose,
            MessagePurpose::Unclassified,
            "an intervening record or an unobserved gap broke the metadata pair"
        );
        assert!(!output.claims.iter().any(|claim| matches!(
            claim.fact,
            Fact::Relation {
                relation: Relation::Trigger { .. }
            }
        )));
    }
}

#[test]
fn encrypted_communication_parts_are_known_opaque_values_and_missing_ciphertext_is_diagnosed() {
    for present in [true, false] {
        let rows = child_rows();
        let mut received = rows
            .iter()
            .find(|value| value["payload"]["type"] == "agent_message")
            .unwrap()
            .clone();
        let opaque = &mut received["payload"]["content"][1];
        assert_eq!(opaque["type"], "encrypted_content");
        assert!(opaque["encrypted_content"].is_string());
        if !present {
            opaque.as_object_mut().unwrap().remove("encrypted_content");
        }
        let expected = opaque.clone();
        let outputs = run(&[rows[0].clone(), received], false);
        let output = &outputs[1];
        assert!(!output.claims.iter().any(|claim| matches!(&claim.fact, Fact::Diagnostic { diagnostic } if diagnostic.code == DiagnosticCode::UnsupportedKind)), "the vendor's encrypted content kind is supported as opaque");
        assert_eq!(output.claims.iter().any(|claim| matches!(&claim.fact, Fact::Diagnostic { diagnostic } if diagnostic.code == DiagnosticCode::DecodeFailure)), !present);
        let revision = output
            .claims
            .iter()
            .find_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::MessageRevision(revision),
                } => Some(revision),
                _ => None,
            })
            .unwrap();
        assert_eq!(revision.parts[1].kind, ContentKind::Opaque);
        let payload = output
            .payloads
            .iter()
            .find(|payload| payload.reference == revision.parts[1].payload)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&payload.bytes).unwrap(),
            expected
        );
    }
}

#[test]
fn a_known_local_path_cannot_bypass_conflicting_root_scoped_certificates() {
    let initial = run(&[serde_json::json!({"type":"session_meta","ordinal":0,"payload":{"id":"sender","session_id":"root","agent_path":"/root/sender"}})], false).remove(0);
    let mut archive = ArchiveContext::default();
    let key = correlation_key(&agent("root"), "codex.agent-path", &["/root/sender".into()]);
    for (name, sequence) in [("sender", 1), ("other", 8)] {
        archive
            .correlations
            .insert(key.clone(), AnyObjectRef::Agent(agent(name)));
        archive.retain_correlation_evidence(
            key.clone(),
            vec![capture(sequence, false).observation_ref],
        );
    }
    let observation = capture(2, false);
    let output = CodexMapper.map(MapInput { capture: &observation, canonical_observation: &observation.observation_ref,
        raw: br#"{"type":"response_item","ordinal":1,"payload":{"type":"agent_message","id":"local-conflict","author":"/root/sender","recipient":"/root","content":[{"type":"input_text","text":"do not choose the containing agent"}]}}"#,
        state: &initial.state, archive: &archive });
    let reduced = reduce(&[initial, output]);
    let reference = MessageRef::native(
        agent("sender"),
        "codex.message",
        vec!["local-conflict".into()],
    )
    .unwrap();
    assert!(matches!(
        reduced.objects[&AnyObjectRef::Message(reference)].declaration,
        crate::reducer::Resolved::Conflicted { .. }
    ));
}

#[test]
fn a_conflicting_path_checkpoint_offers_no_unique_reference_or_sufficient_single_proof() {
    let mut archive = ArchiveContext::default();
    let key = correlation_key(&agent("root"), "codex.agent-path", &["/root/shared".into()]);
    for (id, sequence) in [("a", 1), ("b", 2)] {
        archive
            .correlations
            .insert(key.clone(), AnyObjectRef::Agent(agent(id)));
        archive.retain_correlation_evidence(
            key.clone(),
            vec![capture(sequence, false).observation_ref],
        );
    }
    assert!(!archive.correlations.contains_key(&key));
    assert!(archive.correlation_evidence(&key).is_none());
    let targets: std::collections::BTreeSet<_> = archive.correlation_proofs[&key]
        .iter()
        .map(|proof| proof.reference.clone())
        .collect();
    assert_eq!(
        targets,
        [
            AnyObjectRef::Agent(agent("a")),
            AnyObjectRef::Agent(agent("b"))
        ]
        .into()
    );
}
