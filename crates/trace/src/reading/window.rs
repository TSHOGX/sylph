//! Per-scope resolved history content for window rendering.

use super::*;
use crate::reducer::ResolvedEntryContents;

/// Resolved source bindings shared by each slot in a history window.
/// A bounded renderer may supply only its entry contents, while included revisions
/// must still cover the selected scope used to choose message presentation.
#[derive(Debug, Clone)]
pub(super) struct WindowBindings {
    pub(super) contents: BTreeMap<EntryRef, ResolvedEntryContents>,
    pub(super) included_revisions: BTreeSet<MessageRevisionRef>,
}

impl WindowBindings {
    pub(super) fn new<'a, I>(index: &Index<'_>, scope: &ReadingScope, entries: I) -> Self
    where
        I: Iterator<Item = &'a EntryRef> + Clone,
    {
        let contents = entries
            .map(|entry| {
                (
                    entry.clone(),
                    index
                        .reduction
                        .entry_contents(entry, scope.selection.as_ref()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let included_revisions = contents
            .values()
            .flat_map(|contents| contents.slots.values())
            .filter_map(Resolved::known)
            .flatten()
            .filter_map(|content| match content {
                HistoryContent::Message(revision) => Some(revision.clone()),
                _ => None,
            })
            .collect();
        Self {
            contents,
            included_revisions,
        }
    }
}

/// Prefix facts needed to render a later entry without rescanning earlier rows.
/// A message can be represented by an unresolved diagnostic; update this state
/// at the same point as cold rendering, not by inspecting successful rows alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WindowState {
    pub(super) represented_messages: BTreeSet<MessageRef>,
    pub(super) seen_deliveries: BTreeSet<DeliveryRef>,
}

impl super::Index<'_> {
    pub(super) fn window<'a, I>(&self, scope: &ReadingScope, entries: I) -> Vec<ReadingUnit>
    where
        I: Iterator<Item = &'a EntryRef> + Clone,
    {
        let bindings = WindowBindings::new(self, scope, entries.clone());
        let mut state = WindowState::default();
        let mut units = Vec::new();
        for entry in entries {
            units.extend(self.render_entry(scope, entry, &bindings, &mut state));
        }
        self.append_submitted(scope, &state, &mut units);
        group_work(units)
    }

    /// Render one entry using its selected bindings and the caller's prefix state.
    /// Grouping is separate so an incremental caller controls the affected neighbors.
    pub(super) fn render_entry(
        &self,
        scope: &ReadingScope,
        entry_ref: &EntryRef,
        bindings: &WindowBindings,
        state: &mut WindowState,
    ) -> Vec<ReadingUnit> {
        let contents = &bindings.contents;
        let included_revisions = &bindings.included_revisions;
        let mut units = Vec::new();
        let Some(TraceObject::HistoryEntry(entry)) =
            self.object(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
        else {
            return units;
        };
        let anchor = AnyObjectRef::HistoryEntry(entry_ref.clone());
        let order_basis = match self
            .reduction
            .histories
            .get(&entry.containing_agent)
            .and_then(|history| history.order_basis.get(entry_ref))
        {
            Some(ReadingOrderBasis::NativePredecessor) => ReadOrderBasis::NativePredecessor,
            Some(ReadingOrderBasis::ExplicitOrder) => ReadOrderBasis::ExplicitOrder,
            _ => ReadOrderBasis::PresentationTiebreak,
        };
        if !contents[entry_ref].complete {
            units.push(self.unknown(
                format!("{}:unobserved-content", TraceAddress::of(&anchor).as_str()),
                anchor.clone(),
                order_basis,
                "This entry's content has not been completely observed in the selected history",
            ));
        }
        for (slot, resolved) in &contents[entry_ref].slots {
            let address = format!("{}:{slot}", TraceAddress::of(&anchor).as_str());
            let Some(alternatives) = resolved.known() else {
                units.push(self.unknown(
                    address,
                    anchor.clone(),
                    order_basis,
                    "The selected entry content is unresolved or conflicting",
                ));
                continue;
            };
            let Some(content) = self.slot_content(alternatives) else {
                units.push(self.unknown(
                    address,
                    anchor.clone(),
                    order_basis,
                    "The selected content slot has conflicting or unavailable observations",
                ));
                continue;
            };
            match &content {
                HistoryContent::Message(revision_ref) => {
                    let Some(TraceObject::MessageRevision(revision)) =
                        self.object(&AnyObjectRef::MessageRevision(revision_ref.clone()))
                    else {
                        units.push(self.unknown(
                            address,
                            anchor.clone(),
                            order_basis,
                            "Message revision unavailable",
                        ));
                        continue;
                    };
                    let eligible: BTreeSet<_> = alternatives
                        .iter()
                        .filter_map(|content| match content {
                            HistoryContent::Message(reference) => Some(reference.clone()),
                            _ => None,
                        })
                        .collect();
                    let associated = self.associated_deliveries(entry, &eligible);
                    let revision = if associated.is_empty() {
                        if !state.represented_messages.insert(revision.message.clone()) {
                            continue;
                        }
                        let selected = self
                            .reduction
                            .select_message_in(&revision.message, included_revisions);
                        let Some(TraceObject::MessageRevision(selected)) =
                            selected.known().and_then(|reference| {
                                self.object(&AnyObjectRef::MessageRevision(reference.clone()))
                            })
                        else {
                            units.push(self.unknown(address,anchor.clone(),order_basis,"The selected history has conflicting or unordered message revisions"));
                            continue;
                        };
                        selected
                    } else {
                        revision
                    };
                    let Some(message) = super::message_metadata(self.reduction, &revision.message)
                    else {
                        units.push(self.unknown(
                            address,
                            anchor.clone(),
                            order_basis,
                            "Message author evidence unavailable",
                        ));
                        continue;
                    };
                    if associated.is_empty() {
                        units.push(self.message_unit(
                            scope,
                            address,
                            anchor.clone(),
                            order_basis,
                            &message,
                            revision,
                            None,
                            Some(entry),
                        ));
                    } else {
                        for (delivery, value, selected) in associated {
                            if !state.seen_deliveries.insert(delivery.clone()) {
                                continue;
                            }
                            let address = format!("{address}:{}", delivery.encode());
                            let Some(TraceObject::MessageRevision(delivered)) =
                                selected.known().and_then(|revision| {
                                    self.object(&AnyObjectRef::MessageRevision(revision.clone()))
                                })
                            else {
                                units.push(self.unknown(
                                    address,
                                    anchor.clone(),
                                    order_basis,
                                    "The delivery's observed content is unavailable or conflicting",
                                ));
                                continue;
                            };
                            let Some(delivered_message) =
                                super::message_metadata(self.reduction, &delivered.message)
                            else {
                                continue;
                            };
                            units.push(self.message_unit(
                                scope,
                                address,
                                anchor.clone(),
                                order_basis,
                                &delivered_message,
                                delivered,
                                Some(value),
                                Some(entry),
                            ));
                        }
                    }
                }
                HistoryContent::Operation(operation) => {
                    let Some(TraceObject::Operation(operation)) =
                        self.object(&AnyObjectRef::Operation(operation.clone()))
                    else {
                        units.push(self.unknown(
                            address,
                            anchor.clone(),
                            order_basis,
                            "Invocation details unavailable",
                        ));
                        continue;
                    };
                    // The invocation is observed even when its producer is not.
                    // Keep the entry's retention anchor and the operation's own
                    // inspection target; neither establishes local activity.
                    let owner = match &operation.owner {
                        OwnerRef::Agent { agent } => Some(agent),
                        _ => None,
                    };
                    units.push(ReadingUnit {
                        address,
                        anchor: TraceAddress::of(&anchor),
                        order_basis,
                        source_surface: source_label(entry_ref),
                        entry_context: None,
                        activation: operation
                            .activation
                            .as_ref()
                            .filter(|_| owner == Some(&scope.agent))
                            .map(|activation| {
                                self.execution(&AnyObjectRef::Activation(activation.clone()))
                            }),
                        content: ReadingContent::Work {
                            owner: owner.map(|owner| self.agent_link(owner)),
                            operations: count(1, "this invocation"),
                            agents: unknown_count("agents created by this invocation"),
                            assignments: unknown_count("task deliveries"),
                            failed_operations: count(
                                usize::from(matches!(
                                    self.execution(&AnyObjectRef::Operation(
                                        operation.operation_ref.clone()
                                    ))
                                    .state,
                                    ReadExecutionState::Terminated {
                                        outcome: TerminalOutcome::Failed,
                                        ..
                                    }
                                )),
                                "own operation outcomes",
                            ),
                            active_resources: unknown_count("independent resource states"),
                            required_input: unknown_count("required input"),
                            operations_anchor: TraceAddress::of(&AnyObjectRef::Operation(
                                operation.operation_ref.clone(),
                            )),
                        },
                    });
                }
                HistoryContent::Checkpoint(checkpoint) => units.push(ReadingUnit {
                    address,
                    anchor: TraceAddress::of(&anchor),
                    order_basis,
                    source_surface: source_label(entry_ref),
                    entry_context: None,
                    activation: None,
                    content: ReadingContent::ContextBoundary {
                        checkpoint: TraceAddress::of(&AnyObjectRef::ContextCheckpoint(
                            checkpoint.clone(),
                        )),
                        label: "Context checkpoint".into(),
                        summary: None,
                    },
                }),
                HistoryContent::Opaque(payload) => units.push(ReadingUnit {
                    address,
                    anchor: TraceAddress::of(&anchor),
                    order_basis,
                    source_surface: source_label(entry_ref),
                    entry_context: None,
                    activation: None,
                    content: ReadingContent::Unknown {
                        label: "Retained content".into(),
                        reason: "The source supplied opaque content".into(),
                        payloads: vec![self.access(
                            scope,
                            &anchor,
                            &payload.to_string(),
                            payload,
                            Representation::Conversation,
                            None,
                            Some(PayloadEncoding::Binary),
                        )],
                    },
                }),
            }
        }
        // The entry owns this assertion. Origin metadata is an independently
        // authorized link, so losing it must not erase inherited context.
        let context = self
            .copies
            .context(self.reduction, entry_ref, scope.selection.as_ref())
            .map(|proof| proof.context)
            .or_else(|| {
                entry.original_owner.as_ref().map(|owner| match owner {
                    OwnerRef::Agent { agent } => ReadEntryContext::Inherited {
                        original_owner: Some(self.agent_link(agent)),
                    },
                    OwnerRef::Unresolved { .. } | OwnerRef::Unknown => ReadEntryContext::Unresolved,
                })
            });
        for unit in &mut units {
            unit.entry_context = context.clone();
        }
        units
    }

    pub(super) fn append_submitted(
        &self,
        scope: &ReadingScope,
        state: &WindowState,
        units: &mut Vec<ReadingUnit>,
    ) {
        // A server-accepted input is visible before vendor admission, with exactly
        // its delivery stage. It never creates a synthetic history entry/activation.
        for delivery in self.submitted.get(&scope.agent).into_iter().flatten() {
            if scope.selection.is_some() || state.seen_deliveries.contains(&delivery.delivery_ref) {
                continue;
            }
            let anchor = AnyObjectRef::Delivery(delivery.delivery_ref.clone());
            let selected = self
                .reduction
                .select_delivery_revision(&delivery.delivery_ref, None);
            let Some(TraceObject::MessageRevision(revision)) = selected
                .known()
                .and_then(|revision| self.object(&AnyObjectRef::MessageRevision(revision.clone())))
            else {
                units.push(self.unknown(
                    TraceAddress::of(&anchor).as_str().into(),
                    anchor,
                    ReadOrderBasis::PresentationTiebreak,
                    "The submitted delivery's observed content is unavailable or conflicting",
                ));
                continue;
            };
            let Some(message) = super::message_metadata(self.reduction, &revision.message) else {
                continue;
            };
            units.push(self.message_unit(
                scope,
                TraceAddress::of(&anchor).as_str().into(),
                anchor,
                ReadOrderBasis::PresentationTiebreak,
                &message,
                revision,
                Some(delivery),
                None,
            ));
        }
    }
}

/// Group an ordered region of ungrouped units, including affected boundary rows.
/// Previously grouped rows are not inputs: their counts already include neighbors.
pub(super) fn group_work(units: impl IntoIterator<Item = ReadingUnit>) -> Vec<ReadingUnit> {
    let mut grouped: Vec<ReadingUnit> = Vec::new();
    for unit in units {
        if let (
            Some(previous),
            ReadingContent::Work {
                owner,
                operations,
                failed_operations,
                ..
            },
        ) = (grouped.last_mut(), &unit.content)
        {
            if previous.activation == unit.activation
                && unit.activation.is_some()
                && previous.entry_context == unit.entry_context
                // Optional provenance belongs to an entry's own proof. Grouping
                // distinct entries must not let the first proof authorize both.
                && (unit.entry_context.is_none() || previous.anchor == unit.anchor)
            {
                if let ReadingContent::Work {
                    owner: prior_owner,
                    operations: prior_operations,
                    failed_operations: prior_failed,
                    ..
                } = &mut previous.content
                {
                    if prior_owner
                        .as_ref()
                        .zip(owner.as_ref())
                        .is_some_and(|(a, b)| a.address == b.address)
                    {
                        prior_operations.value = prior_operations
                            .value
                            .zip(operations.value)
                            .map(|(a, b)| a + b);
                        prior_failed.value = prior_failed
                            .value
                            .zip(failed_operations.value)
                            .map(|(a, b)| a + b);
                        continue;
                    }
                }
            }
        }
        let mut unit = unit;
        if let ReadingContent::Work {
            operations_anchor, ..
        } = &mut unit.content
        {
            if let Some(reference) = unit
                .activation
                .as_ref()
                .and_then(|activation| activation.reference.clone())
            {
                *operations_anchor = reference;
            }
        }
        grouped.push(unit);
    }
    grouped
}
