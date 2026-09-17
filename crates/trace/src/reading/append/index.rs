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

/// Snapshot-bound reverse references needed by the reader's certified update guards.
/// General semantic changes rebuild this index with the full reader projection.
#[derive(Debug, Clone)]
pub struct AppendIndex {
    pub(in crate::reading) reduction: Arc<Reduction>,
    pub(in crate::reading) relations:
        imbl::OrdMap<AnyObjectRef, imbl::OrdMap<ClaimRef, ResolvedRelation>>,
    pub(in crate::reading) coverage: imbl::OrdMap<AgentRef, imbl::OrdMap<ClaimRef, Coverage>>,
    pub(in crate::reading) selected_entries: imbl::OrdSet<EntryRef>,
    pub(in crate::reading) submitted_agents: imbl::OrdSet<AgentRef>,
    pub(in crate::reading) message_entries: imbl::OrdMap<MessageRef, MessageEntries>,
    pub(in crate::reading) selected_messages: imbl::OrdSet<MessageRef>,
    pub(in crate::reading) delivered_messages: imbl::OrdSet<MessageRef>,
    pub(in crate::reading) delivered_entries: imbl::OrdSet<EntryRef>,
    references: usize,
    max_references: usize,
}

#[derive(Debug, Clone, Default)]
pub(in crate::reading) struct MessageEntries {
    pub(in crate::reading) entries: imbl::OrdMap<(AgentRef, usize), EntryRef>,
    pub(in crate::reading) unsupported: bool,
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
            message_entries: imbl::OrdMap::new(),
            selected_messages: imbl::OrdSet::new(),
            delivered_messages: imbl::OrdSet::new(),
            delivered_entries: imbl::OrdSet::new(),
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
                for observation in reduction
                    .history_contents
                    .get(entry)
                    .into_iter()
                    .flat_map(|observations| observations.values())
                {
                    for slot in &observation.slots {
                        work.index_records_examined += 1;
                        if let HistoryContent::Message(revision) = &slot.content {
                            if let Some(TraceObject::MessageRevision(revision)) = reduction
                                .object(&AnyObjectRef::MessageRevision(revision.clone()))
                                .and_then(|object| object.declaration.known())
                            {
                                if !index.selected_messages.contains(&revision.message) {
                                    index.reserve(work)?;
                                    index.selected_messages.insert(revision.message.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        for (agent, history) in &reduction.histories {
            for (ordinal, entry) in history.archive_order.iter().enumerate() {
                index.add_history_entry(&reduction, agent, ordinal, entry, work)?;
            }
        }
        for observations in reduction.delivery_revisions.values() {
            for revision in observations.values() {
                work.index_records_examined += 1;
                if let Some(TraceObject::MessageRevision(revision)) = reduction
                    .object(&AnyObjectRef::MessageRevision(revision.clone()))
                    .and_then(|object| object.declaration.known())
                {
                    if !index.delivered_messages.contains(&revision.message) {
                        index.reserve(work)?;
                        index.delivered_messages.insert(revision.message.clone());
                    }
                }
            }
        }
        for stages in reduction.delivery_stages.values() {
            for (stage, _) in stages {
                work.index_records_examined += 1;
                if let DeliveryStage::Admitted { entry: Some(entry) } = stage {
                    if !index.delivered_entries.contains(entry) {
                        index.reserve(work)?;
                        index.delivered_entries.insert(entry.clone());
                    }
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

    pub(in crate::reading) fn add_relation(
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

    pub(in crate::reading) fn add_history_entry(
        &mut self,
        reduction: &Reduction,
        agent: &AgentRef,
        ordinal: usize,
        entry: &EntryRef,
        work: &mut AppendWork,
    ) -> Option<()> {
        work.index_records_examined += 1;
        let contents = reduction.entry_contents(entry, None);
        let mut unsupported = !contents.complete || contents.slots.is_empty();
        let mut messages = BTreeSet::new();
        for slot in contents.slots.values() {
            work.index_records_examined += 1;
            let Some(alternatives) = slot.known() else {
                unsupported = true;
                continue;
            };
            unsupported |= alternatives.len() != 1;
            for content in alternatives {
                work.index_records_examined += 1;
                let HistoryContent::Message(reference) = content else {
                    unsupported = true;
                    continue;
                };
                let Some(TraceObject::MessageRevision(_)) = reduction
                    .object(&AnyObjectRef::MessageRevision(reference.clone()))
                    .and_then(|object| object.declaration.known())
                else {
                    unsupported = true;
                    continue;
                };
            }
        }
        for observation in reduction
            .history_contents
            .get(entry)
            .into_iter()
            .flat_map(|observations| observations.values())
        {
            for slot in &observation.slots {
                work.index_records_examined += 1;
                let HistoryContent::Message(reference) = &slot.content else {
                    continue;
                };
                if let Some(TraceObject::MessageRevision(revision)) = reduction
                    .object(&AnyObjectRef::MessageRevision(reference.clone()))
                    .and_then(|object| object.declaration.known())
                {
                    messages.insert(revision.message.clone());
                }
            }
        }
        unsupported |= messages.len() != 1;
        for message in messages {
            let mut indexed = self
                .message_entries
                .get(&message)
                .cloned()
                .unwrap_or_default();
            let location = (agent.clone(), ordinal);
            if !indexed.entries.contains_key(&location) {
                self.reserve(work)?;
                indexed.entries.insert(location, entry.clone());
            }
            indexed.unsupported |= unsupported;
            self.message_entries.insert(message, indexed);
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

pub(in crate::reading) struct GuardBudget<'a> {
    remaining: usize,
    work: &'a mut AppendWork,
}

impl<'a> GuardBudget<'a> {
    pub(in crate::reading) fn new(remaining: usize, work: &'a mut AppendWork) -> Self {
        Self { remaining, work }
    }

    pub(in crate::reading) fn examine(&mut self) -> Option<()> {
        if self.remaining == 0 {
            self.work.guard_budget_exhausted = true;
            return None;
        }
        self.remaining -= 1;
        self.work.guard_references_examined += 1;
        Some(())
    }

    pub(in crate::reading) fn examine_many(&mut self, count: usize) -> Option<()> {
        for _ in 0..count {
            self.examine()?;
        }
        Some(())
    }
}
