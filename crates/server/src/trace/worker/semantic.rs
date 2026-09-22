//! Mapper-visible semantic facts bound to one immutable reduction.

use std::{collections::BTreeSet, sync::Arc};

use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;
use sylph_trace::{
    mapping::{ArchiveEntryProof, ArchiveHistoryContent, ArchiveSemantic},
    reducer::{IndependentAppend, MessageUpdate, Reduction, Resolved, ResolvedObject},
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticWork {
    pub objects_examined: usize,
    pub content_observations_examined: usize,
    pub support_claims_examined: usize,
    pub aliases_examined: usize,
    pub full_rebuilds: usize,
    pub append_updates: usize,
    pub message_updates: usize,
}

impl SemanticWork {
    pub fn accumulate(&mut self, other: &Self) {
        self.objects_examined += other.objects_examined;
        self.content_observations_examined += other.content_observations_examined;
        self.support_claims_examined += other.support_claims_examined;
        self.aliases_examined += other.aliases_examined;
        self.full_rebuilds += other.full_rebuilds;
        self.append_updates += other.append_updates;
        self.message_updates += other.message_updates;
    }
}

#[derive(Clone)]
pub(super) struct SemanticCache {
    reduction: Arc<Reduction>,
    semantic: Arc<ArchiveSemantic>,
}

impl SemanticCache {
    pub(super) fn advance_message(
        &self,
        proof: &MessageUpdate,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        work: &mut SemanticWork,
    ) -> Option<Self> {
        if !proof.matches(&self.reduction, proof.after()) {
            return None;
        }
        let mut semantic = self.semantic.as_ref().clone();
        for reference in proof.updated_objects() {
            work.objects_examined += 1;
            include_object(
                &mut semantic,
                proof.after().objects.get(reference)?,
                claims,
                work,
            );
        }
        for tail in proof.agents().values() {
            for entry in tail.added_entries() {
                semantic.history_contents.insert(
                    entry.clone(),
                    content_proofs(proof.after().history_contents.get(entry)?, claims, work),
                );
            }
        }
        work.message_updates += 1;
        Some(Self {
            reduction: proof.after().clone(),
            semantic: Arc::new(semantic),
        })
    }

    pub(super) fn get(&self, reduction: &Arc<Reduction>) -> Option<Arc<ArchiveSemantic>> {
        Arc::ptr_eq(&self.reduction, reduction).then(|| self.semantic.clone())
    }

    pub(super) fn rebuild(
        reduction: Arc<Reduction>,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        work: &mut SemanticWork,
    ) -> Self {
        work.full_rebuilds += 1;
        let mut semantic = ArchiveSemantic::default();
        for object in reduction.objects.values() {
            work.objects_examined += 1;
            include_object(&mut semantic, object, claims, work);
        }
        for (entry, observations) in &reduction.history_contents {
            semantic
                .history_contents
                .insert(entry.clone(), content_proofs(observations, claims, work));
        }
        for (alias, canonical) in &reduction.representatives {
            work.aliases_examined += 1;
            if let Some(evidence) = semantic.object_evidence.get(canonical).cloned() {
                semantic.object_evidence.insert(alias.clone(), evidence);
            }
        }
        Self {
            reduction,
            semantic: Arc::new(semantic),
        }
    }

    pub(super) fn advance(
        &self,
        proof: &IndependentAppend,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        work: &mut SemanticWork,
    ) -> Option<Self> {
        if !proof.matches(&self.reduction, proof.after()) {
            return None;
        }
        if Arc::ptr_eq(proof.before(), proof.after()) {
            return Some(self.clone());
        }
        let mut semantic = self.semantic.as_ref().clone();
        for reference in proof.objects() {
            work.objects_examined += 1;
            let object = proof.after().objects.get(reference)?;
            include_object(&mut semantic, object, claims, work);
        }
        for added in proof.agents().values() {
            for entry in added.added_entries() {
                let observations = proof.after().history_contents.get(entry)?;
                semantic
                    .history_contents
                    .insert(entry.clone(), content_proofs(observations, claims, work));
            }
        }
        // The certificate permits only fresh self-represented identities. No
        // previous alias or canonical support changed, so alias scans are unnecessary.
        work.append_updates += 1;
        Some(Self {
            reduction: proof.after().clone(),
            semantic: Arc::new(semantic),
        })
    }
}

fn include_object(
    semantic: &mut ArchiveSemantic,
    object: &ResolvedObject,
    claims: &imbl::OrdMap<ClaimRef, Claim>,
    work: &mut SemanticWork,
) {
    let Resolved::Known {
        value,
        claims: support,
    } = &object.declaration
    else {
        return;
    };
    match value {
        TraceObject::Agent(agent) => {
            semantic
                .agents
                .insert(agent.agent_ref.clone(), agent.clone());
        }
        TraceObject::HistoryEntry(entry) => {
            semantic
                .history_entries
                .insert(entry.entry_ref.clone(), entry.clone());
        }
        TraceObject::Operation(operation) => {
            semantic
                .operations
                .insert(operation.operation_ref.clone(), operation.clone());
        }
        TraceObject::Result(result) => {
            semantic
                .result_operations
                .insert(result.result_ref.clone(), result.operation.clone());
        }
        _ => return,
    }
    let mut evidence = BTreeSet::new();
    let mut entry_proofs = imbl::OrdMap::new();
    for reference in support {
        work.support_claims_examined += 1;
        if let Some(claim) = claims.get(reference) {
            evidence.extend(claim.evidence.iter().cloned());
            if matches!(value, TraceObject::HistoryEntry(_)) {
                if let ObjectAddress::Occurrence { position, .. } = claim.claim_ref.address() {
                    entry_proofs.insert(
                        reference.clone(),
                        ArchiveEntryProof {
                            position: position.clone(),
                            evidence: claim.evidence.clone(),
                        },
                    );
                }
            }
        }
    }
    if let TraceObject::HistoryEntry(entry) = value {
        semantic
            .history_entry_proofs
            .insert(entry.entry_ref.clone(), entry_proofs);
    }
    semantic
        .object_evidence
        .insert(value.object_ref(), evidence.into_iter().collect());
}

fn content_proofs(
    observations: &std::collections::BTreeMap<ClaimRef, HistoryContentObservation>,
    claims: &imbl::OrdMap<ClaimRef, Claim>,
    work: &mut SemanticWork,
) -> imbl::Vector<ArchiveHistoryContent> {
    observations
        .iter()
        .filter_map(|(reference, observation)| {
            work.content_observations_examined += 1;
            work.support_claims_examined += 1;
            let proof = claims.get(reference)?;
            Some(ArchiveHistoryContent {
                claim_ref: reference.clone(),
                observation: observation.clone(),
                evidence: proof.evidence.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylph_trace::reducer::Reducer;

    fn agent() -> AgentRef {
        AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:semantic-cache").unwrap())
    }

    fn entry(id: &str) -> EntryRef {
        EntryRef::native(agent(), "entry", vec![id.into()]).unwrap()
    }

    fn operation(id: &str) -> OperationRef {
        OperationRef::native(agent(), "operation", vec![id.into()]).unwrap()
    }

    fn claim(id: &str, fact: Fact) -> Claim {
        Claim {
            claim_ref: ClaimRef::native(agent(), "claim", vec![id.into()]).unwrap(),
            fact,
            evidence: vec![ObservationRef {
                issuer: Issuer::Server("fixture".into()),
                producer_id: id.into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            }],
            rule_id: "semantic-cache/fixture".into(),
            mapper_build: "test".into(),
            supersedes: vec![],
            source_revision: None,
        }
    }

    fn operation_object(id: &str) -> TraceObject {
        TraceObject::Operation(Operation {
            operation_ref: operation(id),
            owner: OwnerRef::Agent { agent: agent() },
            activation: None,
            requester: None,
            category: OperationCategory::Tool {
                namespace: None,
                name: "read".into(),
            },
            input_revision: None,
            argument_state: ArgumentState::Sealed,
        })
    }

    #[test]
    fn rebuilding_semantics_preserves_declared_families_alias_support_and_content_provenance() {
        let result = ResultRef::native(agent(), "result", vec!["returned".into()]).unwrap();
        let revision =
            MessageRevisionRef::native(agent(), "revision", vec!["body".into()]).unwrap();
        let position = SourcePosition {
            source: SourceRef {
                issuer: Issuer::Server("fixture".into()),
                instance: "native".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: 1 },
        };
        let agent_claim = claim(
            "agent",
            Fact::Declare {
                object: TraceObject::Agent(AgentThread {
                    agent_ref: agent(),
                    vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                    name: None,
                    model: None,
                }),
            },
        );
        let entry_claim = claim(
            "entry",
            Fact::Declare {
                object: TraceObject::HistoryEntry(HistoryEntry {
                    entry_ref: entry("one"),
                    containing_agent: agent(),
                    predecessor: None,
                    original_owner: None,
                }),
            },
        );
        let mut repeated = claim("entry-repeat", entry_claim.fact.clone());
        repeated.evidence.extend(entry_claim.evidence.clone());
        let left = claim(
            "operation-a",
            Fact::Declare {
                object: operation_object("a"),
            },
        );
        let right = claim(
            "operation-b",
            Fact::Declare {
                object: operation_object("b"),
            },
        );
        let returned = claim(
            "result",
            Fact::Declare {
                object: TraceObject::Result(ResultOccurrence {
                    result_ref: result.clone(),
                    operation: operation("b"),
                    recipient: None,
                    disposition: ResultDisposition::FinalReturn,
                    representation: Representation::CallerResult,
                    payload: None,
                    delivery: None,
                }),
            },
        );
        let content = claim(
            "content",
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry("one"),
                    position,
                    "fixture/native",
                    vec![HistoryContent::Message(revision)],
                ),
            },
        );
        let equivalent = claim(
            "equivalent",
            Fact::Equivalence {
                left: AnyObjectRef::Operation(operation("a")),
                right: AnyObjectRef::Operation(operation("b")),
                rule: "native-proof".into(),
            },
        );
        let values = vec![
            agent_claim.clone(),
            entry_claim.clone(),
            repeated.clone(),
            left.clone(),
            right.clone(),
            returned.clone(),
            content.clone(),
            equivalent,
        ];
        let mut reducer = Reducer::default();
        reducer
            .apply(
                &values,
                &values
                    .iter()
                    .flat_map(|claim| claim.evidence.clone())
                    .collect(),
                values.len(),
            )
            .unwrap();
        let reduction = Arc::new(reducer.resolve());
        let claims = reducer.claim_snapshot();
        let mut work = SemanticWork::default();
        let cache = SemanticCache::rebuild(reduction.clone(), &claims, &mut work);
        let semantic = cache.get(&reduction).unwrap();
        assert_eq!(semantic.agents.len(), 1);
        assert_eq!(semantic.history_entries.len(), 1);
        assert_eq!(semantic.operations.len(), 1);
        assert_eq!(semantic.result_operations[&result], operation("a"));
        assert_eq!(
            semantic.object_evidence[&AnyObjectRef::Agent(agent())]
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            agent_claim.evidence
        );
        let expected_entry = entry_claim
            .evidence
            .iter()
            .chain(&repeated.evidence)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            semantic.object_evidence[&AnyObjectRef::HistoryEntry(entry("one"))]
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected_entry
        );
        let expected_operation = left
            .evidence
            .iter()
            .chain(&right.evidence)
            .cloned()
            .collect::<BTreeSet<_>>();
        for name in ["a", "b"] {
            assert_eq!(
                semantic.object_evidence[&AnyObjectRef::Operation(operation(name))]
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected_operation
            );
        }
        assert_eq!(semantic.history_contents[&entry("one")].len(), 1);
        assert_eq!(
            semantic.history_contents[&entry("one")][0].claim_ref,
            content.claim_ref
        );
        assert_eq!(
            semantic.history_contents[&entry("one")][0].evidence,
            content.evidence
        );
        assert_eq!(work.full_rebuilds, 1);
        assert_eq!(work.objects_examined, reduction.objects.len());
        assert_eq!(work.aliases_examined, reduction.representatives.len());
        assert_eq!(work.content_observations_examined, 1);
        assert_eq!(work.support_claims_examined, 7);
        assert!(cache.get(&Arc::new(reduction.as_ref().clone())).is_none());
        let held = semantic.clone();
        let mut candidate = semantic.as_ref().clone();
        candidate.history_entries.remove(&entry("one"));
        assert_eq!(held.history_entries.len(), 1);
        assert!(candidate.operations.ptr_eq(&held.operations));
    }
}
