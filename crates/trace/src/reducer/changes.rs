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
    pub diagnostics: Option<ChangedValue<Vec<ReductionDiagnostic>>>,
    pub withdrawn_claims: SetChanges<ClaimRef>,
}

impl ReductionChanges {
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
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
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
            candidate_heads: vec![entry("old")],
            archive_order: vec![entry("old")],
            order_basis: BTreeMap::from([(entry("old"), ReadingOrderBasis::PresentationTiebreak)]),
            selected_entries: None,
            unresolved_entries: vec![],
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
        let before = BTreeMap::from([(1, "removed"), (3, "old"), (5, "unchanged"), (7, "last")]);
        let after = BTreeMap::from([(2, "added"), (3, "new"), (5, "unchanged"), (8, "tail")]);
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
            objects: BTreeMap::from([(key.clone(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            objects: BTreeMap::from([(key.clone(), new.clone())]),
            ..Default::default()
        };
        let changes = ReductionChanges::between(&before, &after);
        assert_replaced(&changes.objects, &key, &old, &new);
        assert_eq!(
            ReductionChanges::between(&after, &Reduction::default())
                .objects
                .removed,
            after.objects
        );
    }

    #[test]
    fn representative_remapping_preserves_both_canonical_endpoints() {
        let key = AnyObjectRef::Message(message("alias"));
        let old = AnyObjectRef::Message(message("old"));
        let new = AnyObjectRef::Message(message("new"));
        let before = Reduction {
            representatives: BTreeMap::from([(key.clone(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            representatives: BTreeMap::from([(key.clone(), new.clone())]),
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
            executions: BTreeMap::from([(key.clone(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            executions: BTreeMap::from([(key.clone(), new.clone())]),
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
            delivery_stages: BTreeMap::from([(delivery(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            delivery_stages: BTreeMap::from([(delivery(), new.clone())]),
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
            delivery_revisions: BTreeMap::from([(delivery(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            delivery_revisions: BTreeMap::from([(delivery(), new.clone())]),
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
            resource_states: BTreeMap::from([(resource(), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            resource_states: BTreeMap::from([(resource(), new.clone())]),
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
            agent_states: BTreeMap::from([(agent("root"), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            agent_states: BTreeMap::from([(agent("root"), new.clone())]),
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
            relations: vec![ResolvedRelation {
                claim: claim("edge"),
                relation: relation("old"),
            }],
            ..Default::default()
        };
        let after = Reduction {
            relations: vec![ResolvedRelation {
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
        new.archive_order.push(entry("new"));
        new.selected_entries = Some(vec![entry("new")]);
        let before = Reduction {
            histories: BTreeMap::from([(agent("root"), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            histories: BTreeMap::from([(agent("root"), new.clone())]),
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
            selection_memberships: BTreeMap::from([(selection("selected"), None)]),
            ..Default::default()
        };
        let after = Reduction {
            selection_memberships: BTreeMap::from([(selection("selected"), Some(vec![]))]),
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
            history_contents: BTreeMap::from([(entry("entry"), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            history_contents: BTreeMap::from([(entry("entry"), new.clone())]),
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
            selection_content_observations: BTreeMap::from([(selection("selected"), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            selection_content_observations: BTreeMap::from([(selection("selected"), new.clone())]),
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
            messages: BTreeMap::from([(message("message"), old.clone())]),
            ..Default::default()
        };
        let after = Reduction {
            messages: BTreeMap::from([(message("message"), new.clone())]),
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
            coverage: vec![(coverage("old"), claim("coverage"))],
            ..Default::default()
        };
        let after = Reduction {
            coverage: vec![(coverage("new"), claim("coverage"))],
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
            diagnostics: vec![old.clone()],
            ..Default::default()
        };
        let after = Reduction {
            diagnostics: vec![new.clone()],
            ..Default::default()
        };
        assert_eq!(
            ReductionChanges::between(&before, &after).diagnostics,
            Some(ChangedValue {
                before: Some(vec![old.clone()]),
                after: vec![new]
            })
        );
        assert_eq!(
            ReductionChanges::between(&before, &Reduction::default()).diagnostics,
            Some(ChangedValue {
                before: Some(vec![old]),
                after: vec![]
            })
        );
    }

    #[test]
    fn withdrawn_claim_changes_distinguish_reinstatement_from_new_withdrawal() {
        let before = Reduction {
            withdrawn_claims: vec![claim("reinstated"), claim("unchanged")],
            ..Default::default()
        };
        let after = Reduction {
            withdrawn_claims: vec![claim("newly-withdrawn"), claim("unchanged")],
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
}
