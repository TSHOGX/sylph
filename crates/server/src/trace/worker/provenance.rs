//! Retained support, separate from arrival order and from referenced authorship.
//!
//! The least declaration (ClaimRef, ObservationRef) pair fixes the primary epoch.
//! Other resolved fields contribute dependency epochs without changing that primary.
//! Relation evidence belongs to relation rows, not to both endpoint headers: an
//! unavailable parent must remain redactable without withholding a child's bytes.

use super::*;
use sylph_trace::reducer::{MapChanges, ReductionError, Resolved};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectProvenance {
    pub primary_claim: ClaimRef,
    pub primary_observation: ObservationRef,
    pub primary_epoch: IngestionEpoch,
    pub dependency_epochs: imbl::OrdSet<IngestionEpoch>,
}

pub(crate) type ProvenanceMap = imbl::OrdMap<AnyObjectRef, Arc<ObjectProvenance>>;
type WitnessDomain = (SourceRef, u64, HistoryCoordinate, String);

/// Disposable, structurally shared reverse indexes. A full reconciliation rebuilds
/// them; proved append/message changes touch only incoming claims and changed keys.
#[derive(Clone, Default)]
pub(super) struct SupportIndex {
    direct: imbl::OrdMap<AnyObjectRef, imbl::OrdSet<ClaimRef>>,
    support: imbl::OrdMap<AnyObjectRef, imbl::OrdSet<ClaimRef>>,
    relations: imbl::OrdMap<(AnyObjectRef, AnyObjectRef), imbl::OrdSet<ClaimRef>>,
    source_witnesses: imbl::OrdMap<WitnessDomain, imbl::OrdSet<ClaimRef>>,
    claim_epochs: imbl::OrdMap<ClaimRef, imbl::OrdSet<IngestionEpoch>>,
}

impl SupportIndex {
    #[cfg(test)]
    pub(super) fn fixture_epochs(
        values: impl IntoIterator<Item = (ClaimRef, IngestionEpoch)>,
    ) -> Self {
        Self {
            claim_epochs: values
                .into_iter()
                .map(|(claim, epoch)| (claim, imbl::ordset![epoch]))
                .collect(),
            ..Default::default()
        }
    }

    pub(super) fn claim_dependencies(
        &self,
        claim: &ClaimRef,
    ) -> Option<&imbl::OrdSet<IngestionEpoch>> {
        self.claim_epochs.get(claim)
    }

    /// Endpoints are canonical object references, as used by reader projections.
    pub(super) fn relation_dependencies(
        &self,
        source: &AnyObjectRef,
        target: &AnyObjectRef,
    ) -> imbl::OrdSet<IngestionEpoch> {
        self.relations
            .get(&(source.clone(), target.clone()))
            .into_iter()
            .flatten()
            .filter_map(|claim| self.claim_dependencies(claim))
            .flatten()
            .cloned()
            .collect()
    }

    fn advance(
        previous: Option<&Self>,
        reduction: &Reduction,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        incoming: &[Claim],
        changes: &ReductionChanges,
        incremental: bool,
    ) -> (Self, BTreeSet<AnyObjectRef>, BTreeSet<ClaimRef>) {
        let incremental = incremental && previous.is_some();
        let mut next = if incremental {
            previous.cloned().unwrap_or_default()
        } else {
            Self {
                claim_epochs: previous
                    .map(|index| index.claim_epochs.clone())
                    .unwrap_or_default(),
                ..Self::default()
            }
        };
        let mut affected = BTreeSet::new();
        let mut needed = BTreeSet::new();
        if incremental {
            for claim in incoming {
                next.index_direct(reduction, claim, &mut affected);
                needed.insert(claim.claim_ref.clone());
            }
            for (claim, relation) in &changes.relations.changed {
                next.index_relation(reduction, claim, &relation.after, &mut affected);
            }
            affected.extend(changed_keys(&changes.objects));
            affected.extend(changed_keys(&changes.executions));
            affected.extend(changed_keys(&changes.delivery_stages).map(AnyObjectRef::Delivery));
            affected.extend(changed_keys(&changes.delivery_revisions).map(AnyObjectRef::Delivery));
            affected.extend(changed_keys(&changes.resource_states).map(AnyObjectRef::Resource));
            affected.extend(changed_keys(&changes.agent_states).map(AnyObjectRef::Agent));
            affected.extend(changed_keys(&changes.histories).map(AnyObjectRef::Agent));
            affected
                .extend(changed_keys(&changes.history_contents).map(AnyObjectRef::HistoryEntry));
            affected.extend(changed_keys(&changes.messages).map(AnyObjectRef::Message));
            affected.extend(
                changed_keys(&changes.selection_memberships).map(AnyObjectRef::HistorySelection),
            );
            affected.extend(
                changed_keys(&changes.selection_content_observations)
                    .map(AnyObjectRef::HistorySelection),
            );
        } else {
            let withdrawn: BTreeSet<_> = reduction.withdrawn_claims.iter().collect();
            for claim in claims
                .values()
                .filter(|claim| !withdrawn.contains(&claim.claim_ref))
            {
                next.index_direct(reduction, claim, &mut affected);
                needed.insert(claim.claim_ref.clone());
            }
            for relation in &reduction.relations {
                next.index_relation(
                    reduction,
                    &relation.claim,
                    &relation.relation,
                    &mut affected,
                );
            }
            affected.extend(reduction.objects.keys().cloned());
        }
        for reference in &affected {
            if let Some(object) = reduction.objects.get(reference) {
                let mut support = next.direct.get(reference).cloned().unwrap_or_default();
                support.extend(resolved_claims(&object.declaration));
                if let Some(execution) = reduction.executions.get(reference) {
                    support.extend(execution.boundaries.iter().map(|(_, claim)| claim.clone()));
                    support.extend(execution.started.iter().cloned());
                    support.extend(execution.terminated.iter().cloned());
                    support.extend(execution.interrupt_requests.iter().cloned());
                    support.extend(resolved_claims(&execution.outcome));
                }
                match reference {
                    AnyObjectRef::Agent(agent) => {
                        support.extend(
                            reduction
                                .agent_states
                                .get(agent)
                                .into_iter()
                                .flatten()
                                .map(|(_, claim)| claim.clone()),
                        );
                        if let Some(history) = reduction.histories.get(agent) {
                            support.extend(resolved_claims(&history.selection));
                        }
                    }
                    AnyObjectRef::Delivery(delivery) => {
                        support.extend(
                            reduction
                                .delivery_stages
                                .get(delivery)
                                .into_iter()
                                .flatten()
                                .map(|(_, claim)| claim.clone()),
                        );
                        support.extend(
                            reduction
                                .delivery_revisions
                                .get(delivery)
                                .into_iter()
                                .flat_map(|bindings| bindings.keys().cloned()),
                        );
                    }
                    AnyObjectRef::Resource(resource) => {
                        support.extend(
                            reduction
                                .resource_states
                                .get(resource)
                                .into_iter()
                                .flatten()
                                .map(|(_, claim)| claim.clone()),
                        );
                    }
                    AnyObjectRef::HistoryEntry(entry) => {
                        support.extend(
                            reduction
                                .history_contents
                                .get(entry)
                                .into_iter()
                                .flat_map(|bindings| bindings.keys().cloned()),
                        );
                    }
                    AnyObjectRef::HistorySelection(selection) => {
                        support.extend(next.selection_support(reduction, claims, selection));
                    }
                    _ => {}
                }
                needed.extend(support.iter().cloned());
                next.support.insert(reference.clone(), support);
            }
        }
        for reference in changes.objects.removed.keys() {
            next.direct.remove(reference);
            next.support.remove(reference);
        }
        (next, affected, needed)
    }

    fn index_direct(
        &mut self,
        reduction: &Reduction,
        claim: &Claim,
        affected: &mut BTreeSet<AnyObjectRef>,
    ) {
        if let Fact::HistoryMappingComplete { extent } = &claim.fact {
            self.source_witnesses
                .entry(witness_domain(extent))
                .or_default()
                .insert(claim.claim_ref.clone());
        }
        // Header coverage summarizes claims for every subject in this agent's
        // scope, independently of the object's own detail coverage.
        if let Fact::Coverage { coverage } = &claim.fact {
            let agent = canonical(
                reduction,
                &AnyObjectRef::Agent(coverage.subject.scope().clone()),
            );
            self.direct
                .entry(agent.clone())
                .or_default()
                .insert(claim.claim_ref.clone());
            affected.insert(agent);
        }
        let reference = match &claim.fact {
            Fact::HistoryPosition { entry, .. } => Some(AnyObjectRef::HistoryEntry(entry.clone())),
            Fact::Coverage { coverage } => Some(coverage.subject.clone()),
            Fact::Diagnostic { diagnostic } => diagnostic.subject.clone(),
            Fact::Equivalence { left, right, rule }
                if left != right
                    && !rule.trim().is_empty()
                    && canonical(reduction, left) == canonical(reduction, right) =>
            {
                Some(left.clone())
            }
            _ => None,
        };
        if let Some(reference) = reference {
            let reference = canonical(reduction, &reference);
            self.direct
                .entry(reference.clone())
                .or_default()
                .insert(claim.claim_ref.clone());
            affected.insert(reference);
        }
    }

    fn selection_support(
        &self,
        reduction: &Reduction,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        selection: &SelectionRef,
    ) -> imbl::OrdSet<ClaimRef> {
        let mut support = imbl::OrdSet::new();
        let mut pending = vec![selection.clone()];
        let mut visited = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            let Some(object) = reduction.object(&AnyObjectRef::HistorySelection(reference)) else {
                continue;
            };
            if !visited.insert(object.reference.clone()) {
                continue;
            }
            support.extend(resolved_claims(&object.declaration));
            let Some(TraceObject::HistorySelection(selection)) = object.declaration.known() else {
                continue;
            };
            if let SelectionMembership::SourceExtents {
                extents,
                inherited_selections,
            } = &selection.membership
            {
                pending.extend(inherited_selections.iter().cloned());
                for extent in extents {
                    for claim in self
                        .source_witnesses
                        .get(&witness_domain(extent))
                        .into_iter()
                        .flatten()
                    {
                        let Some(Claim {
                            fact: Fact::HistoryMappingComplete { extent: witness },
                            ..
                        }) = claims.get(claim)
                        else {
                            continue;
                        };
                        if extent.start_inclusive < witness.end_exclusive
                            && witness.start_inclusive < extent.end_exclusive
                        {
                            support.insert(claim.clone());
                        }
                    }
                }
            }
            // Membership and content support is scoped to the actual selected set,
            // never to every entry in the containing agent's retained archive.
            for entry in reduction
                .selection_memberships
                .get(&selection.selection_ref)
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Some(object) = reduction.object(&AnyObjectRef::HistoryEntry(entry.clone())) {
                    support.extend(resolved_claims(&object.declaration));
                    support.extend(
                        self.direct
                            .get(&object.reference)
                            .into_iter()
                            .flatten()
                            .cloned(),
                    );
                }
            }
            support.extend(
                reduction
                    .selection_content_observations
                    .get(&selection.selection_ref)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .cloned(),
            );
        }
        support
    }

    fn index_relation(
        &mut self,
        reduction: &Reduction,
        claim: &ClaimRef,
        relation: &Relation,
        affected: &mut BTreeSet<AnyObjectRef>,
    ) {
        // History ordering changes a row's own position. Other relation types
        // are separately authorized link rows and never poison endpoint headers.
        let positioned = match relation {
            Relation::HistoryPredecessor { entry, .. } => vec![entry],
            Relation::ReadingOrder { before, after } => vec![before, after],
            _ => vec![],
        };
        for entry in positioned {
            let reference = canonical(reduction, &AnyObjectRef::HistoryEntry(entry.clone()));
            self.direct
                .entry(reference.clone())
                .or_default()
                .insert(claim.clone());
            affected.insert(reference);
        }
        let subjects: BTreeSet<_> = relation
            .subject_refs()
            .iter()
            .map(|reference| canonical(reduction, reference))
            .collect();
        for source in &subjects {
            for target in &subjects {
                if source != target {
                    self.relations
                        .entry((source.clone(), target.clone()))
                        .or_default()
                        .insert(claim.clone());
                }
            }
        }
    }
}

impl ProjectionWorker {
    /// Restore the disposable support index without changing the persisted row
    /// provenance that defines the baseline for the next candidate comparison.
    pub(super) async fn restore_provenance_index(
        &self,
        state: &mut WorkerState,
        through_cut: u64,
    ) -> Result<()> {
        let claims = state.reducer.claim_snapshot();
        let (mut index, _, needed) = SupportIndex::advance(
            None,
            &state.reduction,
            &claims,
            &[],
            &ReductionChanges::default(),
            false,
        );
        self.load_provenance_epochs(&mut index, &claims, needed, through_cut)
            .await?;
        state.provenance_index = Some(index);
        Ok(())
    }

    async fn load_provenance_epochs(
        &self,
        index: &mut SupportIndex,
        claims: &imbl::OrdMap<ClaimRef, Claim>,
        needed: BTreeSet<ClaimRef>,
        through_cut: u64,
    ) -> Result<BTreeMap<ObservationRef, IngestionEpoch>> {
        let mut epochs: BTreeMap<ObservationRef, IngestionEpoch> = BTreeMap::new();
        for claim_ref in needed {
            if index.claim_epochs.contains_key(&claim_ref) {
                continue;
            }
            let claim = claims
                .get(&claim_ref)
                .ok_or(ReductionError::MissingEvidence)?;
            let mut dependencies = imbl::OrdSet::new();
            for observation in &claim.evidence {
                let epoch = if let Some(epoch) = epochs.get(observation) {
                    epoch.clone()
                } else {
                    let epoch = self
                        .evidence
                        .observation_epoch(observation, through_cut)
                        .await?
                        .ok_or(ReductionError::MissingEvidence)?;
                    epochs.insert(observation.clone(), epoch.clone());
                    epoch
                };
                dependencies.insert(epoch);
            }
            index.claim_epochs.insert(claim_ref, dependencies);
        }
        Ok(epochs)
    }

    /// A candidate's support index and durable provenance advance with its rows.
    /// Failed/cancelled work cannot alter the committed authorization baseline.
    pub(super) async fn refresh_provenance(
        &self,
        unit: &mut ProjectionUnit,
        changes: &ReductionChanges,
    ) -> Result<()> {
        if changes.is_empty() && unit.next.provenance_index.is_some() {
            return Ok(());
        }
        let claims = unit.next.reducer.claim_snapshot();
        let (mut index, affected, needed) = SupportIndex::advance(
            unit.next.provenance_index.as_ref(),
            &unit.next.reduction,
            &claims,
            &unit.batch.claims,
            changes,
            unit.append.is_some() || unit.message_update.is_some(),
        );
        let epochs = self
            .load_provenance_epochs(&mut index, &claims, needed, unit.batch.through_cut)
            .await?;
        for reference in affected {
            let Some(object) = unit.next.reduction.objects.get(&reference) else {
                continue;
            };
            let primary = resolved_claims(&object.declaration)
                .into_iter()
                .filter_map(|reference| claims.get(&reference))
                .flat_map(|claim| {
                    claim
                        .evidence
                        .iter()
                        .map(move |observation| (&claim.claim_ref, observation))
                })
                .min()
                .ok_or(ReductionError::MissingEvidence)?;
            let prior = unit.next.object_provenance.get(&reference).filter(|prior| {
                &prior.primary_claim == primary.0 && &prior.primary_observation == primary.1
            });
            let primary_epoch = if let Some(prior) = prior {
                prior.primary_epoch.clone()
            } else if let Some(epoch) = epochs.get(primary.1) {
                epoch.clone()
            } else {
                self.evidence
                    .observation_epoch(primary.1, unit.batch.through_cut)
                    .await?
                    .ok_or(ReductionError::MissingEvidence)?
            };
            let dependency_epochs = index
                .support
                .get(&reference)
                .into_iter()
                .flatten()
                .filter_map(|claim| index.claim_dependencies(claim))
                .flatten()
                .cloned()
                .collect();
            let value = ObjectProvenance {
                primary_claim: primary.0.clone(),
                primary_observation: primary.1.clone(),
                primary_epoch,
                dependency_epochs,
            };
            if unit
                .next
                .object_provenance
                .get(&reference)
                .is_some_and(|previous| previous.as_ref() == &value)
            {
                continue;
            }
            unit.batch.mapper_states.insert(
                format!("object_provenance:{}", reference.encode()),
                serde_json::to_value(&value)?,
            );
            unit.next
                .object_provenance
                .insert(reference, Arc::new(value));
        }
        for reference in changes.objects.removed.keys() {
            if unit.next.object_provenance.remove(reference).is_some() {
                unit.batch.mapper_states.insert(
                    format!("object_provenance:{}", reference.encode()),
                    serde_json::Value::Null,
                );
            }
        }
        unit.next.provenance_index = Some(index);
        Ok(())
    }
}

fn canonical(reduction: &Reduction, reference: &AnyObjectRef) -> AnyObjectRef {
    reduction
        .representatives
        .get(reference)
        .unwrap_or(reference)
        .clone()
}

fn witness_domain(extent: &SourceExtent) -> WitnessDomain {
    (
        extent.source.clone(),
        extent.generation,
        extent.coordinate.clone(),
        extent.mapping_rule.clone(),
    )
}

fn changed_keys<K: Clone, V>(changes: &MapChanges<K, V>) -> impl Iterator<Item = K> + '_ {
    changes
        .changed
        .keys()
        .chain(changes.removed.keys())
        .cloned()
}

fn resolved_claims<T>(resolved: &Resolved<T>) -> imbl::OrdSet<ClaimRef> {
    match resolved {
        Resolved::Known { claims, .. } => claims.iter().cloned().collect(),
        Resolved::Conflicted { alternatives } => alternatives
            .iter()
            .flat_map(|(_, claims)| claims.iter().cloned())
            .collect(),
        Resolved::Unknown => imbl::OrdSet::new(),
    }
}

#[cfg(test)]
#[path = "provenance_support_tests.rs"]
mod tests;
