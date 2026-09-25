//! Reader-only guards for a reducer-proved independent archive tail.
use super::*;
use crate::reducer::{HistoryView, IndependentAppend};

pub(super) mod index;
use index::GuardBudget;
pub use index::{AppendIndex, AppendWork};

#[derive(Debug, Clone)]
pub struct AppendProjection {
    pub projection: ReadingProjection,
    pub index: AppendIndex,
}

/// Render a certified archive tail using bounded reader dependency lookups.
/// Returning `None` requests full reconciliation; the supplied index is unchanged.
pub fn project_independent_append(
    index: &AppendIndex,
    proof: &IndependentAppend,
    payloads: &PayloadPreviews,
    max_parts: usize,
    max_guard_work: usize,
    work: &mut AppendWork,
) -> Option<AppendProjection> {
    if !proof.matches(&index.reduction, proof.after()) {
        return None;
    }
    let after = proof.after();
    let mut guard = GuardBudget::new(max_guard_work, work);
    let mut partial = Reduction::default();
    let mut agents = BTreeSet::new();
    let mut rendered_objects = BTreeSet::new();
    for reference in proof.objects() {
        guard.examine()?;
        let object = after.object(reference)?;
        agents.insert(reference.scope().clone());
        match object.declaration.known()? {
            TraceObject::Message(message) => {
                if let Author::Agent {
                    agent: OwnerRef::Agent { agent },
                } = &message.author
                {
                    agents.insert(agent.clone());
                }
                partial.messages.insert(
                    message.message_ref.clone(),
                    after.messages.get(&message.message_ref)?.clone(),
                );
            }
            TraceObject::MessageRevision(_) => {}
            TraceObject::HistoryEntry(entry) => {
                if let Some(OwnerRef::Agent { agent }) = &entry.original_owner {
                    agents.insert(agent.clone());
                }
                partial.history_contents.insert(
                    entry.entry_ref.clone(),
                    after.history_contents.get(&entry.entry_ref)?.clone(),
                );
            }
            _ => return None,
        }
        partial.objects.insert(reference.clone(), object.clone());
        rendered_objects.insert(reference.clone());
    }
    for (agent, append) in proof.agents() {
        guard.examine()?;
        // The first non-inherited tail can change the header's context state.
        // Reconcile that transition once; ordinary local tails keep their bounds.
        if index.inherited_history.get(agent) == Some(&true)
            || index.submitted_agents.contains(agent)
            || index.selected_entries.contains(append.previous_head())
        {
            return None;
        }
        let history = after.histories.get(agent)?;
        let mut tail = imbl::Vector::new();
        let mut order_basis = imbl::OrdMap::new();
        for entry in append.added_entries() {
            guard.examine()?;
            tail.push_back(entry.clone());
            order_basis.insert(entry.clone(), history.order_basis.get(entry)?.clone());
        }
        partial.histories.insert(
            agent.clone(),
            HistoryView {
                agent: agent.clone(),
                selection: Resolved::Unknown,
                candidate_heads: imbl::Vector::new(),
                archive_order: tail,
                order_basis,
                selected_entries: None,
                unresolved_entries: imbl::Vector::new(),
            },
        );
        let boundary = AnyObjectRef::HistoryEntry(append.previous_head().clone());
        partial
            .objects
            .insert(boundary.clone(), after.object(&boundary)?.clone());
        agents.insert(boundary.scope().clone());
        rendered_objects.insert(boundary);
    }
    let mut relations = BTreeMap::new();
    for reference in &rendered_objects {
        guard.examine()?;
        for (claim, relation) in index.relations.get(reference).into_iter().flatten() {
            guard.examine()?;
            if matches!(relation.relation, Relation::ContextCopy { .. }) {
                // The partial region omits remote version proofs. Reconcile the
                // semantic graph instead of silently dropping optional lineage.
                return None;
            }
            relations.insert(claim.clone(), relation.clone());
        }
    }
    for relation in proof.predecessor_relations() {
        guard.examine()?;
        relations.insert(relation.claim.clone(), relation.clone());
    }
    partial.relations = relations.into_values().collect();
    for agent in agents {
        guard.examine()?;
        // Every coverage record in an included agent's scope is retained, including
        // object-specific coverage. The ordinary projector builds both indexes.
        for (claim, coverage) in index.coverage.get(&agent).into_iter().flatten() {
            guard.examine()?;
            partial
                .coverage
                .push_back((coverage.clone(), claim.clone()));
        }
        let reference = AnyObjectRef::Agent(agent);
        if let Some(object) = after.object(&reference) {
            partial.objects.insert(reference, object.clone());
        }
    }
    for object in partial.objects.values() {
        for _ in object.declaration.known()?.payload_refs() {
            guard.examine()?;
        }
    }
    let mut next_index = index.clone();
    for (agent, append) in proof.agents() {
        for (offset, entry) in append.added_entries().iter().enumerate() {
            next_index.add_history_entry(
                after,
                agent,
                append.previous_archive_len().checked_add(offset)?,
                entry,
                work,
            )?;
        }
    }
    for relation in proof.predecessor_relations() {
        next_index.add_relation(relation, work)?;
    }
    next_index.reduction = after.clone();
    let mut projected = project(&partial, payloads, max_parts);
    // Tail rows have no activation and cannot change agent execution. Headers and
    // directories retain their committed values, including the complete history.
    projected.headers.clear();
    projected.directory.clear();
    projected.activations.clear();
    projected
        .windows
        .retain(|scope, _| partial.histories.contains_key(&scope.agent));
    projected
        .objects
        .retain(|reference, _| rendered_objects.contains(reference));
    projected
        .object_relations
        .retain(|reference, _| rendered_objects.contains(reference));
    projected
        .result_occurrences
        .retain(|reference, _| rendered_objects.contains(reference));
    Some(AppendProjection {
        projection: projected,
        index: next_index,
    })
}

#[cfg(test)]
mod tests;
