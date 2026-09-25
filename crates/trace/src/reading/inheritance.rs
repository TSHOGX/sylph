//! Positive retained context evidence; absence of production is never local ownership.
use super::*;

pub(super) fn inherited_entry(reduction: &Reduction, reference: &EntryRef) -> bool {
    matches!(reduction.object(&AnyObjectRef::HistoryEntry(reference.clone()))
        .and_then(|object| object.declaration.known()),
        Some(TraceObject::HistoryEntry(entry)) if matches!(&entry.original_owner,
            Some(OwnerRef::Agent { agent }) if agent != &entry.containing_agent))
}

pub(super) fn sources(reduction: &Reduction) -> BTreeMap<AgentRef, Vec<AnyObjectRef>> {
    let mut candidates = BTreeMap::new();
    for (agent, history) in &reduction.histories {
        if history.archive_order.is_empty() || !history.unresolved_entries.is_empty() {
            continue;
        }
        let mut sources = Vec::new();
        let complete = history.archive_order.iter().all(|entry| {
            if !inherited_entry(reduction, entry) {
                return false;
            }
            let contents = reduction.entry_contents(entry, None);
            if !contents.complete
                || contents.slots.is_empty()
                || contents.slots.values().any(|slot| slot.known().is_none())
            {
                return false;
            }
            sources.push(AnyObjectRef::HistoryEntry(entry.clone()));
            true
        });
        if complete {
            candidates.insert(agent.clone(), sources);
        }
    }
    if candidates.is_empty() {
        return candidates;
    }
    // Content can arrive before its entry declaration, and therefore before it
    // enters HistoryView. Retained orphan observations still prevent an exclusive
    // archive classification; their namespace does not establish provenance.
    for entry in reduction.history_contents.keys() {
        if !matches!(
            reduction
                .object(&AnyObjectRef::HistoryEntry(entry.clone()))
                .and_then(|object| object.declaration.known()),
            Some(TraceObject::HistoryEntry(_))
        ) {
            candidates.remove(entry.scope());
        }
    }
    // Walk objects once, not once per candidate agent. Conflicting declarations
    // cannot establish absence of local work; consider every retained alternative.
    for object in reduction.objects.values() {
        let alternatives: Vec<_> = match &object.declaration {
            Resolved::Known { value, .. } => vec![value],
            Resolved::Conflicted { alternatives } => {
                // The readable history excludes conflicted entry declarations.
                // Their absence from that view cannot certify the whole archive.
                for (value, _) in alternatives {
                    if let TraceObject::HistoryEntry(entry) = value {
                        candidates.remove(&entry.containing_agent);
                    }
                }
                alternatives.iter().map(|(value, _)| value).collect()
            }
            Resolved::Unknown => continue,
        };
        for value in alternatives {
            let owner = match value {
                TraceObject::Activation(value) => Some(&value.owner),
                TraceObject::Operation(value) => Some(&value.owner),
                TraceObject::Message(value) => value.production.as_ref().map(|scope| &scope.owner),
                TraceObject::Delivery(Delivery {
                    recipient: Recipient::Agent { agent },
                    ..
                }) => Some(agent),
                _ => None,
            };
            if let Some(owner) = owner {
                let reference = value.object_ref();
                let agent = match owner {
                    OwnerRef::Agent { agent } => agent,
                    // An unassigned operation can be local work. Refusing this
                    // aggregate claims no actor and leaves its bytes inspectable.
                    OwnerRef::Unknown | OwnerRef::Unresolved { .. } => reference.scope(),
                };
                candidates.remove(agent);
            }
        }
    }
    // Lifecycle evidence can precede the declaration that assigns its owner.
    // An unresolved local namespace is uncertainty, not inherited execution.
    for reference in reduction.executions.keys() {
        let known_owner = reduction
            .object(reference)
            .and_then(|object| object.declaration.known())
            .and_then(|object| match object {
                TraceObject::Activation(activation) => Some(&activation.owner),
                TraceObject::Operation(operation) => Some(&operation.owner),
                _ => None,
            });
        if !matches!(known_owner, Some(OwnerRef::Agent { .. })) {
            candidates.remove(reference.scope());
        }
    }
    candidates
}
