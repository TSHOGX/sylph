//! Typed differences between complete reference reductions.
//!
//! This comparison still visits the full old and new reduction and deeply compares
//! their values. It is not incremental reduction: ordered map comparison is linear
//! in visited keys plus value comparison, while indexing claim-keyed sequences and
//! collecting changed keys adds ordered-map work. Only changed values are cloned.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedValue<T> {
    pub before: Option<T>,
    pub after: T,
}

/// Added keys have no prior value; removals retain the old endpoints for invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapChanges<K, V> {
    pub changed: BTreeMap<K, ChangedValue<V>>,
    pub removed: BTreeMap<K, V>,
}

impl<K, V> Default for MapChanges<K, V> {
    fn default() -> Self {
        Self {
            changed: BTreeMap::new(),
            removed: BTreeMap::new(),
        }
    }
}

impl<K, V> MapChanges<K, V> {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

impl<K: Ord, V: PartialEq> MapChanges<K, V> {
    /// Compose consecutive state transitions without revisiting unchanged keys.
    pub fn compose(&mut self, later: Self) {
        for (key, change) in later.changed {
            let before = match self.changed.remove(&key) {
                Some(previous) => previous.before,
                None => self.removed.remove(&key).or(change.before),
            };
            if before.as_ref() != Some(&change.after) {
                self.changed.insert(
                    key,
                    ChangedValue {
                        before,
                        after: change.after,
                    },
                );
            }
        }
        for (key, value) in later.removed {
            let before = match self.changed.remove(&key) {
                Some(previous) => previous.before,
                None => Some(self.removed.remove(&key).unwrap_or(value)),
            };
            if let Some(before) = before {
                self.removed.insert(key, before);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetChanges<T> {
    pub added: BTreeSet<T>,
    pub removed: BTreeSet<T>,
}

impl<T> Default for SetChanges<T> {
    fn default() -> Self {
        Self {
            added: BTreeSet::new(),
            removed: BTreeSet::new(),
        }
    }
}

impl<T> SetChanges<T> {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl<T: Ord> SetChanges<T> {
    pub fn compose(&mut self, later: Self) {
        for value in later.added {
            if !self.removed.remove(&value) {
                self.added.insert(value);
            }
        }
        for value in later.removed {
            if !self.added.remove(&value) {
                self.removed.insert(value);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReductionChanges {
    pub objects: MapChanges<AnyObjectRef, ResolvedObject>,
    pub representatives: MapChanges<AnyObjectRef, AnyObjectRef>,
    pub executions: MapChanges<AnyObjectRef, ExecutionState>,
    pub delivery_stages: MapChanges<DeliveryRef, Vec<(DeliveryStage, ClaimRef)>>,
    pub delivery_revisions: MapChanges<DeliveryRef, BTreeMap<ClaimRef, MessageRevisionRef>>,
    pub resource_states: MapChanges<ResourceRef, Vec<(ResourceState, ClaimRef)>>,
    pub agent_states: MapChanges<AgentRef, Vec<(AgentState, ClaimRef)>>,
    pub relations: MapChanges<ClaimRef, Relation>,
    pub histories: MapChanges<AgentRef, HistoryView>,
    pub selection_memberships: MapChanges<SelectionRef, Option<Vec<EntryRef>>>,
    pub history_contents: MapChanges<EntryRef, BTreeMap<ClaimRef, HistoryContentObservation>>,
    pub selection_content_observations: MapChanges<SelectionRef, Option<BTreeSet<ClaimRef>>>,
    pub messages: MapChanges<MessageRef, MessageHistory>,
    pub coverage: MapChanges<ClaimRef, Coverage>,
    /// Derived diagnostics have no separate stable identity; preserve both complete lists.
    pub diagnostics: Option<ChangedValue<imbl::Vector<ReductionDiagnostic>>>,
    pub withdrawn_claims: SetChanges<ClaimRef>,
}

impl ReductionChanges {
    /// Combine consecutive unpublished reductions, retaining initial and final
    /// endpoints only. Work visits changed keys and compares their changed values;
    /// it never scans the complete committed or final reduction.
    pub fn compose(&mut self, later: Self) {
        let Self {
            objects,
            representatives,
            executions,
            delivery_stages,
            delivery_revisions,
            resource_states,
            agent_states,
            relations,
            histories,
            selection_memberships,
            history_contents,
            selection_content_observations,
            messages,
            coverage,
            diagnostics,
            withdrawn_claims,
        } = later;
        self.objects.compose(objects);
        self.representatives.compose(representatives);
        self.executions.compose(executions);
        self.delivery_stages.compose(delivery_stages);
        self.delivery_revisions.compose(delivery_revisions);
        self.resource_states.compose(resource_states);
        self.agent_states.compose(agent_states);
        self.relations.compose(relations);
        self.histories.compose(histories);
        self.selection_memberships.compose(selection_memberships);
        self.history_contents.compose(history_contents);
        self.selection_content_observations
            .compose(selection_content_observations);
        self.messages.compose(messages);
        self.coverage.compose(coverage);
        self.withdrawn_claims.compose(withdrawn_claims);
        if let Some(change) = diagnostics {
            let before = match self.diagnostics.take() {
                Some(previous) => previous.before,
                None => change.before,
            };
            if before.as_ref() != Some(&change.after) {
                self.diagnostics = Some(ChangedValue {
                    before,
                    after: change.after,
                });
            }
        }
    }

    pub fn between(before: &Reduction, after: &Reduction) -> Self {
        // Exhaustive destructuring makes a newly added Reduction field require a
        // deliberate change here instead of disappearing from invalidation silently.
        let Reduction {
            objects,
            representatives,
            executions,
            delivery_stages,
            delivery_revisions,
            resource_states,
            agent_states,
            relations,
            histories,
            selection_memberships,
            history_contents,
            selection_content_observations,
            messages,
            coverage,
            diagnostics,
            withdrawn_claims,
        } = after;
        // The reference reducer emits one relation/coverage fact per ClaimRef.
        // Borrow values while indexing; unchanged records need no owned copy.
        let old_relations = before
            .relations
            .iter()
            .map(|value| (&value.claim, &value.relation))
            .collect();
        let new_relations = relations
            .iter()
            .map(|value| (&value.claim, &value.relation))
            .collect();
        let old_coverage = before
            .coverage
            .iter()
            .map(|(value, claim)| (claim, value))
            .collect();
        let new_coverage = coverage
            .iter()
            .map(|(value, claim)| (claim, value))
            .collect();
        let old_withdrawn = before
            .withdrawn_claims
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let new_withdrawn = withdrawn_claims.iter().cloned().collect::<BTreeSet<_>>();
        Self {
            objects: compare_maps(&before.objects, objects),
            representatives: compare_maps(&before.representatives, representatives),
            executions: compare_maps(&before.executions, executions),
            delivery_stages: compare_maps(&before.delivery_stages, delivery_stages),
            delivery_revisions: compare_maps(&before.delivery_revisions, delivery_revisions),
            resource_states: compare_maps(&before.resource_states, resource_states),
            agent_states: compare_maps(&before.agent_states, agent_states),
            relations: compare_borrowed_maps(&old_relations, &new_relations),
            histories: compare_maps(&before.histories, histories),
            selection_memberships: compare_maps(
                &before.selection_memberships,
                selection_memberships,
            ),
            history_contents: compare_maps(&before.history_contents, history_contents),
            selection_content_observations: compare_maps(
                &before.selection_content_observations,
                selection_content_observations,
            ),
            messages: compare_maps(&before.messages, messages),
            coverage: compare_borrowed_maps(&old_coverage, &new_coverage),
            diagnostics: (before.diagnostics != *diagnostics).then(|| ChangedValue {
                before: Some(before.diagnostics.clone()),
                after: diagnostics.clone(),
            }),
            withdrawn_claims: SetChanges {
                added: new_withdrawn.difference(&old_withdrawn).cloned().collect(),
                removed: old_withdrawn.difference(&new_withdrawn).cloned().collect(),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
            && self.representatives.is_empty()
            && self.executions.is_empty()
            && self.delivery_stages.is_empty()
            && self.delivery_revisions.is_empty()
            && self.resource_states.is_empty()
            && self.agent_states.is_empty()
            && self.relations.is_empty()
            && self.histories.is_empty()
            && self.selection_memberships.is_empty()
            && self.history_contents.is_empty()
            && self.selection_content_observations.is_empty()
            && self.messages.is_empty()
            && self.coverage.is_empty()
            && self.diagnostics.is_none()
            && self.withdrawn_claims.is_empty()
    }
}

fn compare_maps<K: Ord + Clone, V: PartialEq + Clone>(
    before: &imbl::OrdMap<K, V>,
    after: &imbl::OrdMap<K, V>,
) -> MapChanges<K, V> {
    compare_entries(before.iter(), after.iter())
}

fn compare_borrowed_maps<K: Ord + Clone, V: PartialEq + Clone>(
    before: &BTreeMap<&K, &V>,
    after: &BTreeMap<&K, &V>,
) -> MapChanges<K, V> {
    compare_entries(
        before.iter().map(|(key, value)| (*key, *value)),
        after.iter().map(|(key, value)| (*key, *value)),
    )
}

fn compare_entries<'a, K: Ord + Clone + 'a, V: PartialEq + Clone + 'a>(
    mut before: impl Iterator<Item = (&'a K, &'a V)>,
    mut after: impl Iterator<Item = (&'a K, &'a V)>,
) -> MapChanges<K, V> {
    let mut changes = MapChanges::default();
    let mut old = before.next();
    let mut new = after.next();
    loop {
        match (old, new) {
            (None, None) => break,
            (Some((key, value)), None) => {
                changes.removed.insert(key.clone(), value.clone());
                old = before.next();
            }
            (None, Some((key, value))) => {
                changes.changed.insert(
                    key.clone(),
                    ChangedValue {
                        before: None,
                        after: value.clone(),
                    },
                );
                new = after.next();
            }
            (Some((old_key, old_value)), Some((new_key, new_value))) => {
                match old_key.cmp(new_key) {
                    std::cmp::Ordering::Less => {
                        changes.removed.insert(old_key.clone(), old_value.clone());
                        old = before.next();
                    }
                    std::cmp::Ordering::Greater => {
                        changes.changed.insert(
                            new_key.clone(),
                            ChangedValue {
                                before: None,
                                after: new_value.clone(),
                            },
                        );
                        new = after.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if old_value != new_value {
                            changes.changed.insert(
                                new_key.clone(),
                                ChangedValue {
                                    before: Some(old_value.clone()),
                                    after: new_value.clone(),
                                },
                            );
                        }
                        old = before.next();
                        new = after.next();
                    }
                }
            }
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentRef {
        AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env:codex:{id}")).unwrap())
    }
    fn claim(id: &str) -> ClaimRef {
        ClaimRef::native(agent("root"), "claims", vec![id.into()]).unwrap()
    }
    fn entry(id: &str) -> EntryRef {
        EntryRef::native(agent("root"), "entries", vec![id.into()]).unwrap()
    }
    fn selection(id: &str) -> SelectionRef {
        SelectionRef::native(agent("root"), "selections", vec![id.into()]).unwrap()
    }
    fn message(id: &str) -> MessageRef {
        MessageRef::native(agent("root"), "messages", vec![id.into()]).unwrap()
    }
    fn revision(id: &str) -> MessageRevisionRef {
        MessageRevisionRef::native(agent("root"), "revisions", vec![id.into()]).unwrap()
    }
    fn delivery() -> DeliveryRef {
        DeliveryRef::native(agent("root"), "deliveries", vec!["delivery".into()]).unwrap()
    }
    fn resource() -> ResourceRef {
        ResourceRef::native(agent("root"), "resources", vec!["resource".into()]).unwrap()
    }
    fn position() -> SourcePosition {
        SourcePosition {
            source: SourceRef {
                issuer: Issuer::Server("fixture".into()),
                instance: "source".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: 1 },
        }
    }
    fn history() -> HistoryView {
        HistoryView {
            agent: agent("root"),
            selection: Resolved::Unknown,
            candidate_heads: imbl::vector![entry("old")],
            archive_order: imbl::vector![entry("old")],
            order_basis: BTreeMap::from([(entry("old"), ReadingOrderBasis::PresentationTiebreak)])
                .into(),
            selected_entries: None,
            unresolved_entries: imbl::Vector::new(),
        }
    }
    fn message_history() -> MessageHistory {
        MessageHistory {
            revisions: vec![revision("old")],
            selected: Resolved::Unknown,
            sealed: false,
            late_segments: vec![],
            conflicted_revisions: vec![],
            candidates: vec![],
        }
    }
    fn assert_replaced<K: Ord + Clone + std::fmt::Debug, V: PartialEq + Clone + std::fmt::Debug>(
        changes: &MapChanges<K, V>,
        key: &K,
        before: &V,
        after: &V,
    ) {
        assert_eq!(changes.changed.len(), 1);
        assert!(changes.removed.is_empty());
        assert_eq!(
            changes.changed[key],
            ChangedValue {
                before: Some(before.clone()),
                after: after.clone()
            }
        );
    }

    #[test]
    fn unchanged_reduction_and_unchanged_keys_produce_no_invalidation() {
        let mut before = Reduction::default();
        before.histories.insert(agent("root"), history());
        before
            .messages
            .insert(message("message"), message_history());
        assert!(ReductionChanges::between(&before, &before.clone()).is_empty());
        assert!(ReductionChanges::between(&Reduction::default(), &Reduction::default()).is_empty());
    }

    #[test]
    fn map_additions_replacements_and_removals_keep_their_old_and_new_values() {
        let before: imbl::OrdMap<_, _> =
            BTreeMap::from([(1, "removed"), (3, "old"), (5, "unchanged"), (7, "last")]).into();
        let after: imbl::OrdMap<_, _> =
            BTreeMap::from([(2, "added"), (3, "new"), (5, "unchanged"), (8, "tail")]).into();
        let changed = compare_maps(&before, &after);
        assert_eq!(
            changed.changed,
            BTreeMap::from([
                (
                    2,
                    ChangedValue {
                        before: None,
                        after: "added"
                    }
                ),
                (
                    3,
                    ChangedValue {
                        before: Some("old"),
                        after: "new"
                    }
                ),
                (
                    8,
                    ChangedValue {
                        before: None,
                        after: "tail"
                    }
                ),
            ])
        );
        assert_eq!(
            changed.removed,
            BTreeMap::from([(1, "removed"), (7, "last")])
        );
    }

    #[test]
    fn declaration_changes_and_removals_preserve_provenance_and_old_identity() {
        let key = AnyObjectRef::Agent(agent("root"));
        let object = TraceObject::Agent(AgentThread {
            agent_ref: agent("root"),
            vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
            name: None,
            model: None,
        });
        let old = ResolvedObject {
            reference: key.clone(),
            declaration: Resolved::Known {
                value: object.clone(),
                claims: vec![claim("old-evidence")],
            },
        };
        let new = ResolvedObject {
            reference: key.clone(),
            declaration: Resolved::Known {
                value: object,
                claims: vec![claim("new-evidence")],
            },
        };
        let before = Reduction {
            objects: BTreeMap::from([(key.clone(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            objects: BTreeMap::from([(key.clone(), new.clone())]).into(),
            ..Default::default()
        };
        let changes = ReductionChanges::between(&before, &after);
        assert_replaced(&changes.objects, &key, &old, &new);
        assert_eq!(
            ReductionChanges::between(&after, &Reduction::default())
                .objects
                .removed,
            after.objects.into_iter().collect()
        );
    }

    #[test]
    fn representative_remapping_preserves_both_canonical_endpoints() {
        let key = AnyObjectRef::Message(message("alias"));
        let old = AnyObjectRef::Message(message("old"));
        let new = AnyObjectRef::Message(message("new"));
        let before = Reduction {
            representatives: BTreeMap::from([(key.clone(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            representatives: BTreeMap::from([(key.clone(), new.clone())]).into(),
            ..Default::default()
        };
        let changes = ReductionChanges::between(&before, &after);
        assert_replaced(&changes.representatives, &key, &old, &new);
        assert!(changes.objects.is_empty());
    }

    #[test]
    fn execution_outcome_changes_do_not_require_a_new_declaration() {
        let key = AnyObjectRef::Activation(
            ActivationRef::native(agent("root"), "turn", vec!["one".into()]).unwrap(),
        );
        let old = ExecutionState {
            started: vec![claim("start")],
            terminated: vec![],
            outcome: Resolved::Unknown,
            interrupt_requests: vec![],
            boundaries: vec![],
        };
        let mut new = old.clone();
        new.outcome = Resolved::Known {
            value: TerminalOutcome::Failed,
            claims: vec![claim("failed")],
        };
        let before = Reduction {
            executions: BTreeMap::from([(key.clone(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            executions: BTreeMap::from([(key.clone(), new.clone())]).into(),
            ..Default::default()
        };
        let changes = ReductionChanges::between(&before, &after);
        assert_replaced(&changes.executions, &key, &old, &new);
        assert!(changes.objects.is_empty());
    }

    #[test]
    fn delivery_stage_changes_keep_old_and_new_admission_entries() {
        let old = vec![(
            DeliveryStage::Admitted {
                entry: Some(entry("old")),
            },
            claim("delivery"),
        )];
        let new = vec![(
            DeliveryStage::Admitted {
                entry: Some(entry("new")),
            },
            claim("delivery"),
        )];
        let before = Reduction {
            delivery_stages: BTreeMap::from([(delivery(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            delivery_stages: BTreeMap::from([(delivery(), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).delivery_stages,
            &delivery(),
            &old,
            &new,
        );
    }

    #[test]
    fn delivery_revision_bindings_preserve_changed_and_removed_body_endpoints() {
        let old = BTreeMap::from([(claim("binding"), revision("old"))]);
        let new = BTreeMap::from([(claim("binding"), revision("new"))]);
        let before = Reduction {
            delivery_revisions: BTreeMap::from([(delivery(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            delivery_revisions: BTreeMap::from([(delivery(), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).delivery_revisions,
            &delivery(),
            &old,
            &new,
        );
        assert_eq!(
            ReductionChanges::between(&before, &Reduction::default())
                .delivery_revisions
                .removed[&delivery()],
            old
        );
    }

    #[test]
    fn resource_lifetime_changes_are_independent_of_execution_declarations() {
        let old = vec![(ResourceState::Running, claim("resource"))];
        let new = vec![(
            ResourceState::Closed {
                outcome: TerminalOutcome::Succeeded,
                reason: None,
            },
            claim("resource"),
        )];
        let before = Reduction {
            resource_states: BTreeMap::from([(resource(), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            resource_states: BTreeMap::from([(resource(), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).resource_states,
            &resource(),
            &old,
            &new,
        );
    }

    #[test]
    fn agent_observation_state_changes_remain_visible_without_object_changes() {
        let old = vec![(AgentState::Exists, claim("agent"))];
        let new = vec![(
            AgentState::VendorClosed {
                reason: Some("closed".into()),
            },
            claim("agent"),
        )];
        let before = Reduction {
            agent_states: BTreeMap::from([(agent("root"), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            agent_states: BTreeMap::from([(agent("root"), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).agent_states,
            &agent("root"),
            &old,
            &new,
        );
    }

    #[test]
    fn relation_retargeting_and_removal_keep_the_previous_dependency_endpoints() {
        let relation = |child| Relation::Spawn {
            parent: agent("root"),
            child: agent(child),
            operation: None,
        };
        let before = Reduction {
            relations: imbl::vector![ResolvedRelation {
                claim: claim("edge"),
                relation: relation("old"),
            }],
            ..Default::default()
        };
        let after = Reduction {
            relations: imbl::vector![ResolvedRelation {
                claim: claim("edge"),
                relation: relation("new"),
            }],
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).relations,
            &claim("edge"),
            &relation("old"),
            &relation("new"),
        );
        assert_eq!(
            ReductionChanges::between(&before, &Reduction::default())
                .relations
                .removed[&claim("edge")],
            relation("old")
        );
    }

    #[test]
    fn history_order_and_selection_changes_keep_the_previous_membership() {
        let old = history();
        let mut new = old.clone();
        new.archive_order.push_back(entry("new"));
        new.selected_entries = Some(imbl::vector![entry("new")]);
        let before = Reduction {
            histories: BTreeMap::from([(agent("root"), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            histories: BTreeMap::from([(agent("root"), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).histories,
            &agent("root"),
            &old,
            &new,
        );
    }

    #[test]
    fn selected_membership_distinguishes_unknown_from_known_empty() {
        let before = Reduction {
            selection_memberships: BTreeMap::from([(selection("selected"), None)]).into(),
            ..Default::default()
        };
        let after = Reduction {
            selection_memberships: BTreeMap::from([(selection("selected"), Some(Vec::new()))])
                .into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).selection_memberships,
            &selection("selected"),
            &None,
            &Some(vec![]),
        );
    }

    #[test]
    fn entry_content_changes_keep_prior_revision_slots_and_completeness() {
        let observation = |revision_id| {
            HistoryContentObservation::complete(
                entry("entry"),
                position(),
                "fixture/history",
                vec![HistoryContent::Message(revision(revision_id))],
            )
        };
        let old = BTreeMap::from([(claim("content"), observation("old"))]);
        let mut new = BTreeMap::from([(claim("content"), observation("new"))]);
        new.get_mut(&claim("content")).unwrap().complete = false;
        let before = Reduction {
            history_contents: BTreeMap::from([(entry("entry"), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            history_contents: BTreeMap::from([(entry("entry"), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).history_contents,
            &entry("entry"),
            &old,
            &new,
        );
    }

    #[test]
    fn selection_content_bindings_are_not_inferred_from_unchanged_entry_membership() {
        let old = Some(BTreeSet::from([claim("old")]));
        let new = Some(BTreeSet::from([claim("new")]));
        let before = Reduction {
            selection_content_observations: BTreeMap::from([(selection("selected"), old.clone())])
                .into(),
            ..Default::default()
        };
        let after = Reduction {
            selection_content_observations: BTreeMap::from([(selection("selected"), new.clone())])
                .into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).selection_content_observations,
            &selection("selected"),
            &old,
            &new,
        );
    }

    #[test]
    fn message_revision_selection_and_late_segments_change_without_message_declaration() {
        let old = message_history();
        let mut new = old.clone();
        new.selected = Resolved::Known {
            value: revision("new"),
            claims: vec![claim("final")],
        };
        new.late_segments.push(revision("late"));
        new.sealed = true;
        let before = Reduction {
            messages: BTreeMap::from([(message("message"), old.clone())]).into(),
            ..Default::default()
        };
        let after = Reduction {
            messages: BTreeMap::from([(message("message"), new.clone())]).into(),
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).messages,
            &message("message"),
            &old,
            &new,
        );
    }

    #[test]
    fn coverage_changes_preserve_dimension_and_old_new_subjects() {
        let coverage = |id| Coverage {
            subject: AnyObjectRef::Agent(agent(id)),
            source: position().source,
            dimension: CoverageDimension::Timing,
            interval: CoverageInterval::Object {
                object: AnyObjectRef::Agent(agent(id)),
            },
            state: CoverageState::Complete,
        };
        let before = Reduction {
            coverage: imbl::vector![(coverage("old"), claim("coverage"))],
            ..Default::default()
        };
        let after = Reduction {
            coverage: imbl::vector![(coverage("new"), claim("coverage"))],
            ..Default::default()
        };
        assert_replaced(
            &ReductionChanges::between(&before, &after).coverage,
            &claim("coverage"),
            &coverage("old"),
            &coverage("new"),
        );
        assert_eq!(
            ReductionChanges::between(&before, &Reduction::default())
                .coverage
                .removed[&claim("coverage")],
            coverage("old")
        );
    }

    #[test]
    fn diagnostic_changes_keep_removed_claim_evidence_and_messages() {
        let old = ReductionDiagnostic {
            code: DiagnosticCode::MissingSource,
            claims: vec![claim("old")],
            message: "prior diagnostic".into(),
        };
        let new = ReductionDiagnostic {
            code: DiagnosticCode::ConflictingOwner,
            claims: vec![claim("new")],
            message: "new diagnostic".into(),
        };
        let before = Reduction {
            diagnostics: imbl::vector![old.clone()],
            ..Default::default()
        };
        let after = Reduction {
            diagnostics: imbl::vector![new.clone()],
            ..Default::default()
        };
        assert_eq!(
            ReductionChanges::between(&before, &after).diagnostics,
            Some(ChangedValue {
                before: Some(imbl::vector![old.clone()]),
                after: imbl::vector![new]
            })
        );
        assert_eq!(
            ReductionChanges::between(&before, &Reduction::default()).diagnostics,
            Some(ChangedValue {
                before: Some(imbl::vector![old]),
                after: imbl::Vector::new()
            })
        );
    }

    #[test]
    fn withdrawn_claim_changes_distinguish_reinstatement_from_new_withdrawal() {
        let before = Reduction {
            withdrawn_claims: imbl::vector![claim("reinstated"), claim("unchanged")],
            ..Default::default()
        };
        let after = Reduction {
            withdrawn_claims: imbl::vector![claim("newly-withdrawn"), claim("unchanged")],
            ..Default::default()
        };
        assert_eq!(
            ReductionChanges::between(&before, &after).withdrawn_claims,
            SetChanges {
                added: BTreeSet::from([claim("newly-withdrawn")]),
                removed: BTreeSet::from([claim("reinstated")]),
            }
        );
    }

    fn composition_state(label: &str) -> Reduction {
        let mut state = Reduction::default();
        let object = AnyObjectRef::Agent(agent("root"));
        state.objects.insert(
            object.clone(),
            ResolvedObject {
                reference: object.clone(),
                declaration: Resolved::Known {
                    value: TraceObject::Agent(AgentThread {
                        agent_ref: agent("root"),
                        vendor: sylph_protocol::AgentName::parse("codex").unwrap(),
                        name: Some(label.into()),
                        model: None,
                    }),
                    claims: vec![claim(label)],
                },
            },
        );
        state.representatives.insert(
            AnyObjectRef::Agent(agent("alias")),
            AnyObjectRef::Agent(agent(label)),
        );
        state.executions.insert(
            object.clone(),
            ExecutionState {
                started: vec![claim(label)],
                terminated: vec![],
                outcome: Resolved::Unknown,
                interrupt_requests: vec![],
                boundaries: vec![],
            },
        );
        state.delivery_stages.insert(
            delivery(),
            vec![(
                DeliveryStage::Admitted {
                    entry: Some(entry(label)),
                },
                claim("admitted"),
            )],
        );
        state.delivery_revisions.insert(
            delivery(),
            BTreeMap::from([(claim("binding"), revision(label))]),
        );
        state.resource_states.insert(
            resource(),
            vec![(
                ResourceState::GoalUpdated {
                    status: label.into(),
                    detail: None,
                },
                claim("resource"),
            )],
        );
        state.agent_states.insert(
            agent("root"),
            vec![(
                AgentState::VendorClosed {
                    reason: Some(label.into()),
                },
                claim("agent"),
            )],
        );
        state.relations.push_back(ResolvedRelation {
            claim: claim("relation"),
            relation: Relation::Spawn {
                parent: agent("root"),
                child: agent(label),
                operation: None,
            },
        });
        let mut view = history();
        view.archive_order = imbl::vector![entry(label)];
        view.candidate_heads = imbl::vector![entry(label)];
        view.order_basis = [(entry(label), ReadingOrderBasis::NativePredecessor)]
            .into_iter()
            .collect();
        state.histories.insert(agent("root"), view);
        state
            .selection_memberships
            .insert(selection("selected"), Some(vec![entry(label)]));
        state.history_contents.insert(
            entry("entry"),
            BTreeMap::from([(
                claim("content"),
                HistoryContentObservation::complete(
                    entry("entry"),
                    position(),
                    "fixture/history",
                    vec![HistoryContent::Message(revision(label))],
                ),
            )]),
        );
        state
            .selection_content_observations
            .insert(selection("selected"), Some(BTreeSet::from([claim(label)])));
        let mut history = message_history();
        history.revisions = vec![revision(label)];
        state.messages.insert(message("message"), history);
        state.coverage.push_back((
            Coverage {
                subject: AnyObjectRef::Agent(agent(label)),
                source: position().source,
                dimension: CoverageDimension::Timing,
                interval: CoverageInterval::Object {
                    object: AnyObjectRef::Agent(agent(label)),
                },
                state: CoverageState::Complete,
            },
            claim("coverage"),
        ));
        state.diagnostics.push_back(ReductionDiagnostic {
            code: DiagnosticCode::MissingSource,
            claims: vec![claim(label)],
            message: label.into(),
        });
        state.withdrawn_claims.push_back(claim(label));
        state
    }

    #[test]
    fn composed_map_transitions_keep_initial_values_and_cancel_restored_keys() {
        let before = imbl::ordmap! {
            "changed-removed" => "original", "removed-restored" => "original",
            "removed-replaced" => "original", "changed-restored" => "original",
            "changed-again" => "original", "unchanged" => "keep"
        };
        let middle = imbl::ordmap! {
            "added-removed" => "temporary", "added-updated" => "temporary",
            "changed-removed" => "middle", "changed-restored" => "middle",
            "changed-again" => "middle", "unchanged" => "keep"
        };
        let after = imbl::ordmap! {
            "added-updated" => "final", "removed-restored" => "original",
            "removed-replaced" => "final", "changed-restored" => "original",
            "changed-again" => "final", "unchanged" => "keep", "later-added" => "last"
        };
        let mut changes = compare_maps(&before, &middle);
        changes.compose(compare_maps(&middle, &after));
        assert_eq!(changes, compare_maps(&before, &after));
        assert_eq!(
            changes.removed,
            BTreeMap::from([("changed-removed", "original")])
        );
        assert_eq!(changes.changed["added-updated"].before, None);
        assert_eq!(changes.changed["changed-again"].before, Some("original"));
        for key in [
            "added-removed",
            "removed-restored",
            "changed-restored",
            "unchanged",
        ] {
            assert!(!changes.changed.contains_key(key));
            assert!(!changes.removed.contains_key(key));
        }
    }

    #[test]
    fn composed_dependency_passes_keep_original_object_history_and_delivery_endpoints() {
        let before = composition_state("original");
        let middle = composition_state("intermediate");
        let mut after = composition_state("final");
        after.objects.clear();
        after.histories.clear();
        let mut changes = ReductionChanges::between(&before, &middle);
        changes.compose(ReductionChanges::between(&middle, &after));
        assert_eq!(changes, ReductionChanges::between(&before, &after));
        assert_eq!(
            changes.objects.removed[&AnyObjectRef::Agent(agent("root"))],
            before.objects[&AnyObjectRef::Agent(agent("root"))]
        );
        assert_eq!(
            changes.histories.removed[&agent("root")],
            before.histories[&agent("root")]
        );
        assert_replaced(
            &changes.delivery_stages,
            &delivery(),
            &before.delivery_stages[&delivery()],
            &after.delivery_stages[&delivery()],
        );
        assert_replaced(
            &changes.delivery_revisions,
            &delivery(),
            &before.delivery_revisions[&delivery()],
            &after.delivery_revisions[&delivery()],
        );
    }

    #[test]
    fn composing_added_then_removed_state_cancels_every_non_object_family() {
        let empty = Reduction::default();
        let transient = composition_state("transient");
        let mut changes = ReductionChanges::between(&empty, &transient);
        changes.compose(ReductionChanges::between(&transient, &empty));
        assert_eq!(changes, ReductionChanges::default());
        assert!(changes.is_empty());
    }

    #[test]
    fn composing_removal_then_restoration_and_intermediate_changes_cancels_net_noops() {
        let original = composition_state("original");
        let empty = Reduction::default();
        let intermediate = composition_state("intermediate");
        let mut changes = ReductionChanges::between(&original, &empty);
        changes.compose(ReductionChanges::between(&empty, &intermediate));
        changes.compose(ReductionChanges::between(&intermediate, &original));
        assert_eq!(changes, ReductionChanges::default());
        assert!(changes.is_empty());
    }
}
