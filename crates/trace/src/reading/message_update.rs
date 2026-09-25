//! Replace one message at its first archive location using an exact reducer proof.
use super::*;
use crate::reducer::{HistoryView, MessageUpdate};
use append::index::{GuardBudget, MessageEntries};

#[derive(Debug, Clone)]
pub struct MessageProjection {
    pub before: ReadingProjection,
    pub after: ReadingProjection,
    pub index: AppendIndex,
}

/// Reconcile one bounded message region while retaining its existing row address.
/// Selection, delivery and mixed-content dependencies require full reconciliation.
pub fn project_message_update(
    index: &AppendIndex,
    proof: &MessageUpdate,
    before_payloads: &PayloadPreviews,
    after_payloads: &PayloadPreviews,
    max_parts: usize,
    max_guard_work: usize,
    work: &mut AppendWork,
) -> Option<MessageProjection> {
    if !proof.matches(&index.reduction, proof.after())
        || index.selected_messages.contains(proof.message())
        || index.delivered_messages.contains(proof.message())
    {
        return None;
    }
    let mut guard = GuardBudget::new(max_guard_work, work);
    guard.examine()?;
    let indexed = index.message_entries.get(proof.message())?;
    if indexed.unsupported || indexed.entries.is_empty() {
        return None;
    }
    let ((agent, _), _) = indexed.entries.get_min()?;
    if index.inherited_history.get(agent) == Some(&true) {
        return None;
    }
    let scope = ReadingScope {
        agent: agent.clone(),
        selection: None,
    };
    let mut entries = indexed.entries.clone();
    for ((containing_agent, _), entry) in &entries {
        guard.examine()?;
        if containing_agent != agent
            || index.selected_entries.contains(entry)
            || index.delivered_entries.contains(entry)
        {
            return None;
        }
    }
    let mut rendered = BTreeSet::new();
    for reference in proof.updated_objects() {
        guard.examine()?;
        rendered.insert(reference.clone());
    }
    for (containing_agent, addition) in proof.agents() {
        guard.examine()?;
        if containing_agent != agent || index.selected_entries.contains(addition.previous_head()) {
            return None;
        }
        rendered.insert(AnyObjectRef::HistoryEntry(addition.previous_head().clone()));
        for (offset, entry) in addition.added_entries().iter().enumerate() {
            guard.examine()?;
            entries.insert(
                (
                    agent.clone(),
                    addition.previous_archive_len().checked_add(offset)?,
                ),
                entry.clone(),
            );
        }
    }
    let before_partial = region(
        index,
        proof,
        proof.before(),
        indexed,
        &rendered,
        before_payloads,
        &mut guard,
        false,
    )?;
    let after_entries = MessageEntries {
        entries,
        unsupported: false,
    };
    let after_partial = region(
        index,
        proof,
        proof.after(),
        &after_entries,
        &rendered,
        after_payloads,
        &mut guard,
        true,
    )?;
    let before = render_region(
        &before_partial,
        before_payloads,
        max_parts,
        &scope,
        &rendered,
    );
    let after = render_region(&after_partial, after_payloads, max_parts, &scope, &rendered);
    let [before_row] = before.windows.get(&scope)?.as_slice() else {
        return None;
    };
    let [after_row] = after.windows.get(&scope)?.as_slice() else {
        return None;
    };
    if before_row.address != after_row.address || before_row.anchor != after_row.anchor {
        return None;
    }
    let mut next_index = index.clone();
    for (agent, addition) in proof.agents() {
        for (offset, entry) in addition.added_entries().iter().enumerate() {
            next_index.add_history_entry(
                proof.after(),
                agent,
                addition.previous_archive_len().checked_add(offset)?,
                entry,
                work,
            )?;
        }
    }
    for relation in proof.predecessor_relations() {
        next_index.add_relation(relation, work)?;
    }
    next_index.reduction = proof.after().clone();
    Some(MessageProjection {
        before,
        after,
        index: next_index,
    })
}

#[allow(clippy::too_many_arguments)]
fn region(
    index: &AppendIndex,
    proof: &MessageUpdate,
    reduction: &Reduction,
    indexed: &MessageEntries,
    rendered: &BTreeSet<AnyObjectRef>,
    payloads: &PayloadPreviews,
    guard: &mut GuardBudget<'_>,
    include_added_relations: bool,
) -> Option<Reduction> {
    let mut partial = Reduction::default();
    let message_ref = AnyObjectRef::Message(proof.message().clone());
    let message_object = reduction.object(&message_ref)?;
    let TraceObject::Message(message) = message_object.declaration.known()? else {
        return None;
    };
    let history = reduction.messages.get(proof.message())?;
    guard.examine_many(history.revisions.len())?;
    partial
        .messages
        .insert(proof.message().clone(), history.clone());
    let mut agents = BTreeSet::from([proof.message().scope().clone()]);
    if let Author::Agent {
        agent: OwnerRef::Agent { agent },
    } = &message.author
    {
        agents.insert(agent.clone());
    }
    if let Some(production) = &message.production {
        if let OwnerRef::Agent { agent } = &production.owner {
            agents.insert(agent.clone());
        }
        if let Some(activation) = &production.activation {
            let reference = AnyObjectRef::Activation(activation.clone());
            agents.insert(reference.scope().clone());
            if let Some(object) = reduction.object(&reference) {
                partial.objects.insert(reference.clone(), object.clone());
            }
            if let Some(execution) = reduction.executions.get(&reference) {
                guard.examine_many(execution.boundaries.len())?;
                partial.executions.insert(reference, execution.clone());
            }
        }
    }
    partial.objects.insert(message_ref, message_object.clone());
    for revision in &history.revisions {
        let reference = AnyObjectRef::MessageRevision(revision.clone());
        let object = reduction.object(&reference)?;
        let TraceObject::MessageRevision(value) = object.declaration.known()? else {
            return None;
        };
        for part in &value.parts {
            guard.examine()?;
            if let Some(preview) = payloads.get(&part.payload) {
                guard.examine_many(preview.chunks.as_ref().map_or(0, Vec::len))?;
            }
        }
        partial.objects.insert(reference, object.clone());
    }
    for ((agent, _), entry) in &indexed.entries {
        guard.examine()?;
        let reference = AnyObjectRef::HistoryEntry(entry.clone());
        let object = reduction.object(&reference)?;
        let TraceObject::HistoryEntry(value) = object.declaration.known()? else {
            return None;
        };
        if let Some(OwnerRef::Agent { agent }) = &value.original_owner {
            agents.insert(agent.clone());
        }
        let observations = reduction.history_contents.get(entry)?;
        for observation in observations.values() {
            guard.examine()?;
            if !observation.complete {
                return None;
            }
            for slot in &observation.slots {
                guard.examine()?;
                let HistoryContent::Message(revision) = &slot.content else {
                    return None;
                };
                let Some(TraceObject::MessageRevision(revision)) = partial
                    .objects
                    .get(&AnyObjectRef::MessageRevision(revision.clone()))
                    .and_then(|object| object.declaration.known())
                else {
                    return None;
                };
                if &revision.message != proof.message() {
                    return None;
                }
            }
        }
        partial.objects.insert(reference, object.clone());
        partial
            .history_contents
            .insert(entry.clone(), observations.clone());
        let history = partial
            .histories
            .entry(agent.clone())
            .or_insert_with(|| HistoryView {
                agent: agent.clone(),
                selection: Resolved::Unknown,
                candidate_heads: imbl::Vector::new(),
                archive_order: imbl::Vector::new(),
                order_basis: imbl::OrdMap::new(),
                selected_entries: None,
                unresolved_entries: imbl::Vector::new(),
            });
        history.archive_order.push_back(entry.clone());
        history.order_basis.insert(
            entry.clone(),
            reduction
                .histories
                .get(agent)?
                .order_basis
                .get(entry)?
                .clone(),
        );
        agents.insert(agent.clone());
    }
    let mut relations = BTreeMap::new();
    for reference in rendered {
        guard.examine()?;
        if let Some(object) = reduction.object(reference) {
            partial.objects.insert(reference.clone(), object.clone());
            agents.insert(reference.scope().clone());
        }
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
    if include_added_relations {
        for relation in proof.predecessor_relations() {
            guard.examine()?;
            relations.insert(relation.claim.clone(), relation.clone());
        }
    }
    for relation in relations.values() {
        for subject in relation.relation.subject_refs() {
            if let AnyObjectRef::Agent(agent) = subject {
                agents.insert(agent);
            }
        }
    }
    partial.relations = relations.into_values().collect();
    for agent in agents {
        guard.examine()?;
        for (claim, coverage) in index.coverage.get(&agent).into_iter().flatten() {
            guard.examine()?;
            partial
                .coverage
                .push_back((coverage.clone(), claim.clone()));
        }
        let reference = AnyObjectRef::Agent(agent);
        if let Some(object) = reduction.object(&reference) {
            partial.objects.insert(reference, object.clone());
        }
    }
    Some(partial)
}

fn render_region(
    reduction: &Reduction,
    payloads: &PayloadPreviews,
    max_parts: usize,
    scope: &ReadingScope,
    rendered: &BTreeSet<AnyObjectRef>,
) -> ReadingProjection {
    let mut projection = project(reduction, payloads, max_parts);
    projection.headers.clear();
    projection.directory.clear();
    projection.activations.clear();
    projection.histories.clear();
    projection.operations.clear();
    projection.selected_objects.clear();
    projection.windows.retain(|key, _| key == scope);
    projection
        .objects
        .retain(|reference, _| rendered.contains(reference));
    projection
        .object_relations
        .retain(|reference, _| rendered.contains(reference));
    projection
        .result_occurrences
        .retain(|reference, _| rendered.contains(reference));
    projection
}

#[cfg(test)]
mod tests;
