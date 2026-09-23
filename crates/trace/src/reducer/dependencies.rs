//! Typed dependencies that can make an older unresolved fact change on declaration.

use std::collections::BTreeSet;
use sylph_protocol::trace::*;

pub(super) fn references(fact: &Fact) -> BTreeSet<AnyObjectRef> {
    let mut dependencies = Dependencies::default();
    dependencies.fact(fact);
    dependencies.objects
}

/// Claim identity is separate from object identity. Supersession links belong
/// to the enclosing Claim and are tracked by the caller alongside these links.
pub(super) fn claim_references(fact: &Fact) -> BTreeSet<ClaimRef> {
    let mut dependencies = Dependencies::default();
    dependencies.fact(fact);
    dependencies.claims
}

/// Extent membership can change when positions or complete-mapping witnesses
/// arrive from another agent's source. Inherited selections are object edges;
/// explicit observation sets are claim edges, both collected separately.
pub(super) fn selection_sources(fact: &Fact) -> BTreeSet<(SourceRef, u64)> {
    let mut dependencies = Dependencies::default();
    dependencies.fact(fact);
    dependencies.selection_sources
}

#[derive(Default)]
struct Dependencies {
    objects: BTreeSet<AnyObjectRef>,
    claims: BTreeSet<ClaimRef>,
    selection_sources: BTreeSet<(SourceRef, u64)>,
}

impl Dependencies {
    fn add(&mut self, reference: AnyObjectRef) {
        // A scoped address does not create its governing Agent. A later Agent
        // declaration can resolve an earlier object even without an owner field.
        self.objects
            .insert(AnyObjectRef::Agent(reference.scope().clone()));
        self.objects.insert(reference);
    }

    fn owner(&mut self, owner: &OwnerRef) {
        match owner {
            OwnerRef::Agent { agent } => self.add(AnyObjectRef::Agent(agent.clone())),
            OwnerRef::Unresolved {
                scope,
                namespace: _,
                native_key: _,
                reason: _,
            } => self.add(AnyObjectRef::Agent(scope.clone())),
        }
    }

    fn author(&mut self, author: &Author) {
        match author {
            Author::Agent { agent } => self.owner(agent),
            Author::Tool { operation } => {
                if let Some(operation) = operation {
                    self.add(AnyObjectRef::Operation(operation.clone()));
                }
            }
            Author::Human { subject: _ }
            | Author::Harness { identity: _ }
            | Author::External { identity: _ }
            | Author::Unknown => {}
        }
    }

    fn recipient(&mut self, recipient: &Recipient) {
        match recipient {
            Recipient::Agent { agent } => self.owner(agent),
            Recipient::Operation { operation } => {
                self.add(AnyObjectRef::Operation(operation.clone()));
            }
            Recipient::Human { subject: _ }
            | Recipient::External { identity: _ }
            | Recipient::Unknown => {}
        }
    }

    fn requester(&mut self, requester: &RequesterRef) {
        self.add(match requester {
            RequesterRef::Agent(reference) => AnyObjectRef::Agent(reference.clone()),
            RequesterRef::Activation(reference) => AnyObjectRef::Activation(reference.clone()),
            RequesterRef::Operation(reference) => AnyObjectRef::Operation(reference.clone()),
            RequesterRef::Delivery(reference) => AnyObjectRef::Delivery(reference.clone()),
            RequesterRef::Resource(reference) => AnyObjectRef::Resource(reference.clone()),
        });
    }

    fn history_content(&mut self, content: &HistoryContent) {
        match content {
            HistoryContent::Message(reference) => {
                self.add(AnyObjectRef::MessageRevision(reference.clone()));
            }
            HistoryContent::Operation(reference) => {
                self.add(AnyObjectRef::Operation(reference.clone()));
            }
            HistoryContent::Checkpoint(reference) => {
                self.add(AnyObjectRef::ContextCheckpoint(reference.clone()));
            }
            HistoryContent::Opaque(_) => {}
        }
    }

    fn extent(&mut self, extent: &SourceExtent) {
        let SourceExtent {
            source,
            generation,
            coordinate: _,
            start_inclusive: _,
            end_exclusive: _,
            mapping_rule: _,
            original_owner,
        } = extent;
        self.selection_sources.insert((source.clone(), *generation));
        if let Some(owner) = original_owner {
            self.owner(owner);
        }
    }

    fn inclusion(&mut self, coverage: &InclusionCoverage) {
        match coverage {
            // MessagePartRef has no independent TraceObject declaration. Parts
            // resolve through their message revision, never a made-up object ID.
            InclusionCoverage::Parts { parts: _ }
            | InclusionCoverage::ByteRanges {
                payload: _,
                ranges: _,
            }
            | InclusionCoverage::Exact
            | InclusionCoverage::Summary
            | InclusionCoverage::Unknown { reason: _ } => {}
        }
    }

    fn coverage_state(&mut self, state: &CoverageState) {
        match state {
            CoverageState::Conflicted { alternatives } => {
                self.claims.extend(alternatives.iter().cloned());
            }
            CoverageState::Following
            | CoverageState::Complete
            | CoverageState::Pending { reason: _ }
            | CoverageState::Gap { reason: _ }
            | CoverageState::Unavailable { reason: _ } => {}
        }
    }

    fn object(&mut self, object: &TraceObject) {
        self.add(object.object_ref());
        match object {
            TraceObject::Agent(AgentThread {
                agent_ref: _,
                vendor: _,
                name: _,
                model: _,
            }) => {}
            TraceObject::Activation(Activation {
                activation_ref: _,
                owner,
            }) => self.owner(owner),
            TraceObject::Message(Message {
                message_ref: _,
                author,
                protocol_role: _,
                purpose: _,
                production,
            }) => {
                self.author(author);
                if let Some(ProductionScope { owner, activation }) = production {
                    self.owner(owner);
                    if let Some(activation) = activation {
                        self.add(AnyObjectRef::Activation(activation.clone()));
                    }
                }
            }
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: _,
                message,
                representation: _,
                form,
                parts,
            }) => {
                self.add(AnyObjectRef::Message(message.clone()));
                match form {
                    RevisionForm::Append { predecessor } => {
                        if let Some(reference) = predecessor {
                            self.add(AnyObjectRef::MessageRevision(reference.clone()));
                        }
                    }
                    RevisionForm::Correction { replaces } => {
                        self.add(AnyObjectRef::MessageRevision(replaces.clone()));
                    }
                    RevisionForm::Complete
                    | RevisionForm::Replacement { covers: _ }
                    | RevisionForm::Final { covers: _ } => {}
                }
                for ContentPart {
                    part_ref: _,
                    kind: _,
                    payload: _,
                    media_type: _,
                    encoding: _,
                } in parts
                {}
            }
            TraceObject::Delivery(Delivery {
                delivery_ref: _,
                sender,
                recipient,
                route,
            }) => {
                self.author(sender);
                self.recipient(recipient);
                match route {
                    DeliveryRoute::ToolReturn { operation } => {
                        self.add(AnyObjectRef::Operation(operation.clone()));
                    }
                    DeliveryRoute::ServerCommand { command_id: _ }
                    | DeliveryRoute::VendorMailbox {
                        namespace: _,
                        occurrence: _,
                    }
                    | DeliveryRoute::NativeHistory
                    | DeliveryRoute::External { namespace: _ }
                    | DeliveryRoute::Unknown => {}
                }
            }
            TraceObject::Operation(Operation {
                operation_ref: _,
                owner,
                activation,
                requester,
                category: _,
                input_revision: _,
                argument_state: _,
            }) => {
                self.owner(owner);
                if let Some(activation) = activation {
                    self.add(AnyObjectRef::Activation(activation.clone()));
                }
                if let Some(requester) = requester {
                    self.requester(requester);
                }
            }
            TraceObject::Result(ResultOccurrence {
                result_ref: _,
                operation,
                recipient,
                disposition: _,
                representation: _,
                payload: _,
                delivery,
            }) => {
                self.add(AnyObjectRef::Operation(operation.clone()));
                if let Some(recipient) = recipient {
                    self.recipient(recipient);
                }
                if let Some(delivery) = delivery {
                    self.add(AnyObjectRef::Delivery(delivery.clone()));
                }
            }
            TraceObject::Resource(Resource {
                resource_ref: _,
                creator,
                host,
                kind: _,
            }) => {
                if let Some(creator) = creator {
                    self.add(AnyObjectRef::Operation(creator.clone()));
                }
                if let Some(host) = host {
                    self.owner(host);
                }
            }
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: _,
                containing_agent,
                predecessor,
                original_owner,
            }) => {
                self.add(AnyObjectRef::Agent(containing_agent.clone()));
                if let Some(predecessor) = predecessor {
                    self.add(AnyObjectRef::HistoryEntry(predecessor.clone()));
                }
                if let Some(owner) = original_owner {
                    self.owner(owner);
                }
            }
            TraceObject::HistorySelection(HistorySelection {
                selection_ref: _,
                agent,
                membership,
                content_scope,
                excluded_heads,
            }) => {
                self.add(AnyObjectRef::Agent(agent.clone()));
                match membership {
                    SelectionMembership::EntryPath { head } => {
                        self.add(AnyObjectRef::HistoryEntry(head.clone()));
                    }
                    SelectionMembership::EntrySet { entries } => {
                        for entry in entries {
                            self.add(AnyObjectRef::HistoryEntry(entry.clone()));
                        }
                    }
                    SelectionMembership::SourceExtents {
                        extents,
                        inherited_selections,
                    } => {
                        for extent in extents {
                            self.extent(extent);
                        }
                        for selection in inherited_selections {
                            self.add(AnyObjectRef::HistorySelection(selection.clone()));
                        }
                    }
                }
                match content_scope {
                    HistoryContentScope::ObservationSet { observations } => {
                        self.claims.extend(observations.iter().cloned())
                    }
                    HistoryContentScope::AllObservedAtCut => {}
                }
                for head in excluded_heads {
                    self.add(AnyObjectRef::HistoryEntry(head.clone()));
                }
            }
            TraceObject::ContextCheckpoint(ContextCheckpoint {
                checkpoint_ref: _,
                owner,
                basis: _,
                content,
                coverage,
            }) => {
                self.owner(owner);
                match content {
                    ContextContent::Ordered {
                        entries,
                        settings: _,
                    } => {
                        for content in entries {
                            self.history_content(content);
                        }
                    }
                    ContextContent::Selection { selection } => {
                        self.add(AnyObjectRef::HistorySelection(selection.clone()))
                    }
                    ContextContent::OpaqueReplacement { payload: _ }
                    | ContextContent::Summary { payload: _ }
                    | ContextContent::Unknown { reason: _ } => {}
                }
                self.inclusion(coverage);
            }
            TraceObject::Measurement(Measurement {
                model: _,
                measurement_ref: _,
                subject,
                metric: _,
                unit: _,
                value: _,
                mode,
                aggregation_scope: _,
                components,
            }) => {
                self.add(subject.clone());
                match mode {
                    MeasurementMode::Interval { interval_ref } => self.add(interval_ref.clone()),
                    MeasurementMode::Cumulative {
                        counter_domain: _,
                        reset_epoch: _,
                        sample_position: _,
                    }
                    | MeasurementMode::Gauge => {}
                }
                for MeasurementComponent {
                    measurement,
                    relationship: _,
                } in components
                {
                    self.add(AnyObjectRef::Measurement(measurement.clone()));
                }
            }
        }
    }

    fn fact(&mut self, fact: &Fact) {
        match fact {
            Fact::Declare { object } => self.object(object),
            Fact::AgentState { agent, state } => {
                self.add(AnyObjectRef::Agent(agent.clone()));
                match state {
                    AgentState::Interior { state } => self.coverage_state(state),
                    AgentState::Exists | AgentState::VendorClosed { reason: _ } => {}
                }
            }
            Fact::DeliveryStage { delivery, stage } => {
                self.add(AnyObjectRef::Delivery(delivery.clone()));
                match stage {
                    DeliveryStage::Admitted { entry } => {
                        if let Some(entry) = entry {
                            self.add(AnyObjectRef::HistoryEntry(entry.clone()));
                        }
                    }
                    DeliveryStage::Included { target, coverage } => {
                        self.add(match target {
                            InclusionTarget::Inference { operation } => {
                                AnyObjectRef::Operation(operation.clone())
                            }
                            InclusionTarget::Checkpoint { checkpoint } => {
                                AnyObjectRef::ContextCheckpoint(checkpoint.clone())
                            }
                        });
                        self.inclusion(coverage);
                    }
                    DeliveryStage::Requested
                    | DeliveryStage::Accepted
                    | DeliveryStage::Queued
                    | DeliveryStage::Rejected { reason: _ }
                    | DeliveryStage::Cancelled { reason: _ } => {}
                }
            }
            Fact::Execution {
                execution,
                boundary,
            } => {
                self.add(execution.object_ref());
                match boundary {
                    ExecutionBoundary::Started { time: _ }
                    | ExecutionBoundary::Terminated { time: _, reason: _ }
                    | ExecutionBoundary::Outcome {
                        outcome: _,
                        reason: _,
                        detail: _,
                    }
                    | ExecutionBoundary::InterruptRequested { command_id: _ } => {}
                }
            }
            Fact::ResourceState { resource, state } => {
                self.add(AnyObjectRef::Resource(resource.clone()));
                match state {
                    ResourceState::ScheduleFired {
                        firing_key: _,
                        delivery,
                    } => {
                        if let Some(delivery) = delivery {
                            self.add(AnyObjectRef::Delivery(delivery.clone()));
                        }
                    }
                    ResourceState::Created
                    | ResourceState::Running
                    | ResourceState::Yielded
                    | ResourceState::Closed {
                        outcome: _,
                        reason: _,
                    }
                    | ResourceState::GoalUpdated {
                        status: _,
                        detail: _,
                    }
                    | ResourceState::ScheduleRegistered { registration: _ }
                    | ResourceState::ScheduleCancelled
                    | ResourceState::Unknown { reason: _ } => {}
                }
            }
            Fact::Relation { relation } => {
                for reference in relation.subject_refs() {
                    self.add(reference);
                }
                match relation {
                    // subject_refs deliberately omits unresolved ownership, which
                    // still depends on the named scope for incremental reduction.
                    Relation::ContextLineage {
                        recipient: _,
                        checkpoint: _,
                        original_owner,
                    } => self.owner(original_owner),
                    Relation::Spawn {
                        parent: _,
                        child: _,
                        operation: _,
                    }
                    | Relation::Interaction {
                        sender: _,
                        recipient: _,
                        delivery: _,
                        operation: _,
                    }
                    | Relation::Trigger {
                        delivery: _,
                        activation: _,
                    }
                    | Relation::Nesting {
                        enclosing: _,
                        nested: _,
                    }
                    | Relation::UsesResource {
                        operation: _,
                        resource: _,
                    }
                    | Relation::HistoryPredecessor {
                        predecessor: _,
                        entry: _,
                    }
                    | Relation::ContextCopy {
                        original: _,
                        copied: _,
                        original_position: _,
                        copied_position: _,
                    }
                    | Relation::ContextMembership {
                        checkpoint: _,
                        selection: _,
                        disposition: _,
                    }
                    | Relation::Representation {
                        original: _,
                        transformed: _,
                        rule: _,
                    }
                    | Relation::ReadingOrder {
                        before: _,
                        after: _,
                    } => {}
                }
            }
            Fact::SelectHistory {
                agent,
                selection,
                selected_at: _,
            } => {
                self.add(AnyObjectRef::Agent(agent.clone()));
                self.add(AnyObjectRef::HistorySelection(selection.clone()));
            }
            Fact::HistoryPosition {
                entry,
                position: _,
                coordinate: _,
                value: _,
                mapping_rule: _,
            } => {
                self.add(AnyObjectRef::HistoryEntry(entry.clone()));
            }
            Fact::HistoryContentObserved {
                observation:
                    HistoryContentObservation {
                        entry,
                        position: _,
                        mapping_rule: _,
                        slots,
                        complete: _,
                    },
            } => {
                self.add(AnyObjectRef::HistoryEntry(entry.clone()));
                for HistorySlot { slot: _, content } in slots {
                    self.history_content(content);
                }
            }
            Fact::DeliveryRevisionObserved { delivery, revision } => {
                self.add(AnyObjectRef::Delivery(delivery.clone()));
                self.add(AnyObjectRef::MessageRevision(revision.clone()));
            }
            Fact::HistoryMappingComplete { extent } => self.extent(extent),
            Fact::Equivalence {
                left,
                right,
                rule: _,
            } => {
                self.add(left.clone());
                self.add(right.clone());
            }
            Fact::Coverage {
                coverage:
                    Coverage {
                        subject,
                        source: _,
                        dimension: _,
                        interval,
                        state,
                    },
            } => {
                self.add(subject.clone());
                match interval {
                    CoverageInterval::History { selection } => {
                        self.add(AnyObjectRef::HistorySelection(selection.clone()))
                    }
                    CoverageInterval::Object { object } => self.add(object.clone()),
                    CoverageInterval::CapturedPrefix {
                        source: _,
                        generation: _,
                        complete_records: _,
                        complete_bytes: _,
                    }
                    | CoverageInterval::Source {
                        first: _,
                        through: _,
                    }
                    | CoverageInterval::Payload {
                        payload: _,
                        ranges: _,
                    } => {}
                }
                self.coverage_state(state);
            }
            Fact::Diagnostic {
                diagnostic:
                    TraceDiagnostic {
                        subject,
                        code: _,
                        message: _,
                        related_claims,
                    },
            } => {
                if let Some(subject) = subject {
                    self.add(subject.clone());
                }
                self.claims.extend(related_claims.iter().cloned());
            }
        }
        // PayloadRef, SourceRef/Position, clocks, vendor handles and free-form
        // strings are not object identities. Their availability/order has separate
        // eligibility rules; parsing serialized strings would invent dependencies.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> AgentRef {
        AgentRef::Session(sylph_protocol::SessionId::decode(&format!("env7:codex:{name}")).unwrap())
    }

    fn operation(name: &str) -> OperationRef {
        OperationRef::native(agent("root"), "operation", vec![name.into()]).unwrap()
    }

    fn revision(name: &str) -> MessageRevisionRef {
        MessageRevisionRef::native(agent("root"), "revision", vec![name.into()]).unwrap()
    }

    fn claim(name: &str) -> ClaimRef {
        ClaimRef::native(agent("root"), "claim", vec![name.into()]).unwrap()
    }

    fn selection(name: &str) -> SelectionRef {
        SelectionRef::native(agent("root"), "selection", vec![name.into()]).unwrap()
    }

    fn source() -> SourceRef {
        SourceRef {
            issuer: Issuer::Server("fixture".into()),
            instance: "source".into(),
        }
    }

    fn unresolved(scope: AgentRef) -> OwnerRef {
        OwnerRef::Unresolved {
            scope,
            namespace: "vendor".into(),
            native_key: vec!["missing".into()],
            reason: "Owner unknown".into(),
        }
    }

    fn contains(fact: Fact, expected: impl IntoIterator<Item = AnyObjectRef>) {
        let actual = references(&fact);
        for reference in expected {
            assert!(
                actual.contains(&reference),
                "nested dependency {reference:?} absent from {actual:?}"
            );
        }
    }

    #[test]
    fn declarations_include_owner_author_requester_and_route_dependencies() {
        let message = MessageRef::native(agent("root"), "message", vec!["old".into()]).unwrap();
        let activation =
            ActivationRef::native(agent("root"), "activation", vec!["missing".into()]).unwrap();
        contains(
            Fact::Declare {
                object: TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Tool {
                        operation: Some(operation("author")),
                    },
                    protocol_role: None,
                    purpose: MessagePurpose::Unclassified,
                    production: Some(ProductionScope {
                        owner: unresolved(agent("owner-scope")),
                        activation: Some(activation.clone()),
                    }),
                }),
            },
            [
                AnyObjectRef::Message(message),
                AnyObjectRef::Operation(operation("author")),
                AnyObjectRef::Agent(agent("owner-scope")),
                AnyObjectRef::Activation(activation.clone()),
            ],
        );
        let delivery = DeliveryRef::native(agent("root"), "delivery", vec!["old".into()]).unwrap();
        contains(
            Fact::Declare {
                object: TraceObject::Delivery(Delivery {
                    delivery_ref: delivery.clone(),
                    sender: Author::Agent {
                        agent: unresolved(agent("sender")),
                    },
                    recipient: Recipient::Operation {
                        operation: operation("recipient"),
                    },
                    route: DeliveryRoute::ToolReturn {
                        operation: operation("route"),
                    },
                }),
            },
            [
                AnyObjectRef::Delivery(delivery.clone()),
                AnyObjectRef::Agent(agent("sender")),
                AnyObjectRef::Operation(operation("recipient")),
                AnyObjectRef::Operation(operation("route")),
            ],
        );
        let resource =
            ResourceRef::native(agent("root"), "resource", vec!["requester".into()]).unwrap();
        for (requester, expected) in [
            (
                RequesterRef::Agent(agent("requester")),
                AnyObjectRef::Agent(agent("requester")),
            ),
            (
                RequesterRef::Activation(activation.clone()),
                AnyObjectRef::Activation(activation.clone()),
            ),
            (
                RequesterRef::Operation(operation("requester")),
                AnyObjectRef::Operation(operation("requester")),
            ),
            (
                RequesterRef::Delivery(delivery.clone()),
                AnyObjectRef::Delivery(delivery),
            ),
            (
                RequesterRef::Resource(resource.clone()),
                AnyObjectRef::Resource(resource),
            ),
        ] {
            contains(
                Fact::Declare {
                    object: TraceObject::Operation(Operation {
                        operation_ref: operation("old"),
                        owner: OwnerRef::Agent {
                            agent: agent("owner"),
                        },
                        activation: Some(activation.clone()),
                        requester: Some(requester),
                        category: OperationCategory::Wait,
                        input_revision: None,
                        argument_state: ArgumentState::Collecting,
                    }),
                },
                [
                    expected,
                    AnyObjectRef::Agent(agent("owner")),
                    AnyObjectRef::Activation(activation.clone()),
                ],
            );
        }
    }

    #[test]
    fn revision_predecessors_and_corrections_remain_dependencies() {
        let message = MessageRef::native(agent("root"), "message", vec!["missing".into()]).unwrap();
        for form in [
            RevisionForm::Append {
                predecessor: Some(revision("earlier")),
            },
            RevisionForm::Correction {
                replaces: revision("earlier"),
            },
        ] {
            contains(
                Fact::Declare {
                    object: TraceObject::MessageRevision(MessageRevision {
                        revision_ref: revision("new"),
                        message: message.clone(),
                        representation: Representation::Conversation,
                        form,
                        parts: vec![],
                    }),
                },
                [
                    AnyObjectRef::MessageRevision(revision("new")),
                    AnyObjectRef::MessageRevision(revision("earlier")),
                    AnyObjectRef::Message(message.clone()),
                ],
            );
        }
    }

    #[test]
    fn selected_history_retains_inherited_excluded_and_observation_dependencies() {
        let excluded = EntryRef::native(agent("root"), "entry", vec!["excluded".into()]).unwrap();
        let fact = Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection("current"),
                agent: agent("root"),
                membership: SelectionMembership::SourceExtents {
                    extents: vec![SourceExtent {
                        source: source(),
                        generation: 0,
                        coordinate: HistoryCoordinate::LocalRecordIndex,
                        start_inclusive: 0,
                        end_exclusive: 3,
                        mapping_rule: "fixture".into(),
                        original_owner: Some(unresolved(agent("source-owner"))),
                    }],
                    inherited_selections: vec![selection("inherited")],
                },
                content_scope: HistoryContentScope::ObservationSet {
                    observations: vec![claim("missing-content")],
                },
                excluded_heads: vec![excluded.clone()],
            }),
        };
        contains(
            fact.clone(),
            [
                AnyObjectRef::HistorySelection(selection("current")),
                AnyObjectRef::HistorySelection(selection("inherited")),
                AnyObjectRef::HistoryEntry(excluded),
                AnyObjectRef::Agent(agent("source-owner")),
            ],
        );
        assert_eq!(
            claim_references(&fact),
            BTreeSet::from([claim("missing-content")])
        );
        assert_eq!(selection_sources(&fact), BTreeSet::from([(source(), 0)]));
    }

    #[test]
    fn source_extents_keep_the_physical_generation_and_do_not_invent_one_for_ordinal_domains() {
        let domain = SourceRef {
            issuer: Issuer::Server("fixture".into()),
            instance: "ordinal-domain".into(),
        };
        let extent = |generation| SourceExtent {
            source: source(),
            generation,
            coordinate: HistoryCoordinate::VendorOrdinal {
                domain_ref: domain.clone(),
            },
            start_inclusive: 8,
            end_exclusive: 11,
            mapping_rule: "vendor-coordinate".into(),
            original_owner: None,
        };
        let fact = Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection("cross-agent"),
                agent: agent("different-owner"),
                membership: SelectionMembership::SourceExtents {
                    extents: vec![extent(2), extent(7)],
                    inherited_selections: vec![selection("inherited")],
                },
                content_scope: HistoryContentScope::AllObservedAtCut,
                excluded_heads: vec![],
            }),
        };
        assert_eq!(
            selection_sources(&fact),
            BTreeSet::from([(source(), 2), (source(), 7)])
        );
        assert_eq!(
            selection_sources(&Fact::HistoryMappingComplete { extent: extent(7) }),
            BTreeSet::from([(source(), 7)])
        );
        let selecting = Fact::SelectHistory {
            agent: agent("root"),
            selection: selection("not-declared"),
            selected_at: SourcePosition {
                source: source(),
                generation: 9,
                locator: SourceLocator::Frame { ordinal: 1 },
            },
        };
        assert!(selection_sources(&selecting).is_empty());
        assert!(references(&selecting)
            .contains(&AnyObjectRef::HistorySelection(selection("not-declared"))));
    }

    #[test]
    fn coverage_intervals_measurement_intervals_and_schedule_firings_are_indexed() {
        let resource =
            ResourceRef::native(agent("root"), "resource", vec!["schedule".into()]).unwrap();
        let delivery =
            DeliveryRef::native(agent("root"), "delivery", vec!["firing".into()]).unwrap();
        contains(
            Fact::ResourceState {
                resource: resource.clone(),
                state: ResourceState::ScheduleFired {
                    firing_key: vec!["1".into()],
                    delivery: Some(delivery.clone()),
                },
            },
            [
                AnyObjectRef::Resource(resource),
                AnyObjectRef::Delivery(delivery),
            ],
        );
        for interval in [
            CoverageInterval::History {
                selection: selection("covered"),
            },
            CoverageInterval::Object {
                object: AnyObjectRef::Operation(operation("covered")),
            },
        ] {
            let fact = Fact::Coverage {
                coverage: Coverage {
                    subject: AnyObjectRef::Agent(agent("root")),
                    source: source(),
                    dimension: CoverageDimension::Context,
                    interval: interval.clone(),
                    state: CoverageState::Conflicted {
                        alternatives: vec![claim("coverage")],
                    },
                },
            };
            let expected = match interval {
                CoverageInterval::History { selection } => {
                    AnyObjectRef::HistorySelection(selection)
                }
                CoverageInterval::Object { object } => object,
                _ => unreachable!(),
            };
            contains(fact.clone(), [expected]);
            assert_eq!(claim_references(&fact), BTreeSet::from([claim("coverage")]));
        }
        let measurement =
            MeasurementRef::native(agent("root"), "metric", vec!["combined".into()]).unwrap();
        let component =
            MeasurementRef::native(agent("root"), "metric", vec!["missing".into()]).unwrap();
        contains(
            Fact::Declare {
                object: TraceObject::Measurement(Measurement {
                    model: None,
                    measurement_ref: measurement.clone(),
                    subject: AnyObjectRef::Agent(agent("root")),
                    metric: Metric::OutputTokens,
                    unit: MeasurementUnit::Tokens,
                    value: 1.into(),
                    mode: MeasurementMode::Interval {
                        interval_ref: AnyObjectRef::Operation(operation("interval")),
                    },
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![MeasurementComponent {
                        measurement: component.clone(),
                        relationship: ComponentRelationship::Included,
                    }],
                }),
            },
            [
                AnyObjectRef::Measurement(measurement),
                AnyObjectRef::Measurement(component),
                AnyObjectRef::Operation(operation("interval")),
            ],
        );
    }

    #[test]
    fn context_and_delivery_inclusion_track_nested_history_and_execution() {
        let checkpoint =
            CheckpointRef::native(agent("root"), "context", vec!["missing".into()]).unwrap();
        contains(
            Fact::Declare {
                object: TraceObject::ContextCheckpoint(ContextCheckpoint {
                    checkpoint_ref: checkpoint.clone(),
                    owner: unresolved(agent("context-owner")),
                    basis: ContextBasis::ObservedRequest,
                    content: ContextContent::Ordered {
                        entries: vec![
                            HistoryContent::Message(revision("context")),
                            HistoryContent::Operation(operation("context")),
                        ],
                        settings: None,
                    },
                    coverage: InclusionCoverage::Exact,
                }),
            },
            [
                AnyObjectRef::ContextCheckpoint(checkpoint.clone()),
                AnyObjectRef::Agent(agent("context-owner")),
                AnyObjectRef::MessageRevision(revision("context")),
                AnyObjectRef::Operation(operation("context")),
            ],
        );
        let delivery =
            DeliveryRef::native(agent("root"), "delivery", vec!["included".into()]).unwrap();
        for target in [
            InclusionTarget::Checkpoint {
                checkpoint: checkpoint.clone(),
            },
            InclusionTarget::Inference {
                operation: operation("inference"),
            },
        ] {
            let expected = match &target {
                InclusionTarget::Checkpoint { checkpoint } => {
                    AnyObjectRef::ContextCheckpoint(checkpoint.clone())
                }
                InclusionTarget::Inference { operation } => {
                    AnyObjectRef::Operation(operation.clone())
                }
            };
            contains(
                Fact::DeliveryStage {
                    delivery: delivery.clone(),
                    stage: DeliveryStage::Included {
                        target,
                        coverage: InclusionCoverage::Exact,
                    },
                },
                [AnyObjectRef::Delivery(delivery.clone()), expected],
            );
        }
        contains(
            Fact::Relation {
                relation: Relation::ContextLineage {
                    recipient: agent("root"),
                    checkpoint: Some(checkpoint),
                    original_owner: unresolved(agent("lineage-owner")),
                },
            },
            [AnyObjectRef::Agent(agent("lineage-owner"))],
        );
    }

    #[test]
    fn diagnostic_and_interior_claim_links_remain_distinct_from_object_addresses() {
        let diagnostic = Fact::Diagnostic {
            diagnostic: TraceDiagnostic {
                subject: Some(AnyObjectRef::Operation(operation("missing"))),
                code: DiagnosticCode::CaptureGap,
                message: revision("just-text").encode(),
                related_claims: vec![claim("related"), claim("related")],
            },
        };
        assert_eq!(
            references(&diagnostic),
            BTreeSet::from([
                AnyObjectRef::Agent(agent("root")),
                AnyObjectRef::Operation(operation("missing"))
            ])
        );
        assert_eq!(
            claim_references(&diagnostic),
            BTreeSet::from([claim("related")])
        );
        let interior = Fact::AgentState {
            agent: agent("root"),
            state: AgentState::Interior {
                state: CoverageState::Conflicted {
                    alternatives: vec![claim("interior")],
                },
            },
        };
        assert_eq!(
            claim_references(&interior),
            BTreeSet::from([claim("interior")])
        );
    }
}
