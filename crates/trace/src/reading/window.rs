//! Per-scope resolved history content for window rendering.

use super::*;
use crate::reducer::ResolvedEntryContents;

/// Resolved source bindings shared by each slot in a history window.
/// Work grouping deliberately remains in `Index::window`: it depends on adjacent rows.
#[derive(Debug, Clone)]
pub(super) struct WindowBindings {
    pub(super) contents: BTreeMap<EntryRef, ResolvedEntryContents>,
    pub(super) included_revisions: BTreeSet<MessageRevisionRef>,
}

impl WindowBindings {
    pub(super) fn new(index: &Index<'_>, scope: &ReadingScope, entries: &[EntryRef]) -> Self {
        let contents = entries
            .iter()
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

impl super::Index<'_> {
    pub(super) fn window(&self, scope: &ReadingScope, entries: &[EntryRef]) -> Vec<ReadingUnit> {
        let bindings = WindowBindings::new(self, scope, entries);
        let contents = &bindings.contents;
        let included_revisions = &bindings.included_revisions;
        let mut units = Vec::new();
        let mut seen_deliveries = BTreeSet::new();
        let mut represented_messages = BTreeSet::new();
        for entry_ref in entries {
            let Some(TraceObject::HistoryEntry(entry)) =
                self.object(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
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
                        let associated = self
                            .deliveries
                            .get(entry_ref)
                            .into_iter()
                            .flatten()
                            .filter_map(|delivery| {
                                match self.object(&AnyObjectRef::Delivery(delivery.clone())) {
                                    Some(TraceObject::Delivery(value)) => {
                                        let revision = self
                                            .reduction
                                            .select_delivery_revision(delivery, Some(&eligible));
                                        (!matches!(revision, Resolved::Unknown))
                                            .then_some((delivery, value, revision))
                                    }
                                    _ => None,
                                }
                            })
                            .collect::<Vec<_>>();
                        let revision = if associated.is_empty() {
                            if !represented_messages.insert(revision.message.clone()) {
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
                        let Some(TraceObject::Message(message)) =
                            self.object(&AnyObjectRef::Message(revision.message.clone()))
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
                                message,
                                revision,
                                None,
                                Some(entry),
                            ));
                        } else {
                            for (delivery, value, selected) in associated {
                                seen_deliveries.insert(delivery.clone());
                                let address = format!("{address}:{}", delivery.encode());
                                let Some(TraceObject::MessageRevision(delivered)) =
                                    selected.known().and_then(|revision| {
                                        self.object(&AnyObjectRef::MessageRevision(
                                            revision.clone(),
                                        ))
                                    })
                                else {
                                    units.push(self.unknown(address, anchor.clone(), order_basis,
                                        "The delivery's observed content is unavailable or conflicting"));
                                    continue;
                                };
                                units.push(self.message_unit(
                                    scope,
                                    address,
                                    anchor.clone(),
                                    order_basis,
                                    message,
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
                        let OwnerRef::Agent { agent: owner } = &operation.owner else {
                            units.push(self.unknown(
                                address,
                                anchor.clone(),
                                order_basis,
                                "Operation owner is unresolved",
                            ));
                            continue;
                        };
                        units.push(ReadingUnit {
                            address,
                            anchor: TraceAddress::of(&anchor),
                            order_basis,
                            source_surface: source_label(entry_ref),
                            activation: operation
                                .activation
                                .as_ref()
                                .filter(|_| owner == &scope.agent)
                                .map(|activation| {
                                    self.execution(&AnyObjectRef::Activation(activation.clone()))
                                }),
                            content: ReadingContent::Work {
                                owner: self.agent_link(owner),
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
                                PayloadEncoding::Binary,
                            )],
                        },
                    }),
                }
            }
        }
        // A server-accepted input is visible before vendor admission, with exactly
        // its delivery stage. It never creates a synthetic history entry/activation.
        for delivery in self.submitted.get(&scope.agent).into_iter().flatten() {
            if scope.selection.is_some() || seen_deliveries.contains(&delivery.delivery_ref) {
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
            let Some(TraceObject::Message(message)) =
                self.object(&AnyObjectRef::Message(revision.message.clone()))
            else {
                continue;
            };
            units.push(self.message_unit(
                scope,
                TraceAddress::of(&anchor).as_str().into(),
                anchor,
                ReadOrderBasis::PresentationTiebreak,
                message,
                revision,
                Some(delivery),
                None,
            ));
        }
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
                if previous.activation == unit.activation && unit.activation.is_some() {
                    if let ReadingContent::Work {
                        owner: prior_owner,
                        operations: prior_operations,
                        failed_operations: prior_failed,
                        ..
                    } = &mut previous.content
                    {
                        if prior_owner.address == owner.address {
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
}
