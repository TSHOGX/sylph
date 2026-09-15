//! One validated native-history graph serves direct and inherited membership reads.
//!
//! Source extents require a separate complete-mapping witness. A missing source and
//! a fully interpreted header-only interval both contain zero HistoryPosition facts;
//! that cardinality cannot decide whether selected membership is known.

use super::*;

type SelectionObservationGroup<'a> = (&'a SourcePosition, Vec<(&'a SelectionRef, &'a ClaimRef)>);

struct ReadingView {
    order: Vec<EntryRef>,
    heads: Vec<EntryRef>,
    unresolved: Vec<EntryRef>,
    basis: BTreeMap<EntryRef, ReadingOrderBasis>,
}

struct HistoryGraph {
    entries: BTreeMap<EntryRef, HistoryEntry>,
    predecessor: BTreeMap<EntryRef, EntryRef>,
    invalid: BTreeSet<EntryRef>,
    claims: BTreeMap<EntryRef, Vec<ClaimRef>>,
}

impl HistoryGraph {
    fn build(active: &[&Claim], result: &mut Reduction) -> Self {
        let mut graph = Self {
            entries: BTreeMap::new(),
            predecessor: BTreeMap::new(),
            invalid: BTreeSet::new(),
            claims: BTreeMap::new(),
        };
        for object in result.objects.values() {
            if let Resolved::Known {
                value: TraceObject::HistoryEntry(entry),
                claims,
            } = &object.declaration
            {
                graph.entries.insert(entry.entry_ref.clone(), entry.clone());
                graph.claims.insert(entry.entry_ref.clone(), claims.clone());
            }
        }
        let mut candidates: BTreeMap<EntryRef, BTreeMap<EntryRef, Vec<ClaimRef>>> = BTreeMap::new();
        for entry in graph.entries.values() {
            if let Some(parent) = &entry.predecessor {
                candidates
                    .entry(entry.entry_ref.clone())
                    .or_default()
                    .entry(parent.clone())
                    .or_default()
                    .extend(graph.claims[&entry.entry_ref].clone());
            }
        }
        // Native predecessors have their own validity graph. Read-order relations
        // must neither hide a conflicting native parent nor establish membership.
        for claim in active {
            if let Fact::Relation {
                relation: Relation::HistoryPredecessor { predecessor, entry },
            } = &claim.fact
            {
                candidates
                    .entry(entry.clone())
                    .or_default()
                    .entry(predecessor.clone())
                    .or_default()
                    .push(claim.claim_ref.clone());
            }
        }
        let mut edges = Vec::new();
        for (entry, parents) in candidates {
            let claims: Vec<_> = parents.values().flatten().cloned().collect();
            if parents.len() != 1 {
                graph.invalid.insert(entry);
                diagnostic(
                    result,
                    DiagnosticCode::InvalidRelation,
                    claims,
                    "History entry has conflicting native predecessors",
                );
                continue;
            }
            let parent = parents.into_keys().next().unwrap();
            if graph
                .entries
                .get(&entry)
                .zip(graph.entries.get(&parent))
                .is_some_and(|(child, parent)| child.containing_agent != parent.containing_agent)
            {
                graph.invalid.insert(entry);
                diagnostic(
                    result,
                    DiagnosticCode::InvalidRelation,
                    claims,
                    "Native predecessor crosses containing-agent histories",
                );
                continue;
            }
            for claim in claims {
                edges.push((parent.clone(), entry.clone(), claim));
            }
            graph.predecessor.insert(entry, parent);
        }
        let cycles = cyclic_edge_claims(&edges);
        for (_, entry, claim) in edges {
            if cycles.contains(&claim) {
                graph.invalid.insert(entry);
            }
        }
        if !cycles.is_empty() {
            diagnostic(
                result,
                DiagnosticCode::CyclicHistory,
                cycles.into_iter().collect(),
                "Cyclic native predecessors remain unavailable in every selected path",
            );
        }
        graph
    }

    fn path(&self, agent: &AgentRef, head: &EntryRef) -> Option<Vec<EntryRef>> {
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        let mut cursor = Some(head.clone());
        while let Some(reference) = cursor {
            let entry = self.entries.get(&reference)?;
            if &entry.containing_agent != agent
                || self.invalid.contains(&reference)
                || !seen.insert(reference.clone())
            {
                return None;
            }
            path.push(reference.clone());
            cursor = self.predecessor.get(&reference).cloned();
        }
        path.reverse();
        Some(path)
    }

    fn reading_order(
        &self,
        agent: &AgentRef,
        relations: &[ResolvedRelation],
        result: &mut Reduction,
    ) -> ReadingView {
        let owned: BTreeSet<_> = self
            .entries
            .values()
            .filter(|entry| &entry.containing_agent == agent)
            .map(|entry| entry.entry_ref.clone())
            .collect();
        let mut children: BTreeMap<EntryRef, BTreeSet<EntryRef>> = BTreeMap::new();
        let mut incoming_basis = BTreeMap::new();
        let mut unresolved: BTreeSet<_> = self.invalid.intersection(&owned).cloned().collect();
        for (entry, parent) in &self.predecessor {
            if !owned.contains(entry) {
                continue;
            }
            if !owned.contains(parent) {
                unresolved.insert(parent.clone());
                continue;
            }
            if !self.invalid.contains(entry) {
                incoming_basis.insert(entry.clone(), ReadingOrderBasis::NativePredecessor);
                children
                    .entry(parent.clone())
                    .or_default()
                    .insert(entry.clone());
            }
        }
        let heads = owned
            .iter()
            .filter(|entry| !children.contains_key(*entry))
            .cloned()
            .collect();
        for relation in relations {
            if let Relation::ReadingOrder { before, after } = &relation.relation {
                if owned.contains(before) && owned.contains(after) {
                    incoming_basis
                        .entry(after.clone())
                        .or_insert(ReadingOrderBasis::ExplicitOrder);
                    children
                        .entry(before.clone())
                        .or_default()
                        .insert(after.clone());
                }
            }
        }
        let mut indegree: BTreeMap<_, usize> =
            owned.iter().map(|entry| (entry.clone(), 0)).collect();
        for descendants in children.values() {
            for child in descendants {
                *indegree.get_mut(child).unwrap() += 1;
            }
        }
        let mut ready: BTreeSet<_> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(entry, _)| entry.clone())
            .collect();
        let mut order = Vec::new();
        let mut basis = BTreeMap::new();
        let mut tied = BTreeSet::new();
        while !ready.is_empty() {
            // Every simultaneously eligible entry participated in the arbitrary
            // ordering choice, including the one popped after the set shrinks.
            if ready.len() > 1 {
                tied.extend(ready.iter().cloned());
            }
            let entry = ready.pop_first().unwrap();
            basis.insert(
                entry.clone(),
                if tied.contains(&entry) {
                    ReadingOrderBasis::PresentationTiebreak
                } else {
                    incoming_basis
                        .get(&entry)
                        .cloned()
                        .unwrap_or(ReadingOrderBasis::PresentationTiebreak)
                },
            );
            order.push(entry.clone());
            if let Some(descendants) = children.get(&entry) {
                for child in descendants {
                    let degree = indegree.get_mut(child).unwrap();
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(child.clone());
                    }
                }
            }
        }
        if order.len() != owned.len() {
            diagnostic(
                result,
                DiagnosticCode::CyclicHistory,
                vec![],
                "Contradictory reader order uses deterministic archive presentation",
            );
            let ordered: BTreeSet<_> = order.iter().cloned().collect();
            for entry in owned.difference(&ordered) {
                basis.insert(entry.clone(), ReadingOrderBasis::PresentationTiebreak);
            }
            order.extend(owned.difference(&ordered).cloned());
        }
        ReadingView {
            order,
            heads,
            unresolved: unresolved.into_iter().collect(),
            basis,
        }
    }
}

pub(super) fn resolve_histories(active: &[&Claim], result: &mut Reduction) {
    let graph = HistoryGraph::build(active, result);
    let selections: BTreeMap<_, _> = result
        .objects
        .values()
        .filter_map(|object| match object.declaration.known() {
            Some(TraceObject::HistorySelection(selection)) => {
                Some((selection.selection_ref.clone(), selection.clone()))
            }
            _ => None,
        })
        .collect();
    let agents: BTreeSet<_> = graph
        .entries
        .values()
        .map(|entry| entry.containing_agent.clone())
        .chain(selections.values().map(|selection| selection.agent.clone()))
        .chain(active.iter().filter_map(|claim| match &claim.fact {
            Fact::SelectHistory { agent, .. } => Some(agent.clone()),
            _ => None,
        }))
        .collect();
    let relations = result.relations.clone();
    let mut read_views = BTreeMap::new();
    for agent in &agents {
        read_views.insert(
            agent.clone(),
            graph.reading_order(agent, &relations, result),
        );
    }
    let entry_order: BTreeMap<_, _> = read_views
        .values()
        .flat_map(|view| view.order.iter().cloned())
        .enumerate()
        .map(|(index, entry)| (entry, index))
        .collect();
    for agent in agents {
        let ReadingView {
            order: archive_order,
            heads: candidate_heads,
            unresolved: unresolved_entries,
            basis: order_basis,
        } = read_views.remove(&agent).unwrap();
        let mut latest: BTreeMap<(SourceRef, u64), SelectionObservationGroup<'_>> = BTreeMap::new();
        for claim in active {
            if let Fact::SelectHistory {
                agent: selected_agent,
                selection,
                selected_at,
            } = &claim.fact
            {
                if selected_agent != &agent {
                    continue;
                }
                let prior = latest
                    .entry((selected_at.source.clone(), selected_at.generation))
                    .or_insert((selected_at, vec![]));
                match source_order(selected_at, prior.0) {
                    Some(std::cmp::Ordering::Greater) => {
                        *prior = (selected_at, vec![(selection, &claim.claim_ref)])
                    }
                    Some(std::cmp::Ordering::Equal) | None => {
                        prior.1.push((selection, &claim.claim_ref))
                    }
                    Some(std::cmp::Ordering::Less) => {}
                }
            }
        }
        let selected = resolve_values(latest.values().flat_map(|(_, values)| {
            values
                .iter()
                .map(|(selection, claim)| ((*selection).clone(), (*claim).clone()))
        }));
        let selected_entries = selected
            .known()
            .and_then(|reference| selections.get(reference))
            .filter(|selection| selection.agent == agent)
            .and_then(|selection| {
                membership(
                    selection,
                    &graph,
                    &selections,
                    active,
                    &entry_order,
                    &mut BTreeSet::new(),
                )
            });
        if selected.known().is_some() && selected_entries.is_none() {
            diagnostic(result,DiagnosticCode::UnknownSelection,latest.values().flat_map(|(_,values)|values.iter().map(|(_,claim)|(*claim).clone())).collect(),"Selected history awaits complete source mapping or unambiguous predecessor evidence");
        }
        result.histories.insert(
            agent.clone(),
            HistoryView {
                agent,
                selection: selected,
                candidate_heads,
                archive_order,
                order_basis,
                selected_entries,
                unresolved_entries,
            },
        );
    }
}

fn membership(
    selection: &HistorySelection,
    graph: &HistoryGraph,
    selections: &BTreeMap<SelectionRef, HistorySelection>,
    active: &[&Claim],
    entry_order: &BTreeMap<EntryRef, usize>,
    visiting: &mut BTreeSet<SelectionRef>,
) -> Option<Vec<EntryRef>> {
    if !visiting.insert(selection.selection_ref.clone()) {
        return None;
    }
    let mut members = Vec::new();
    match &selection.membership {
        SelectionMembership::EntryPath { head } => members = graph.path(&selection.agent, head)?,
        SelectionMembership::EntrySet { entries } => {
            for entry in entries {
                graph.entries.get(entry)?;
                members.push(entry.clone());
            }
            members.sort_by_key(|entry| entry_order.get(entry).copied());
        }
        SelectionMembership::SourceExtents {
            extents,
            inherited_selections,
        } => {
            for inherited in inherited_selections {
                members.extend(membership(
                    selections.get(inherited)?,
                    graph,
                    selections,
                    active,
                    entry_order,
                    visiting,
                )?);
            }
            for extent in extents {
                if !complete_mapping(extent, active) {
                    return None;
                }
                // First gather every binding in this coordinate domain, before
                // filtering the selected range. Otherwise one conflicting value
                // outside the range would disappear and bless the convenient one.
                let mut bindings: BTreeMap<EntryRef, BTreeSet<u64>> = BTreeMap::new();
                for claim in active {
                    if let Fact::HistoryPosition {
                        entry,
                        position,
                        coordinate,
                        value,
                        mapping_rule,
                    } = &claim.fact
                    {
                        if position.source == extent.source
                            && position.generation == extent.generation
                            && coordinate == &extent.coordinate
                            && mapping_rule == &extent.mapping_rule
                        {
                            bindings.entry(entry.clone()).or_default().insert(*value);
                        }
                    }
                }
                let mut positions = BTreeMap::new();
                for (entry, values) in bindings {
                    if !values.iter().any(|value| {
                        *value >= extent.start_inclusive && *value < extent.end_exclusive
                    }) {
                        continue;
                    }
                    if values.len() != 1 {
                        return None;
                    }
                    let declaration = graph.entries.get(&entry)?;
                    if let Some(original) = &extent.original_owner {
                        let recorded =
                            declaration
                                .original_owner
                                .clone()
                                .unwrap_or_else(|| OwnerRef::Agent {
                                    agent: declaration.containing_agent.clone(),
                                });
                        if &recorded != original {
                            return None;
                        }
                    }
                    positions.insert((*values.first().unwrap(), entry.clone()), entry);
                }
                members.extend(positions.into_values());
            }
        }
    }
    visiting.remove(&selection.selection_ref);
    let mut unique = BTreeSet::new();
    members.retain(|entry| unique.insert(entry.clone()));
    Some(members)
}

fn complete_mapping(extent: &SourceExtent, active: &[&Claim]) -> bool {
    if extent.start_inclusive == extent.end_exclusive {
        return true;
    }
    let mut intervals = Vec::new();
    for claim in active {
        if let Fact::HistoryMappingComplete { extent: witness } = &claim.fact {
            if witness.source == extent.source
                && witness.generation == extent.generation
                && witness.coordinate == extent.coordinate
                && witness.mapping_rule == extent.mapping_rule
            {
                intervals.push((witness.start_inclusive, witness.end_exclusive));
            }
        }
    }
    intervals.sort();
    let mut through = extent.start_inclusive;
    for (start, end) in intervals {
        if end <= through {
            continue;
        }
        if start > through {
            return false;
        }
        through = end;
        if through >= extent.end_exclusive {
            return true;
        }
    }
    false
}
