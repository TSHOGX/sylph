//! An archive tail whose new messages cannot change earlier history rows.
//!
//! General corrections still require reconciliation. This proof is deliberately
//! about normalized dependencies, not vendor names or serialized body equality.
use super::*;
use crate::reducer::{HistoryView, ReductionChanges};

/// Render only independently appended message entries, or request reconciliation.
/// The supplied reductions remain the authority for identity and history order.
pub fn project_independent_append(
    before: &Reduction,
    after: &Reduction,
    changes: &ReductionChanges,
    payloads: &BTreeMap<PayloadRef, PayloadPreview>,
    max_parts: usize,
) -> Option<ReadingProjection> {
    if changes.objects.changed.is_empty()
        || !changes.objects.removed.is_empty()
        || !changes.representatives.removed.is_empty()
        || changes
            .representatives
            .changed
            .iter()
            .any(|(reference, change)| change.before.is_some() || reference != &change.after)
        || !changes.executions.is_empty()
        || !changes.delivery_stages.is_empty()
        || !changes.delivery_revisions.is_empty()
        || !changes.resource_states.is_empty()
        || !changes.agent_states.is_empty()
        || !changes.relations.removed.is_empty()
        || !changes.selection_memberships.is_empty()
        || !changes.selection_content_observations.is_empty()
        || !changes.coverage.is_empty()
        || changes.diagnostics.is_some()
        || !changes.withdrawn_claims.is_empty()
        || !changes.messages.removed.is_empty()
        || !changes.histories.removed.is_empty()
        || !changes.history_contents.removed.is_empty()
        || after
            .representatives
            .iter()
            .any(|(reference, canonical)| reference != canonical)
    {
        return None;
    }
    let mut partial = Reduction::default();
    let mut agents = BTreeSet::new();
    let mut entries = BTreeSet::new();
    let mut messages = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    for (reference, change) in &changes.objects.changed {
        if change.before.is_some() {
            return None;
        }
        agents.insert(reference.scope().clone());
        match change.after.declaration.known()? {
            TraceObject::Message(message) => {
                if let Author::Agent {
                    agent: OwnerRef::Agent { agent },
                } = &message.author
                {
                    agents.insert(agent.clone());
                }
                if message
                    .production
                    .as_ref()
                    .is_some_and(|scope| scope.activation.is_some())
                {
                    return None;
                }
                messages.insert(message.message_ref.clone());
            }
            TraceObject::MessageRevision(revision) => {
                if !matches!(revision.form, RevisionForm::Complete) {
                    return None;
                }
                revisions.insert(revision.revision_ref.clone());
            }
            TraceObject::HistoryEntry(entry) => {
                if entry.entry_ref.scope() != &entry.containing_agent {
                    return None;
                }
                if let Some(OwnerRef::Agent { agent }) = &entry.original_owner {
                    agents.insert(agent.clone());
                }
                entries.insert(entry.entry_ref.clone());
            }
            _ => return None,
        }
        partial
            .objects
            .insert(reference.clone(), change.after.clone());
    }
    if messages.is_empty() || revisions.is_empty() || entries.is_empty() {
        return None;
    }
    if before.relations.iter().any(|value| {
        value
            .relation
            .subject_refs()
            .iter()
            .any(|reference| changes.objects.changed.contains_key(reference))
    }) {
        return None;
    }
    for revision in &revisions {
        let TraceObject::MessageRevision(value) = after
            .object(&AnyObjectRef::MessageRevision(revision.clone()))?
            .declaration
            .known()?
        else {
            return None;
        };
        if !messages.contains(&value.message) {
            return None;
        }
    }
    for (message, change) in &changes.messages.changed {
        if change.before.is_some()
            || !messages.contains(message)
            || change.after.revisions.len() != 1
            || change
                .after
                .revisions
                .iter()
                .any(|revision| !revisions.contains(revision))
        {
            return None;
        }
        partial
            .messages
            .insert(message.clone(), change.after.clone());
    }
    if partial.messages.len() != messages.len() {
        return None;
    }
    let mut boundaries = BTreeSet::new();
    for change in changes.relations.changed.values() {
        let Relation::HistoryPredecessor { predecessor, entry } = &change.after else {
            return None;
        };
        if change.before.is_some() || !entries.contains(entry) {
            return None;
        }
        if !entries.contains(predecessor) {
            boundaries.insert(predecessor.clone());
        }
    }
    // A late declaration can satisfy an older missing reference. Such an update
    // must revisit those rows even though the object itself is newly declared.
    if before.history_contents.values().flat_map(BTreeMap::values)
        .flat_map(|observation| &observation.slots)
        .any(|slot| matches!(&slot.content, HistoryContent::Message(revision) if revisions.contains(revision)))
        || before.delivery_revisions.values().flat_map(BTreeMap::values)
            .any(|revision| revisions.contains(revision))
        || before.delivery_stages.values().flatten()
            .any(|(stage, _)| matches!(stage, DeliveryStage::Admitted { entry: Some(entry) } if entries.contains(entry)))
        || before.selection_memberships.values().flatten().flatten().any(|entry| entries.contains(entry))
    { return None; }
    for (entry, change) in &changes.history_contents.changed {
        if change.before.is_some() || !entries.contains(entry) {
            return None;
        }
        if change.after.values().any(|observation| !observation.complete
            || observation.slots.iter().any(|slot| !matches!(&slot.content, HistoryContent::Message(revision) if revisions.contains(revision))))
        { return None; }
        partial
            .history_contents
            .insert(entry.clone(), change.after.clone());
    }
    if partial.history_contents.len() != entries.len() {
        return None;
    }
    for entry in &entries {
        let contents = after.entry_contents(entry, None);
        if !contents.complete || contents.slots.values().any(|slot|
            !matches!(slot.known().map(Vec::as_slice), Some([HistoryContent::Message(reference)]) if revisions.contains(reference))) {
            return None;
        }
    }
    let mut appended = BTreeSet::new();
    for (agent, change) in &changes.histories.changed {
        let old = change.before.as_ref()?;
        let new = &change.after;
        if old.selection != new.selection
            || old.selected_entries != new.selected_entries
            || old.unresolved_entries != new.unresolved_entries
            || old.candidate_heads.len() != new.candidate_heads.len()
            || !new.archive_order.starts_with(&old.archive_order)
            || old
                .order_basis
                .iter()
                .any(|(entry, basis)| new.order_basis.get(entry) != Some(basis))
        {
            return None;
        }
        let tail = &new.archive_order[old.archive_order.len()..];
        if tail.is_empty() || tail.iter().any(|entry| !entries.contains(entry)) {
            return None;
        }
        let mut predecessor = old.archive_order.last();
        for entry in tail {
            let TraceObject::HistoryEntry(value) = after
                .object(&AnyObjectRef::HistoryEntry(entry.clone()))?
                .declaration
                .known()?
            else {
                return None;
            };
            if value.predecessor.as_ref() != predecessor {
                return None;
            }
            predecessor = Some(entry);
        }
        appended.extend(tail.iter().cloned());
        partial.histories.insert(
            agent.clone(),
            HistoryView {
                agent: agent.clone(),
                selection: Resolved::Unknown,
                candidate_heads: vec![],
                archive_order: tail.to_vec(),
                order_basis: tail
                    .iter()
                    .map(|entry| Some((entry.clone(), new.order_basis.get(entry)?.clone())))
                    .collect::<Option<_>>()?,
                selected_entries: None,
                unresolved_entries: vec![],
            },
        );
    }
    if appended != entries {
        return None;
    }
    if before
        .selection_memberships
        .values()
        .flatten()
        .flatten()
        .any(|entry| boundaries.contains(entry))
    {
        return None;
    }
    let mut rendered_objects: BTreeSet<_> = changes.objects.changed.keys().cloned().collect();
    for boundary in boundaries {
        let reference = AnyObjectRef::HistoryEntry(boundary);
        let object = after.object(&reference)?;
        agents.insert(reference.scope().clone());
        partial.objects.insert(reference.clone(), object.clone());
        rendered_objects.insert(reference);
    }
    partial.relations = after
        .relations
        .iter()
        .filter(|value| {
            value
                .relation
                .subject_refs()
                .iter()
                .any(|reference| rendered_objects.contains(reference))
        })
        .cloned()
        .collect();
    if before.objects.values().filter_map(|object| object.declaration.known()).any(|object|
        matches!(object, TraceObject::Delivery(delivery)
            if matches!(delivery.route, DeliveryRoute::ServerCommand { .. })
                && matches!(&delivery.recipient, Recipient::Agent { agent: OwnerRef::Agent { agent } }
                    if changes.histories.changed.contains_key(agent)))) {
        return None;
    }
    for agent in agents {
        let reference = AnyObjectRef::Agent(agent);
        if let Some(object) = after.object(&reference) {
            partial.objects.insert(reference, object.clone());
        }
    }
    // Preserve agent-scope coverage in new object details. It is unchanged by the
    // append proof and must not be synthesized from the partial history.
    partial.coverage = after.coverage.clone();
    let needed_payloads = partial
        .objects
        .values()
        .filter_map(|value| value.declaration.known())
        .flat_map(TraceObject::payload_refs)
        .filter_map(|reference| {
            payloads
                .get(reference)
                .map(|value| (reference.clone(), value.clone()))
        })
        .collect();
    let mut projected = project(&partial, &needed_payloads, max_parts);
    // Tail rows have no activation and cannot change agent execution. Headers and
    // directories therefore retain their committed values, including full history.
    projected.headers.clear();
    projected.directories.clear();
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
    Some(projected)
}

#[cfg(test)]
mod tests;
