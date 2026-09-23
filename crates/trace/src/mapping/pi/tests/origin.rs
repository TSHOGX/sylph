use super::*;
use crate::reducer::ResolvedRelation;
use crate::mapping::{inventory::InventoryMapper, owner_resolution};

fn header_capture(name: &str) -> Capture {
    let mut value = capture(0, false);
    value.observation_ref.producer_id = name.into();
    value.source_position.source.instance = name.into();
    value
}
fn retain(archive: &mut ArchiveContext, capture: &Capture, output: &MapOutput) {
    archive.observation_sources.insert(
        capture.observation_ref.clone(),
        capture.source_position.clone(),
    );
    for claim in &output.claims {
        archive
            .claims
            .insert(claim.claim_ref.clone(), claim.clone());
    }
    for ownership in &output.source_owners {
        archive.source_owners.insert(
            (ownership.source.clone(), ownership.generation),
            ownership.owner.clone(),
        );
    }
    for plan in &output.owner_resolution_plans {
        owner_resolution::retain_plan(&mut archive.owner_resolution_plans, plan.clone());
    }
    for (key, target) in &output.correlations {
        if !output.corroborating_correlations.contains(key) {
            archive.correlation_proofs.remove(key);
        }
        archive.correlations.insert(key.clone(), target.clone());
        archive.retain_correlation_evidence(key.clone(), output.correlation_evidence[key].clone());
    }
}
fn map(
    mapper: &dyn SemanticMapper,
    capture: &Capture,
    value: &Value,
    archive: &mut ArchiveContext,
) -> MapOutput {
    let output = mapper.map(MapInput {
        capture,
        canonical_observation: &capture.observation_ref,
        raw: &serde_json::to_vec(value).unwrap(),
        state: &MapperState::default(),
        archive,
    });
    retain(archive, capture, &output);
    output
}
fn inventory(native: &Capture, name: &str, original: &str, locator: &str) -> (Capture, Value) {
    let mut capture = header_capture(name);
    capture.transport_kind = TransportKind::SourceInventory;
    let definition = SourceDefinition {
        source: native.source_position.source.clone(),
        generation: native.source_position.generation,
        locator: locator.into(),
        owner: AgentRef::Session(SessionId::new(
            match &native.source_position.source.issuer {
                Issuer::Environment(environment) => environment.clone(),
                _ => unreachable!(),
            },
            AgentName::parse("pi").unwrap(),
            NativeId::new(original),
        )),
        basis: SourceOwnerBasis::NativeIdentity {
            observation: native.observation_ref.clone(),
        },
    };
    (capture, serde_json::to_value(definition).unwrap())
}
fn lineage(archive: &ArchiveContext) -> Vec<ResolvedRelation> {
    let mut reducer = Reducer::default();
    let claims: Vec<_> = archive.claims.values().cloned().collect();
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.clone())
        .collect();
    reducer.apply(&claims, &evidence, claims.len()).unwrap();
    reducer
        .resolve()
        .relations
        .into_iter()
        .filter(|value| matches!(value.relation, Relation::ContextLineage { .. }))
        .collect()
}

#[test]
fn pi_fork_source_identity_requires_its_retained_locator_and_header_and_is_order_independent() {
    let path = "/renamed/not-the-session-id.jsonl";
    let parent = header_capture("parent-header");
    let child = header_capture("child-header");
    let (certificate, definition) = inventory(&parent, "inventory", "actual-original", path);
    let fork = serde_json::json!({"type":"session","id":"fork","parentSession":path});
    let original = serde_json::json!({"type":"session","id":"actual-original"});
    let mut expected = None;
    for child_first in [true, false] {
        let mut archive = ArchiveContext::default();
        if child_first {
            map(&PiMapper, &child, &fork, &mut archive);
        }
        map(&PiMapper, &parent, &original, &mut archive);
        if child_first {
            assert!(
                matches!(
                    &lineage(&archive)[0].relation,
                    Relation::ContextLineage {
                        original_owner: OwnerRef::Unresolved { .. },
                        ..
                    }
                ),
                "a header without its locator cannot resolve the fork path"
            );
        }
        map(&InventoryMapper, &certificate, &definition, &mut archive);
        if !child_first {
            map(&PiMapper, &child, &fork, &mut archive);
        }
        let resolved = lineage(&archive);
        assert_eq!(resolved.len(), 1);
        let Relation::ContextLineage {
            recipient,
            checkpoint,
            original_owner,
        } = &resolved[0].relation
        else {
            unreachable!()
        };
        assert_eq!(recipient.anchor_session().native_id.as_str(), "fork");
        assert_eq!(checkpoint, &None);
        assert!(
            matches!(original_owner, OwnerRef::Agent { agent } if agent.anchor_session().native_id.as_str() == "actual-original")
        );
        let proof = &archive.claims[&resolved[0].claim];
        assert_eq!(
            proof.evidence.iter().cloned().collect::<BTreeSet<_>>(),
            [
                parent.observation_ref.clone(),
                child.observation_ref.clone(),
                certificate.observation_ref.clone()
            ]
            .into()
        );
        if let Some(expected) = &expected {
            assert_eq!(&archive.claims, expected);
        } else {
            expected = Some(archive.claims);
        }
    }
}

#[test]
fn a_pi_fork_path_cannot_borrow_a_companion_files_containing_owner() {
    let path = "/native/companion.jsonl";
    let parent = header_capture("parent");
    let child = header_capture("fork");
    let mut archive = ArchiveContext::default();
    map(
        &PiMapper,
        &child,
        &serde_json::json!({"type":"session","id":"fork","parentSession":path}),
        &mut archive,
    );
    map(
        &PiMapper,
        &parent,
        &serde_json::json!({"type":"session","id":"original"}),
        &mut archive,
    );
    let (certificate, mut definition) = inventory(&parent, "companion-inventory", "original", path);
    definition["source"]["instance"] = "companion-file".into();
    definition["basis"] = serde_json::to_value(SourceOwnerBasis::CompanionTranscript {
        source: parent.source_position.source.clone(),
        generation: 0,
    })
    .unwrap();
    let mapped = map(&InventoryMapper, &certificate, &definition, &mut archive);
    assert_eq!(
        mapped.source_definitions.len(),
        1,
        "the companion binding itself is valid"
    );
    assert!(
        matches!(
            &lineage(&archive)[0].relation,
            Relation::ContextLineage {
                original_owner: OwnerRef::Unresolved { .. },
                ..
            }
        ),
        "a companion owner is not the fork source's captured native identity"
    );
}

#[test]
fn conflicting_pi_source_identities_remain_alternative_lineage_proofs() {
    let path = "/reused/location.jsonl";
    let child = header_capture("child");
    let mut expected = None;
    for order in [["original-a", "original-b"], ["original-b", "original-a"]] {
        let mut archive = ArchiveContext::default();
        map(
            &PiMapper,
            &child,
            &serde_json::json!({"type":"session","id":"fork","parentSession":path}),
            &mut archive,
        );
        for name in order {
            let header = header_capture(name);
            map(
                &PiMapper,
                &header,
                &serde_json::json!({"type":"session","id":name}),
                &mut archive,
            );
            let (certificate, definition) =
                inventory(&header, &format!("inventory-{name}"), name, path);
            map(&InventoryMapper, &certificate, &definition, &mut archive);
        }
        let origins: BTreeSet<_> = lineage(&archive)
            .into_iter()
            .filter_map(|relation| match relation.relation {
                Relation::ContextLineage {
                    original_owner: OwnerRef::Agent { agent },
                    ..
                } => Some(agent.anchor_session().native_id.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(origins, ["original-a".into(), "original-b".into()].into());
        if let Some(expected) = &expected {
            assert_eq!(&archive.claims, expected);
        } else {
            expected = Some(archive.claims);
        }
    }
}

#[test]
fn a_pi_fork_source_certificate_cannot_cross_vendor_or_environment() {
    let path = "/same/location.jsonl";
    for foreign_vendor in [false, true] {
        let mut archive = ArchiveContext::default();
        let child = header_capture("child");
        map(
            &PiMapper,
            &child,
            &serde_json::json!({"type":"session","id":"fork","parentSession":path}),
            &mut archive,
        );
        let mut original = header_capture("original");
        if !foreign_vendor {
            let issuer = Issuer::Environment(EnvironmentId::parse("otherenv").unwrap());
            original.observation_ref.issuer = issuer.clone();
            original.source_position.source.issuer = issuer;
        }
        if foreign_vendor {
            map(
                &crate::mapping::claude::ClaudeMapper,
                &original,
                &serde_json::json!({"type":"user","sessionId":"original","uuid":"record","message":{"role":"user","content":"Source identity"}}),
                &mut archive,
            );
        } else {
            map(
                &PiMapper,
                &original,
                &serde_json::json!({"type":"session","id":"original"}),
                &mut archive,
            );
        }
        let (mut certificate, mut definition) = inventory(&original, "inventory", "original", path);
        certificate.observation_ref.issuer = original.observation_ref.issuer.clone();
        certificate.source_position.source.issuer = original.observation_ref.issuer.clone();
        if foreign_vendor {
            definition["owner"]["value"]["agent"] = "claude".into();
        }
        let output = map(&InventoryMapper, &certificate, &definition, &mut archive);
        assert_eq!(
            output.source_definitions.len(),
            1,
            "the other source remains a valid identity binding"
        );
        assert!(matches!(
            &lineage(&archive)[0].relation,
            Relation::ContextLineage {
                original_owner: OwnerRef::Unresolved { .. },
                ..
            }
        ));
    }
}
