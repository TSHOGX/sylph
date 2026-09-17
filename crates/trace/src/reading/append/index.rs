use super::*;
use crate::reducer::ResolvedRelation;
use std::sync::Arc;

/// Reader optimization work is independent of reducer and materialized-row work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendWork {
    pub index_records_examined: usize,
    pub references_indexed: usize,
    pub guard_references_examined: usize,
    pub index_unavailable: bool,
    pub guard_budget_exhausted: bool,
}

impl AppendWork {
    pub fn accumulate(&mut self, other: &Self) {
        self.index_records_examined += other.index_records_examined;
        self.references_indexed += other.references_indexed;
        self.guard_references_examined += other.guard_references_examined;
        self.index_unavailable |= other.index_unavailable;
        self.guard_budget_exhausted |= other.guard_budget_exhausted;
    }
}

/// Snapshot-bound reverse references needed only by the reader's append guard.
/// General semantic changes rebuild this index with the full reader projection.
#[derive(Debug, Clone)]
pub struct AppendIndex {
    pub(super) reduction: Arc<Reduction>,
    pub(super) relations: imbl::OrdMap<AnyObjectRef, imbl::OrdMap<ClaimRef, ResolvedRelation>>,
    pub(super) coverage: imbl::OrdMap<AgentRef, imbl::OrdMap<ClaimRef, Coverage>>,
    pub(super) selected_entries: imbl::OrdSet<EntryRef>,
    pub(super) submitted_agents: imbl::OrdSet<AgentRef>,
    references: usize,
    max_references: usize,
}

impl AppendIndex {
    /// Build bounded optimization metadata during a full projection or restore.
    /// Exhaustion discards this index; it never discards retained trace evidence.
    pub fn build(
        reduction: Arc<Reduction>,
        max_references: usize,
        work: &mut AppendWork,
    ) -> Option<Self> {
        let mut index = Self {
            reduction: reduction.clone(),
            relations: imbl::OrdMap::new(),
            coverage: imbl::OrdMap::new(),
            selected_entries: imbl::OrdSet::new(),
            submitted_agents: imbl::OrdSet::new(),
            references: 0,
            max_references,
        };
        for object in reduction.objects.values() {
            work.index_records_examined += 1;
            if let Some(TraceObject::Delivery(Delivery {
                route: DeliveryRoute::ServerCommand { .. },
                recipient:
                    Recipient::Agent {
                        agent: OwnerRef::Agent { agent },
                    },
                ..
            })) = object.declaration.known()
            {
                if !index.submitted_agents.contains(agent) {
                    index.reserve(work)?;
                    index.submitted_agents.insert(agent.clone());
                }
            }
        }
        for membership in reduction.selection_memberships.values() {
            work.index_records_examined += 1;
            for entry in membership.iter().flatten() {
                work.index_records_examined += 1;
                if !index.selected_entries.contains(entry) {
                    index.reserve(work)?;
                    index.selected_entries.insert(entry.clone());
                }
            }
        }
        for relation in &reduction.relations {
            index.add_relation(relation, work)?;
        }
        for (coverage, claim) in &reduction.coverage {
            work.index_records_examined += 1;
            index.reserve(work)?;
            let agent = coverage.subject.scope();
            let mut bucket = index.coverage.get(agent).cloned().unwrap_or_default();
            bucket.insert(claim.clone(), coverage.clone());
            index.coverage.insert(agent.clone(), bucket);
        }
        Some(index)
    }

    pub(super) fn add_relation(
        &mut self,
        relation: &ResolvedRelation,
        work: &mut AppendWork,
    ) -> Option<()> {
        work.index_records_examined += 1;
        for subject in relation.relation.subject_refs() {
            work.index_records_examined += 1;
            let mut bucket = self.relations.get(&subject).cloned().unwrap_or_default();
            if !bucket.contains_key(&relation.claim) {
                self.reserve(work)?;
                bucket.insert(relation.claim.clone(), relation.clone());
                self.relations.insert(subject, bucket);
            }
        }
        Some(())
    }

    fn reserve(&mut self, work: &mut AppendWork) -> Option<()> {
        if self.references >= self.max_references {
            work.index_unavailable = true;
            return None;
        }
        self.references += 1;
        work.references_indexed += 1;
        Some(())
    }
}

pub(super) struct GuardBudget<'a> {
    remaining: usize,
    work: &'a mut AppendWork,
}

impl<'a> GuardBudget<'a> {
    pub(super) fn new(remaining: usize, work: &'a mut AppendWork) -> Self {
        Self { remaining, work }
    }

    pub(super) fn examine(&mut self) -> Option<()> {
        if self.remaining == 0 {
            self.work.guard_budget_exhausted = true;
            return None;
        }
        self.remaining -= 1;
        self.work.guard_references_examined += 1;
        Some(())
    }
}
