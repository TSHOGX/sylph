//! Resource observations are evidence history, never an inferred current execution.
use super::*;

impl Index<'_> {
    pub(super) fn resource_observations(
        &self,
        resource: &Resource,
    ) -> Vec<ReadResourceObservation> {
        let reference = AnyObjectRef::Resource(resource.resource_ref.clone());
        let scope = ReadingScope {
            agent: reference.scope().clone(),
            selection: None,
        };
        let mut observations: Vec<_> = self
            .reduction
            .resource_states
            .get(&resource.resource_ref)
            .into_iter()
            .flatten()
            .map(|(state, claim)| {
                let payload = match state {
                    ResourceState::GoalUpdated { detail, .. } => detail.as_ref(),
                    ResourceState::ScheduleRegistered { registration } => Some(registration),
                    _ => None,
                };
                ReadResourceObservation {
                    claim_ref: claim.clone(),
                    order_basis: match claim.address() {
                        ObjectAddress::Occurrence {
                            position:
                                SourcePosition {
                                    locator:
                                        SourceLocator::Frame { .. } | SourceLocator::Jsonl { .. },
                                    ..
                                },
                            ..
                        } => ReadOrderBasis::ExplicitOrder,
                        _ => ReadOrderBasis::PresentationTiebreak,
                    },
                    source: match claim.address() {
                        ObjectAddress::Occurrence { position, .. } => Some(position.clone()),
                        _ => None,
                    },
                    state: match state {
                        ResourceState::Created => ReadResourceState::Created,
                        ResourceState::Running => ReadResourceState::Running,
                        ResourceState::Yielded => ReadResourceState::Yielded,
                        ResourceState::Closed { outcome, reason } => ReadResourceState::Closed {
                            outcome: *outcome,
                            reason: reason.as_ref().map(close_reason),
                        },
                        ResourceState::GoalUpdated { status, .. } => {
                            ReadResourceState::GoalUpdated {
                                status: status.clone(),
                            }
                        }
                        ResourceState::ScheduleRegistered { .. } => {
                            ReadResourceState::ScheduleRegistered
                        }
                        ResourceState::ScheduleCancelled => ReadResourceState::ScheduleCancelled,
                        ResourceState::ScheduleFired {
                            firing_key,
                            delivery,
                        } => ReadResourceState::ScheduleFired {
                            firing_key: firing_key.clone(),
                            delivery: delivery.as_ref().map(|delivery| {
                                TraceAddress::of(&AnyObjectRef::Delivery(delivery.clone()))
                            }),
                        },
                        ResourceState::Unknown { reason } => ReadResourceState::Unknown {
                            reason: reason.clone(),
                        },
                    },
                    payloads: payload
                        .map(|payload| {
                            self.access(
                                &scope,
                                &reference,
                                &claim.encode(),
                                payload,
                                Representation::Runtime,
                                Some("application/json".into()),
                                PayloadEncoding::Utf8,
                            )
                        })
                        .into_iter()
                        .collect(),
                }
            })
            .collect();
        // Source coordinates order that source's observations. Sorting independent
        // sources only groups them for paging; the UI explicitly declines a global
        // timeline, newest-state selection, or receipt-derived duration.
        observations.sort_by(|a, b| (&a.source, &a.claim_ref).cmp(&(&b.source, &b.claim_ref)));
        observations
    }
    pub(super) fn resource_links(&self, resource: &Resource, relations: &mut Vec<ReadRelation>) {
        let mut operations: BTreeSet<OperationRef> = relations
            .iter()
            .filter_map(|relation| match relation.target.object() {
                AnyObjectRef::Operation(operation) => Some(operation),
                _ => None,
            })
            .collect();
        if let Some(creator) = &resource.creator {
            operations.insert(creator.clone());
            relations.push(ReadRelation {
                containing_agent: AgentAddress::of(creator.scope()),
                target: TraceAddress::of(&AnyObjectRef::Operation(creator.clone())),
                label: "Creating operation".into(),
                agent: None,
            });
        }
        for operation in operations {
            if let Some(TraceObject::Operation(value)) =
                self.object(&AnyObjectRef::Operation(operation))
            {
                if let Some(activation) = &value.activation {
                    relations.push(ReadRelation {
                        containing_agent: AgentAddress::of(activation.scope()),
                        target: TraceAddress::of(&AnyObjectRef::Activation(activation.clone())),
                        label: "Related activation".into(),
                        agent: None,
                    });
                }
            }
        }
        relations.sort_by(|a, b| (&a.target, &a.label).cmp(&(&b.target, &b.label)));
        relations.dedup_by(|a, b| a.target == b.target && a.label == b.label);
    }
}
