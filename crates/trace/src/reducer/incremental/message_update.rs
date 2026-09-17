//! A separate certificate for an existing message's immutable revision chain.
//!
//! The full declaration and message resolvers remain the semantic oracle. Their
//! input is indexed by message, so revisiting a long stream costs its own retained
//! claims and parts, never the unrelated archive. Fresh identities retain the same
//! unresolved-dependency and linear-tail guards as independent appends.

use super::*;

#[derive(Clone)]
pub struct MessageUpdate {
    before: Arc<Reduction>,
    after: Arc<Reduction>,
    witness: MessageWitness,
}

#[derive(Debug, Clone)]
pub(super) struct MessageWitness {
    message: MessageRef,
    activation: Option<ActivationRef>,
    pub(super) tail: AppendWitness,
}

impl std::fmt::Debug for MessageUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageUpdate")
            .field("witness", &self.witness)
            .finish_non_exhaustive()
    }
}

impl MessageUpdate {
    pub(super) fn new(
        before: Arc<Reduction>,
        after: Arc<Reduction>,
        witness: MessageWitness,
    ) -> Self {
        Self {
            before,
            after,
            witness,
        }
    }

    pub fn before(&self) -> &Arc<Reduction> {
        &self.before
    }
    pub fn after(&self) -> &Arc<Reduction> {
        &self.after
    }
    pub fn message(&self) -> &MessageRef {
        &self.witness.message
    }
    pub fn before_revision(&self) -> &Resolved<MessageRevisionRef> {
        &self.before.messages[&self.witness.message].selected
    }
    pub fn after_revision(&self) -> &Resolved<MessageRevisionRef> {
        &self.after.messages[&self.witness.message].selected
    }
    pub fn activation(&self) -> Option<&ActivationRef> {
        self.witness.activation.as_ref()
    }
    pub fn agents(&self) -> &BTreeMap<AgentRef, IndependentAppendAgent> {
        &self.witness.tail.agents
    }
    /// Includes the existing Message whose revision history changed, together
    /// with every freshly declared revision and history entry.
    pub fn updated_objects(&self) -> &BTreeSet<AnyObjectRef> {
        &self.witness.tail.objects
    }
    pub fn predecessor_relations(&self) -> std::slice::Iter<'_, ResolvedRelation> {
        self.witness.tail.predecessor_relations.iter()
    }
    pub fn matches(&self, before: &Reduction, after: &Reduction) -> bool {
        std::ptr::eq(self.before.as_ref(), before) && std::ptr::eq(self.after.as_ref(), after)
    }

    /// A certificate cannot cross a full reconciliation, a different message or
    /// a different activation. Both immutable intermediate allocations must match.
    pub fn compose(self, later: Self) -> Option<Self> {
        if self.witness.message != later.witness.message
            || self.witness.activation != later.witness.activation
        {
            return None;
        }
        let tail = IndependentAppend {
            before: self.before,
            after: self.after,
            witness: self.witness.tail,
        }
        .compose(IndependentAppend {
            before: later.before,
            after: later.after,
            witness: later.witness.tail,
        })?;
        Some(Self {
            before: tail.before,
            after: tail.after,
            witness: MessageWitness {
                message: self.witness.message,
                activation: self.witness.activation,
                tail: tail.witness,
            },
        })
    }
}

pub(super) fn resolve_message_update(
    before: &Reduction,
    index: &DependencyIndex,
    pending: &[&Claim],
    retained: &imbl::OrdMap<ClaimRef, Claim>,
) -> Option<(Reduction, ReductionChanges, MessageWitness, usize)> {
    if pending.is_empty()
        || pending
            .iter()
            .any(|claim| !claim.supersedes.is_empty() || index.claims.contains(&claim.claim_ref))
    {
        return None;
    }
    let mut messages = BTreeSet::new();
    let mut fresh = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    let mut entries = BTreeMap::new();
    let mut sources = BTreeSet::new();
    for claim in pending {
        // Even a newly addressed object can carry a previously aliased owner,
        // revision predecessor or message reference. Local resolution cannot
        // normalize just one side of that identity class.
        if dependencies::references(&claim.fact)
            .iter()
            .any(|reference| {
                canonical(&before.representatives, reference) != *reference
                    || canonical(
                        &before.representatives,
                        &AnyObjectRef::Agent(reference.scope().clone()),
                    ) != AnyObjectRef::Agent(reference.scope().clone())
            })
        {
            return None;
        }
        match &claim.fact {
            Fact::Declare { object } => {
                let reference = object.object_ref();
                if let TraceObject::Message(message) = object {
                    if !before.objects.contains_key(&reference) {
                        return None;
                    }
                    messages.insert(message.message_ref.clone());
                    continue;
                }
                if index.objects.contains(&reference)
                    || before.objects.contains_key(&reference)
                    || !fresh.insert(reference)
                {
                    return None;
                }
                match object {
                    TraceObject::MessageRevision(revision)
                        if matches!(
                            revision.form,
                            RevisionForm::Append { .. }
                                | RevisionForm::Replacement { .. }
                                | RevisionForm::Final { .. }
                        ) =>
                    {
                        messages.insert(revision.message.clone());
                        revisions.insert(revision.revision_ref.clone());
                    }
                    TraceObject::HistoryEntry(entry)
                        if entry.entry_ref.scope() == &entry.containing_agent =>
                    {
                        entries.insert(entry.entry_ref.clone(), entry.clone());
                    }
                    _ => return None,
                }
            }
            Fact::HistoryContentObserved { observation } => {
                sources.insert((
                    observation.position.source.clone(),
                    observation.position.generation,
                ));
            }
            Fact::HistoryPosition { position, .. } => {
                sources.insert((position.source.clone(), position.generation));
            }
            Fact::HistoryMappingComplete { extent } => {
                sources.insert((extent.source.clone(), extent.generation));
            }
            Fact::Relation {
                relation: Relation::HistoryPredecessor { .. },
            } => {}
            _ => return None,
        }
    }
    if messages.len() != 1
        || revisions.is_empty()
        || entries.is_empty()
        || sources
            .iter()
            .any(|source| index.selection_sources.contains(source))
        || entries.values().any(|entry| {
            index.selection_agents.contains(&entry.containing_agent)
                || index.ordered_agents.contains(&entry.containing_agent)
        })
    {
        return None;
    }
    let message = messages.into_iter().next()?;
    let message_object = AnyObjectRef::Message(message.clone());
    let TraceObject::Message(old_message) =
        before.objects.get(&message_object)?.declaration.known()?
    else {
        return None;
    };
    if old_message.production.as_ref().is_some_and(
        |scope| !matches!(&scope.owner, OwnerRef::Agent { agent } if agent == message.scope()),
    ) {
        return None;
    }
    if entries
        .values()
        .any(|entry| &entry.containing_agent != message.scope())
    {
        return None;
    }
    let old_refs = index.message_claims.get(&message)?;
    let old_claims: Vec<_> = old_refs
        .iter()
        .map(|reference| &retained[reference])
        .collect();
    if old_claims.iter().any(|claim| {
        !claim.supersedes.is_empty()
            || index.claims.contains(&claim.claim_ref)
            || dependencies::references(&claim.fact)
                .iter()
                .any(|reference| {
                    canonical(&before.representatives, reference) != *reference
                        || canonical(
                            &before.representatives,
                            &AnyObjectRef::Agent(reference.scope().clone()),
                        ) != AnyObjectRef::Agent(reference.scope().clone())
                })
            || matches!(
                &claim.fact,
                Fact::Declare {
                    object: TraceObject::MessageRevision(MessageRevision {
                        form: RevisionForm::Correction { .. },
                        ..
                    })
                }
            )
    }) {
        return None;
    }
    // Replaying the old local claims also checks for identity classes and withdrawn
    // declarations: their resolved support must match the retained global value.
    let old = resolve_local(&old_claims)?;
    if old.messages.get(&message) != before.messages.get(&message)
        || old
            .objects
            .iter()
            .any(|(reference, object)| before.objects.get(reference) != Some(object))
    {
        return None;
    }
    let all: BTreeMap<_, _> = old_claims
        .iter()
        .copied()
        .chain(pending.iter().copied())
        .map(|claim| (claim.claim_ref.clone(), claim))
        .collect();
    let mut local = resolve_local(&all.values().copied().collect::<Vec<_>>())?;
    let TraceObject::Message(updated) = local.objects.get(&message_object)?.declaration.known()?
    else {
        return None;
    };
    if updated.author != old_message.author
        || updated.protocol_role != old_message.protocol_role
        || updated.production != old_message.production
        || (updated.purpose != old_message.purpose
            && old_message.purpose != MessagePurpose::Unclassified)
    {
        return None;
    }
    // Existing-message presentation stays a single known stream in this lane.
    // Conflicting finals remain inspectable through the full reconciliation path.
    before.messages.get(&message)?.selected.known()?;
    local.messages.get(&message)?.selected.known()?;
    for reference in &revisions {
        let TraceObject::MessageRevision(revision) = local
            .objects
            .get(&AnyObjectRef::MessageRevision(reference.clone()))?
            .declaration
            .known()?
        else {
            return None;
        };
        if revision.message != message {
            return None;
        }
        if let RevisionForm::Append {
            predecessor: Some(predecessor),
        } = &revision.form
        {
            let TraceObject::MessageRevision(previous) = local
                .objects
                .get(&AnyObjectRef::MessageRevision(predecessor.clone()))?
                .declaration
                .known()?
            else {
                return None;
            };
            if previous.message != message {
                return None;
            }
        }
    }
    let mut observed = BTreeSet::new();
    for claim in pending {
        match &claim.fact {
            Fact::HistoryContentObserved { observation } => {
                if !entries.contains_key(&observation.entry) || !observed.insert(observation.entry.clone())
                    || !observation.complete || observation.slots.is_empty()
                    || observation.slots.iter().any(|slot| !matches!(&slot.content, HistoryContent::Message(revision) if revisions.contains(revision))) {
                    return None;
                }
                local
                    .history_contents
                    .entry(observation.entry.clone())
                    .or_default()
                    .insert(claim.claim_ref.clone(), observation.clone());
            }
            Fact::HistoryPosition { entry, .. } if !entries.contains_key(entry) => return None,
            Fact::Relation {
                relation: relation @ Relation::HistoryPredecessor { predecessor, entry },
            } => {
                if entries.get(entry)?.predecessor.as_ref() != Some(predecessor) {
                    return None;
                }
                local.relations.push_back(ResolvedRelation {
                    claim: claim.claim_ref.clone(),
                    relation: relation.clone(),
                });
            }
            _ => {}
        }
    }
    if observed.len() != entries.len() {
        return None;
    }
    fresh.insert(message_object);
    let diagnostics = std::mem::take(&mut local.diagnostics);
    let (mut result, mut changes, tail) = install_tail(before, local, &entries, fresh)?;
    // With no corrections or withdrawn claims the set of seals only grows, so
    // old late-segment diagnostics cannot disappear. Insert newly proved segments
    // at their sorted positions without copying/scanning unrelated diagnostics.
    let old_diagnostics: BTreeMap<_, _> = old
        .diagnostics
        .iter()
        .map(|diagnostic| {
            (
                (diagnostic.claims.clone(), diagnostic.message.clone()),
                diagnostic,
            )
        })
        .collect();
    let new_keys: BTreeSet<_> = diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.claims.clone(), diagnostic.message.clone()))
        .collect();
    if old_diagnostics.keys().any(|key| !new_keys.contains(key)) {
        return None;
    }
    let mut diagnostics_changed = false;
    for diagnostic in diagnostics {
        let key = (&diagnostic.claims, &diagnostic.message);
        if old_diagnostics.contains_key(&(diagnostic.claims.clone(), diagnostic.message.clone())) {
            continue;
        }
        let insertion = result
            .diagnostics
            .binary_search_by(|old| (&old.claims, &old.message).cmp(&key))
            .err()?;
        result.diagnostics.insert(insertion, diagnostic);
        diagnostics_changed = true;
    }
    if diagnostics_changed {
        changes.diagnostics = Some(ChangedValue {
            before: Some(before.diagnostics.clone()),
            after: result.diagnostics.clone(),
        });
    }
    let activation = old_message
        .production
        .as_ref()
        .and_then(|scope| scope.activation.clone());
    Some((
        result,
        changes,
        MessageWitness {
            message,
            activation,
            tail,
        },
        old_claims.len() * 2 + pending.len(),
    ))
}

fn resolve_local(claims: &[&Claim]) -> Option<Reduction> {
    let mut local = Reduction::default();
    let declarations = declarations(claims, &mut local);
    resolve_objects(&declarations, &mut local);
    resolve_messages(claims, &mut local);
    if local
        .objects
        .values()
        .any(|object| object.declaration.known().is_none())
        || local
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code != DiagnosticCode::LateSegment)
    {
        return None;
    }
    Some(local)
}

#[cfg(test)]
mod tests;
