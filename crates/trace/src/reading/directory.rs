//! One preorder interval per agent, rather than an ancestor/descendant closure.
use super::*;

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub row: AgentDirectoryRow,
    pub component: AgentRef,
    pub position: usize,
    pub end_exclusive: usize,
    pub spawn_claims: Vec<ClaimRef>,
    pub parent: Option<AgentRef>,
}

pub(super) fn project(index: &Index<'_>) -> BTreeMap<AgentRef, DirectoryEntry> {
    let mut edges = BTreeMap::<AgentRef, Vec<ClaimRef>>::new();
    for relation in &index.reduction.relations {
        if let Relation::Spawn { parent, child, .. } = &relation.relation {
            if index.agents.contains_key(parent) && index.agents.contains_key(child) {
                edges
                    .entry(child.clone())
                    .or_default()
                    .push(relation.claim.clone());
            }
        }
    }
    let mut children = BTreeMap::<AgentRef, Vec<AgentRef>>::new();
    for (child, claims) in &mut edges {
        claims.sort();
        claims.dedup();
        children
            .entry(index.parents[child].clone())
            .or_default()
            .push(child.clone());
    }
    for siblings in children.values_mut() {
        // One source's occurrence order is preserved. Independent certificates
        // use their stable reference tie-breaker; activity never reorders a list.
        let position = |agent: &AgentRef| {
            edges[agent]
                .iter()
                .filter_map(|claim| match claim.address() {
                    ObjectAddress::Occurrence { position, .. } => Some(position.clone()),
                    ObjectAddress::Native { .. } => None,
                })
                .min()
        };
        siblings.sort_by_cached_key(|agent| (position(agent), agent.clone()));
    }
    let mut pending: Vec<_> = index
        .agents
        .keys()
        .filter(|agent| !edges.contains_key(*agent))
        .rev()
        .map(|agent| (agent.clone(), agent.clone(), false))
        .collect();
    let mut result = BTreeMap::new();
    let mut ordinal = 0;
    while let Some((component, agent, leaving)) = pending.pop() {
        if leaving {
            let entry: &mut DirectoryEntry =
                result.get_mut(&agent).expect("entered directory node");
            entry.end_exclusive = ordinal;
            continue;
        }
        if agent == component {
            ordinal = 0;
        }
        let entry = DirectoryEntry {
            component: component.clone(),
            row: AgentDirectoryRow {
                agent: index.agent_link(&agent),
                relation: "spawn".into(),
                own_execution: index.agent_execution(&agent),
                activations: count(
                    index.activation_agents.get(&agent).map_or(0, Vec::len),
                    "own activations",
                ),
                required_input: unknown_count("required input"),
                coverage: index.coverage(&agent),
            },
            position: ordinal,
            end_exclusive: ordinal + 1,
            spawn_claims: edges.get(&agent).cloned().unwrap_or_default(),
            parent: edges
                .contains_key(&agent)
                .then(|| index.parents[&agent].clone()),
        };
        result.insert(agent.clone(), entry);
        ordinal += 1;
        pending.push((component.clone(), agent.clone(), true));
        if let Some(children) = children.get(&agent) {
            pending.extend(
                children
                    .iter()
                    .rev()
                    .map(|child| (component.clone(), child.clone(), false)),
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::Reducer;
    #[test]
    fn a_deep_spawn_forest_retains_one_interval_and_one_incoming_proof_per_agent() {
        for count_agents in [100, 1000] {
            let agents: Vec<_> = (0..count_agents)
                .map(|index| {
                    AgentRef::Session(
                        sylph_protocol::SessionId::decode(&format!("env:codex:node-{index:04}"))
                            .unwrap(),
                    )
                })
                .collect();
            let observation = ObservationRef {
                issuer: Issuer::Server("fixture".into()),
                producer_id: "forest".into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            };
            let mut claims = vec![];
            for (index, agent) in agents.iter().enumerate() {
                let fact = Fact::Declare {
                    object: TraceObject::Agent(AgentThread {
                        agent_ref: agent.clone(),
                        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                        name: None,
                        model: None,
                    }),
                };
                claims.push(Claim {
                    claim_ref: ClaimRef::native(agent.clone(), "identity", vec!["one".into()])
                        .unwrap(),
                    fact,
                    evidence: vec![observation.clone()],
                    rule_id: "forest-test".into(),
                    mapper_build: "fixture".into(),
                    supersedes: vec![],
                    source_revision: None,
                });
                if index > 0 {
                    claims.push(Claim {
                        claim_ref: ClaimRef::native(
                            agents[index - 1].clone(),
                            "spawn",
                            vec![index.to_string()],
                        )
                        .unwrap(),
                        fact: Fact::Relation {
                            relation: Relation::Spawn {
                                parent: agents[index - 1].clone(),
                                child: agent.clone(),
                                operation: None,
                            },
                        },
                        evidence: vec![observation.clone()],
                        rule_id: "forest-test".into(),
                        mapper_build: "fixture".into(),
                        supersedes: vec![],
                        source_revision: None,
                    });
                }
            }
            let mut reducer = Reducer::default();
            reducer
                .apply(&claims, &[observation].into_iter().collect(), claims.len())
                .unwrap();
            let projection = super::super::project(&reducer.resolve(), &PayloadPreviews::new(), 8);
            assert_eq!(projection.directory.len(), count_agents);
            assert_eq!(
                projection
                    .directory
                    .values()
                    .map(|entry| entry.spawn_claims.len())
                    .sum::<usize>(),
                count_agents - 1
            );
            for (index, agent) in agents.iter().enumerate() {
                let entry = &projection.directory[agent];
                assert_eq!(entry.position, index);
                assert_eq!(entry.end_exclusive, count_agents);
                assert_eq!(
                    projection.headers[agent].descendant_count.value,
                    Some((count_agents - index - 1) as u64)
                );
            }
        }
    }
}

#[cfg(test)]
mod component_tests {
    use super::*;
    use crate::reducer::Reducer;
    #[test]
    fn an_unrelated_earlier_root_does_not_renumber_an_existing_spawn_tree() {
        let agent = |name: &str| {
            AgentRef::Session(
                sylph_protocol::SessionId::decode(&format!("env:codex:{name}")).unwrap(),
            )
        };
        let root = agent("middle-root");
        let child = agent("middle-child");
        let observation = ObservationRef {
            issuer: Issuer::Server("fixture".into()),
            producer_id: "component".into(),
            producer_epoch: "one".into(),
            producer_seq: 1,
        };
        let declare = |owner: &AgentRef, name: &str, fact: Fact| Claim {
            claim_ref: ClaimRef::native(owner.clone(), "component", vec![name.into()]).unwrap(),
            fact,
            evidence: vec![observation.clone()],
            rule_id: "component-test".into(),
            mapper_build: "fixture".into(),
            supersedes: vec![],
            source_revision: None,
        };
        let object = |owner: &AgentRef| Fact::Declare {
            object: TraceObject::Agent(AgentThread {
                agent_ref: owner.clone(),
                vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                name: None,
                model: None,
            }),
        };
        let claims = vec![
            declare(&root, "root", object(&root)),
            declare(&child, "child", object(&child)),
            declare(
                &root,
                "spawn",
                Fact::Relation {
                    relation: Relation::Spawn {
                        parent: root.clone(),
                        child: child.clone(),
                        operation: None,
                    },
                },
            ),
        ];
        let mut reducer = Reducer::default();
        let evidence = [observation.clone()].into_iter().collect();
        reducer.apply(&claims, &evidence, claims.len()).unwrap();
        let before = super::super::project(&reducer.resolve(), &PayloadPreviews::new(), 8);
        let unrelated = agent("earlier-root");
        reducer
            .apply(
                &[declare(&unrelated, "unrelated", object(&unrelated))],
                &evidence,
                1,
            )
            .unwrap();
        let after = super::super::project(&reducer.resolve(), &PayloadPreviews::new(), 8);
        for agent in [&root, &child] {
            let old = &before.directory[agent];
            let new = &after.directory[agent];
            assert_eq!(
                (&old.component, old.position, old.end_exclusive),
                (&new.component, new.position, new.end_exclusive)
            );
        }
        assert_eq!(after.directory[&unrelated].position, 0);
        assert_eq!(after.directory[&unrelated].end_exclusive, 1);
    }
}
