//! Incremental resolution over a proved independent native-history tail.
use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReductionWork {
    pub claims_resolved: usize,
    pub history_entries_updated: usize,
    pub dependency_references_indexed: usize,
    pub full_resolutions: usize,
    pub dependency_index_unavailable: bool,
}

impl ReductionWork {
    pub fn accumulate(&mut self, other: &Self) {
        self.claims_resolved += other.claims_resolved;
        self.history_entries_updated += other.history_entries_updated;
        self.dependency_references_indexed += other.dependency_references_indexed;
        self.full_resolutions += other.full_resolutions;
        self.dependency_index_unavailable |= other.dependency_index_unavailable;
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub reduction: Arc<Reduction>,
    pub changes: ReductionChanges,
    pub work: ReductionWork,
    pub append: Option<IndependentAppend>,
}

/// Reducer-issued evidence that only fresh independent history tails changed.
/// Strong endpoint references keep the checked values immutable and prevent a
/// reused allocation address from satisfying a later consumer's identity check.
#[derive(Clone)]
pub struct IndependentAppend {
    before: Arc<Reduction>,
    after: Arc<Reduction>,
    witness: AppendWitness,
}

#[derive(Debug, Clone)]
pub struct IndependentAppendAgent {
    previous_archive_len: usize,
    previous_head: EntryRef,
    added_entries: Vec<EntryRef>,
}

impl IndependentAppendAgent {
    pub fn previous_archive_len(&self) -> usize {
        self.previous_archive_len
    }

    pub fn previous_head(&self) -> &EntryRef {
        &self.previous_head
    }

    pub fn added_entries(&self) -> &[EntryRef] {
        &self.added_entries
    }
}

#[derive(Debug, Clone, Default)]
struct AppendWitness {
    agents: BTreeMap<AgentRef, IndependentAppendAgent>,
    objects: BTreeSet<AnyObjectRef>,
    predecessor_relations: Vec<ResolvedRelation>,
}

impl std::fmt::Debug for IndependentAppend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndependentAppend")
            .field("witness", &self.witness)
            .finish_non_exhaustive()
    }
}

impl IndependentAppend {
    pub fn before(&self) -> &Arc<Reduction> {
        &self.before
    }

    pub fn after(&self) -> &Arc<Reduction> {
        &self.after
    }

    pub fn agents(&self) -> &BTreeMap<AgentRef, IndependentAppendAgent> {
        &self.witness.agents
    }

    pub fn objects(&self) -> &BTreeSet<AnyObjectRef> {
        &self.witness.objects
    }

    pub fn predecessor_relations(&self) -> std::slice::Iter<'_, ResolvedRelation> {
        self.witness.predecessor_relations.iter()
    }

    pub fn matches(&self, before: &Reduction, after: &Reduction) -> bool {
        std::ptr::eq(self.before.as_ref(), before) && std::ptr::eq(self.after.as_ref(), after)
    }

    /// Compose only certificates sharing the exact immutable intermediate state.
    /// Work is proportional to certified additions, never prior history length.
    pub fn compose(mut self, later: Self) -> Option<Self> {
        if !Arc::ptr_eq(&self.after, &later.before) {
            return None;
        }
        for (agent, addition) in later.witness.agents {
            if let Some(previous) = self.witness.agents.get_mut(&agent) {
                if previous
                    .previous_archive_len
                    .checked_add(previous.added_entries.len())?
                    != addition.previous_archive_len
                    || previous.added_entries.last() != Some(&addition.previous_head)
                {
                    return None;
                }
                previous.added_entries.extend(addition.added_entries);
            } else {
                self.witness.agents.insert(agent, addition);
            }
        }
        self.witness.objects.extend(later.witness.objects);
        self.witness
            .predecessor_relations
            .extend(later.witness.predecessor_relations);
        self.witness
            .predecessor_relations
            .sort_by(|left, right| left.claim.cmp(&right.claim));
        self.after = later.after;
        Some(self)
    }

    fn unchanged(reduction: Arc<Reduction>) -> Self {
        Self {
            before: reduction.clone(),
            after: reduction,
            witness: AppendWitness::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct IncrementalState {
    pub(super) pending: imbl::OrdSet<ClaimRef>,
    cached: Option<Arc<Reduction>>,
    index: Option<DependencyIndex>,
    index_limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DependencyIndex {
    objects: imbl::OrdSet<AnyObjectRef>,
    claims: imbl::OrdSet<ClaimRef>,
    selection_agents: imbl::OrdSet<AgentRef>,
    ordered_agents: imbl::OrdSet<AgentRef>,
    selection_sources: imbl::OrdSet<(SourceRef, u64)>,
}

impl DependencyIndex {
    fn extend<'a>(
        &mut self,
        claims: impl IntoIterator<Item = &'a Claim>,
        limit: usize,
        work: &mut ReductionWork,
    ) -> bool {
        for claim in claims {
            let objects = dependencies::references(&claim.fact);
            let mut referenced_claims = dependencies::claim_references(&claim.fact);
            referenced_claims.extend(claim.supersedes.iter().cloned());
            let sources = if matches!(
                claim.fact,
                Fact::Declare {
                    object: TraceObject::HistorySelection(_)
                }
            ) {
                dependencies::selection_sources(&claim.fact)
            } else {
                BTreeSet::new()
            };
            work.dependency_references_indexed +=
                objects.len() + referenced_claims.len() + sources.len();
            self.objects.extend(objects);
            self.claims.extend(referenced_claims);
            self.selection_sources.extend(sources);
            if let Fact::Declare {
                object: TraceObject::HistorySelection(selection),
            } = &claim.fact
            {
                self.selection_agents.insert(selection.agent.clone());
            }
            if let Fact::Relation {
                relation: Relation::ReadingOrder { before, after },
            } = &claim.fact
            {
                self.ordered_agents.insert(before.scope().clone());
                self.ordered_agents.insert(after.scope().clone());
            }
            let size = self
                .objects
                .len()
                .saturating_add(self.claims.len())
                .saturating_add(self.selection_agents.len())
                .saturating_add(self.selection_sources.len())
                .saturating_add(self.ordered_agents.len());
            if size > limit {
                return false;
            }
        }
        true
    }
}

impl Reducer {
    /// The dependency budget bounds retained optimization metadata, not evidence.
    /// Exhaustion selects full resolution; it never drops claims or invents a value.
    pub fn resolve_incremental(&mut self, max_dependencies: usize) -> Resolution {
        let previous = self.incremental.cached.clone();
        if self.incremental.pending.is_empty() && self.incremental.index_limit == max_dependencies {
            if let Some(reduction) = previous {
                return Resolution {
                    append: Some(IndependentAppend::unchanged(reduction.clone())),
                    reduction,
                    changes: ReductionChanges::default(),
                    work: ReductionWork {
                        dependency_index_unavailable: self.incremental.index.is_none(),
                        ..Default::default()
                    },
                };
            }
        }
        let pending: Vec<_> = self
            .incremental
            .pending
            .iter()
            .map(|reference| &self.claims[reference])
            .collect();
        let mut work = ReductionWork::default();
        let incremental = previous
            .as_deref()
            .zip(self.incremental.index.as_ref())
            .filter(|_| self.incremental.index_limit == max_dependencies)
            .and_then(|(before, index)| resolve_tail(before, index, &pending));
        let (reduction, changes, append) = if let Some((reduction, changes, witness)) = incremental
        {
            work.claims_resolved = pending.len();
            work.history_entries_updated = witness
                .agents
                .values()
                .map(|agent| agent.added_entries.len())
                .sum();
            let reduction = Arc::new(reduction);
            let append = IndependentAppend {
                before: previous
                    .as_ref()
                    .expect("tail resolution has an initialized predecessor")
                    .clone(),
                after: reduction.clone(),
                witness,
            };
            (reduction, changes, Some(append))
        } else {
            let reduction = Arc::new(self.resolve());
            work.claims_resolved = self.claims.len();
            work.full_resolutions = 1;
            work.history_entries_updated = reduction
                .histories
                .values()
                .map(|history| history.archive_order.len())
                .sum();
            let changes = ReductionChanges::between(
                previous.as_deref().unwrap_or(&Reduction::default()),
                &reduction,
            );
            (reduction, changes, None)
        };
        if previous.is_none() || self.incremental.index_limit != max_dependencies {
            let mut index = DependencyIndex::default();
            self.incremental.index = index
                .extend(self.claims.values(), max_dependencies, &mut work)
                .then_some(index);
        } else if let Some(index) = &mut self.incremental.index {
            if !index.extend(pending, max_dependencies, &mut work) {
                self.incremental.index = None;
            }
        }
        work.dependency_index_unavailable = self.incremental.index.is_none();
        self.incremental.index_limit = max_dependencies;
        self.incremental.pending.clear();
        self.incremental.cached = Some(reduction.clone());
        Resolution {
            reduction,
            changes,
            work,
            append,
        }
    }
}

fn resolve_tail(
    before: &Reduction,
    index: &DependencyIndex,
    claims: &[&Claim],
) -> Option<(Reduction, ReductionChanges, AppendWitness)> {
    if claims.is_empty()
        || claims
            .iter()
            .any(|claim| !claim.supersedes.is_empty() || index.claims.contains(&claim.claim_ref))
    {
        return None;
    }
    let mut declared = BTreeSet::new();
    let mut messages = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    let mut entries = BTreeMap::new();
    let mut sources = BTreeSet::new();
    for claim in claims {
        match &claim.fact {
            Fact::Declare { object } => {
                let reference = object.object_ref();
                if index.objects.contains(&reference)
                    || before.objects.contains_key(&reference)
                    || !declared.insert(reference.clone())
                {
                    return None;
                }
                // Existing equivalence classes can rewrite a nested owner even
                // when the newly declared object itself has no prior identity.
                if dependencies::references(&claim.fact)
                    .iter()
                    .any(|dependency| {
                        canonical(&before.representatives, dependency) != *dependency
                            || canonical(
                                &before.representatives,
                                &AnyObjectRef::Agent(dependency.scope().clone()),
                            ) != AnyObjectRef::Agent(dependency.scope().clone())
                    })
                {
                    return None;
                }
                match object {
                    TraceObject::Message(message) => {
                        if message.production.as_ref().is_some_and(|scope| scope.activation.is_some()
                            || !matches!(&scope.owner, OwnerRef::Agent { agent } if agent == reference.scope())) {
                            return None;
                        }
                        messages.insert(message.message_ref.clone());
                    }
                    TraceObject::MessageRevision(revision)
                        if revision.form == RevisionForm::Complete =>
                    {
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
    if entries.is_empty()
        || messages.is_empty()
        || revisions.is_empty()
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
    let mut local = Reduction::default();
    let declarations = declarations(claims, &mut local);
    resolve_objects(&declarations, &mut local);
    resolve_messages(claims, &mut local);
    if !local.diagnostics.is_empty()
        || local.objects.len() != declared.len()
        || local
            .objects
            .values()
            .any(|object| object.declaration.known().is_none())
        || local.messages.len() != messages.len()
        || local.messages.iter().any(|(message, history)| {
            !messages.contains(message)
                || history.revisions.len() != 1
                || !revisions.contains(&history.revisions[0])
        })
    {
        return None;
    }
    for reference in &revisions {
        let TraceObject::MessageRevision(revision) = local
            .objects
            .get(&AnyObjectRef::MessageRevision(reference.clone()))?
            .declaration
            .known()?
        else {
            return None;
        };
        if !messages.contains(&revision.message) {
            return None;
        }
    }
    let mut observed_entries = BTreeSet::new();
    for claim in claims {
        match &claim.fact {
            Fact::HistoryContentObserved { observation } => {
                if !entries.contains_key(&observation.entry) || !observed_entries.insert(observation.entry.clone())
                    || !observation.complete || observation.slots.iter().any(|slot|
                        !matches!(&slot.content, HistoryContent::Message(revision) if revisions.contains(revision))) {
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
    if observed_entries.len() != entries.len() {
        return None;
    }
    let mut by_agent = BTreeMap::<AgentRef, BTreeMap<EntryRef, EntryRef>>::new();
    for entry in entries.values() {
        let parent = entry.predecessor.as_ref()?;
        if by_agent
            .entry(entry.containing_agent.clone())
            .or_default()
            .insert(parent.clone(), entry.entry_ref.clone())
            .is_some()
        {
            return None;
        }
    }
    let mut result = before.clone();
    let mut changes = ReductionChanges::default();
    let mut witness = AppendWitness {
        objects: declared,
        ..AppendWitness::default()
    };
    for (agent, mut successors) in by_agent {
        let old = before.histories.get(&agent)?;
        if old.candidate_heads.len() != 1
            || !old.unresolved_entries.is_empty()
            || old.selected_entries.is_some()
            || old.archive_order.back() != old.candidate_heads.front()
        {
            return None;
        }
        let mut history = old.clone();
        let mut tail = old.archive_order.back()?.clone();
        let previous_head = tail.clone();
        let mut added_entries = Vec::new();
        while let Some(next) = successors.remove(&tail) {
            history.archive_order.push_back(next.clone());
            history
                .order_basis
                .insert(next.clone(), ReadingOrderBasis::NativePredecessor);
            added_entries.push(next.clone());
            tail = next;
        }
        if !successors.is_empty() {
            return None;
        }
        history.candidate_heads = std::iter::once(tail).collect();
        witness.agents.insert(
            agent.clone(),
            IndependentAppendAgent {
                previous_archive_len: old.archive_order.len(),
                previous_head,
                added_entries,
            },
        );
        result.histories.insert(agent.clone(), history.clone());
        changes.histories.changed.insert(
            agent,
            ChangedValue {
                before: Some(old.clone()),
                after: history,
            },
        );
    }
    for (reference, object) in &local.objects {
        result.objects.insert(reference.clone(), object.clone());
        result
            .representatives
            .insert(reference.clone(), reference.clone());
        changes.objects.changed.insert(
            reference.clone(),
            ChangedValue {
                before: None,
                after: object.clone(),
            },
        );
        changes.representatives.changed.insert(
            reference.clone(),
            ChangedValue {
                before: None,
                after: reference.clone(),
            },
        );
    }
    for (message, history) in local.messages {
        result.messages.insert(message.clone(), history.clone());
        changes.messages.changed.insert(
            message,
            ChangedValue {
                before: None,
                after: history,
            },
        );
    }
    for (entry, observations) in local.history_contents {
        result
            .history_contents
            .insert(entry.clone(), observations.clone());
        changes.history_contents.changed.insert(
            entry,
            ChangedValue {
                before: None,
                after: observations,
            },
        );
    }
    for relation in local.relations {
        witness.predecessor_relations.push(relation.clone());
        let insertion = result
            .relations
            .binary_search_by(|old| old.claim.cmp(&relation.claim))
            .unwrap_err();
        changes.relations.changed.insert(
            relation.claim.clone(),
            ChangedValue {
                before: None,
                after: relation.relation.clone(),
            },
        );
        result.relations.insert(insertion, relation);
    }
    Some((result, changes, witness))
}

#[cfg(test)]
mod tests;
