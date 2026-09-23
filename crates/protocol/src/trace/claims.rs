//! Evidenced facts, with field-specific correction instead of arrival precedence.

use serde::{Deserialize, Serialize};

use super::{identity::*, objects::*};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Relation {
    Spawn {
        parent: AgentRef,
        child: AgentRef,
        operation: Option<OperationRef>,
    },
    Interaction {
        sender: AgentRef,
        recipient: AgentRef,
        delivery: Option<DeliveryRef>,
        operation: Option<OperationRef>,
    },
    Trigger {
        delivery: DeliveryRef,
        activation: ActivationRef,
    },
    Nesting {
        enclosing: OperationRef,
        nested: OperationRef,
    },
    UsesResource {
        operation: OperationRef,
        resource: ResourceRef,
    },
    HistoryPredecessor {
        predecessor: EntryRef,
        entry: EntryRef,
    },
    ContextLineage {
        recipient: AgentRef,
        /// Absent when the source identifies an origin without naming a checkpoint.
        checkpoint: Option<CheckpointRef>,
        original_owner: OwnerRef,
    },
    ContextMembership {
        checkpoint: CheckpointRef,
        selection: SelectionRef,
        disposition: ContextMembershipKind,
    },
    Representation {
        original: MessageRevisionRef,
        transformed: MessageRevisionRef,
        rule: String,
    },
    ReadingOrder {
        before: EntryRef,
        after: EntryRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMembershipKind {
    Included,
    Preserved,
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageDimension {
    Messages,
    Operations,
    ExecutionBoundaries,
    Timing,
    ChildInterior,
    PayloadRanges,
    Context,
    Measurements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CoverageInterval {
    CapturedPrefix {
        source: SourceRef,
        generation: u64,
        complete_records: u64,
        complete_bytes: u64,
    },
    Source {
        first: SourcePosition,
        through: SourcePosition,
    },
    History {
        selection: SelectionRef,
    },
    Object {
        object: AnyObjectRef,
    },
    Payload {
        payload: super::capture::PayloadRef,
        ranges: Vec<ByteRange>,
    },
}

/// Observation status is not an execution outcome or the requesting user's grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CoverageState {
    Following,
    Complete,
    Pending { reason: String },
    Gap { reason: String },
    Unavailable { reason: String },
    Conflicted { alternatives: Vec<ClaimRef> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub subject: AnyObjectRef,
    pub source: SourceRef,
    pub dimension: CoverageDimension,
    pub interval: CoverageInterval,
    pub state: CoverageState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    ReportedFailure,
    DecodeFailure,
    UnsupportedKind,
    MissingOwner,
    MissingSource,
    ConflictingIdentity,
    ConflictingOwner,
    ConflictingOutcome,
    InvalidRelation,
    CyclicHistory,
    CyclicSpawn,
    UnknownSelection,
    LateSegment,
    IncompatibleClock,
    UnknownCounterBaseline,
    CaptureGap,
    ResourceLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDiagnostic {
    pub subject: Option<AnyObjectRef>,
    pub code: DiagnosticCode,
    pub message: String,
    pub related_claims: Vec<ClaimRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentState {
    Exists,
    VendorClosed { reason: Option<String> },
    Interior { state: CoverageState },
}

/// Each family has typed required fields; extension data cannot redefine an owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Fact {
    Declare {
        object: TraceObject,
    },
    AgentState {
        agent: AgentRef,
        state: AgentState,
    },
    DeliveryStage {
        delivery: DeliveryRef,
        stage: DeliveryStage,
    },
    Execution {
        execution: ExecutionRef,
        boundary: ExecutionBoundary,
    },
    ResourceState {
        resource: ResourceRef,
        state: ResourceState,
    },
    Relation {
        relation: Relation,
    },
    SelectHistory {
        agent: AgentRef,
        selection: SelectionRef,
        selected_at: SourcePosition,
    },
    HistoryPosition {
        entry: EntryRef,
        position: SourcePosition,
        coordinate: HistoryCoordinate,
        value: u64,
        mapping_rule: String,
    },
    HistoryContentObserved {
        observation: HistoryContentObservation,
    },
    DeliveryRevisionObserved {
        delivery: DeliveryRef,
        revision: MessageRevisionRef,
    },
    /// The mapper considered every source record in this bounded coordinate range.
    /// This proves that an empty history-member result is known empty, not missing.
    HistoryMappingComplete {
        extent: SourceExtent,
    },
    Equivalence {
        left: AnyObjectRef,
        right: AnyObjectRef,
        rule: String,
    },
    Coverage {
        coverage: Coverage,
    },
    Diagnostic {
        diagnostic: TraceDiagnostic,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceFactKind {
    Declaration,
    AgentState,
    DeliveryStage,
    Execution,
    ResourceState,
    Relation,
    HistorySelection,
    HistoryPosition,
    HistoryContentObserved,
    DeliveryRevisionObserved,
    HistoryMappingComplete,
    Equivalence,
    Coverage,
    Diagnostic,
}

impl TraceFactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::AgentState => "agent_state",
            Self::DeliveryStage => "delivery_stage",
            Self::Execution => "execution",
            Self::ResourceState => "resource_state",
            Self::Relation => "relation",
            Self::HistorySelection => "history_selection",
            Self::HistoryPosition => "history_position",
            Self::HistoryContentObserved => "history_content_observed",
            Self::DeliveryRevisionObserved => "delivery_revision_observed",
            Self::HistoryMappingComplete => "history_mapping_complete",
            Self::Equivalence => "equivalence",
            Self::Coverage => "coverage",
            Self::Diagnostic => "diagnostic",
        }
    }
}

impl Fact {
    pub fn payload_refs(&self) -> Vec<&super::PayloadRef> {
        match self {
            Self::Declare { object } => object.payload_refs(),
            Self::HistoryContentObserved { observation } => observation
                .slots
                .iter()
                .filter_map(|slot| match &slot.content {
                    HistoryContent::Opaque(payload) => Some(payload),
                    _ => None,
                })
                .collect(),
            Self::Execution {
                boundary: ExecutionBoundary::Outcome { detail, .. },
                ..
            } => detail.iter().collect(),
            Self::ResourceState {
                state: ResourceState::GoalUpdated { detail, .. },
                ..
            } => detail.iter().collect(),
            Self::ResourceState {
                state: ResourceState::ScheduleRegistered { registration },
                ..
            } => vec![registration],
            Self::DeliveryStage {
                stage:
                    DeliveryStage::Included {
                        coverage: InclusionCoverage::ByteRanges { payload, .. },
                        ..
                    },
                ..
            } => vec![payload],
            Self::Coverage {
                coverage:
                    Coverage {
                        interval: CoverageInterval::Payload { payload, .. },
                        ..
                    },
            } => vec![payload],
            _ => vec![],
        }
    }

    pub fn kind(&self) -> TraceFactKind {
        match self {
            Self::Declare { .. } => TraceFactKind::Declaration,
            Self::AgentState { .. } => TraceFactKind::AgentState,
            Self::DeliveryStage { .. } => TraceFactKind::DeliveryStage,
            Self::Execution { .. } => TraceFactKind::Execution,
            Self::ResourceState { .. } => TraceFactKind::ResourceState,
            Self::Relation { .. } => TraceFactKind::Relation,
            Self::SelectHistory { .. } => TraceFactKind::HistorySelection,
            Self::HistoryPosition { .. } => TraceFactKind::HistoryPosition,
            Self::HistoryContentObserved { .. } => TraceFactKind::HistoryContentObserved,
            Self::DeliveryRevisionObserved { .. } => TraceFactKind::DeliveryRevisionObserved,
            Self::HistoryMappingComplete { .. } => TraceFactKind::HistoryMappingComplete,
            Self::Equivalence { .. } => TraceFactKind::Equivalence,
            Self::Coverage { .. } => TraceFactKind::Coverage,
            Self::Diagnostic { .. } => TraceFactKind::Diagnostic,
        }
    }

    /// One producer of subject indexes for provenance, correction and bounded reads.
    /// The relation's directly affected subject is first; related participants follow.
    pub fn subject_refs(&self) -> Vec<AnyObjectRef> {
        match self {
            Self::Declare { object } => vec![object.object_ref()],
            Self::AgentState { agent, .. } | Self::SelectHistory { agent, .. } => {
                vec![AnyObjectRef::Agent(agent.clone())]
            }
            Self::DeliveryStage { delivery, .. } => vec![AnyObjectRef::Delivery(delivery.clone())],
            Self::DeliveryRevisionObserved { delivery, revision } => vec![
                AnyObjectRef::Delivery(delivery.clone()),
                AnyObjectRef::MessageRevision(revision.clone()),
            ],
            Self::Execution { execution, .. } => vec![execution.object_ref()],
            Self::ResourceState { resource, .. } => vec![AnyObjectRef::Resource(resource.clone())],
            Self::Relation { relation } => relation.subject_refs(),
            Self::HistoryPosition { entry, .. } => vec![AnyObjectRef::HistoryEntry(entry.clone())],
            Self::HistoryContentObserved { observation } => {
                let mut subjects = vec![AnyObjectRef::HistoryEntry(observation.entry.clone())];
                subjects.extend(
                    observation
                        .slots
                        .iter()
                        .filter_map(|slot| match &slot.content {
                            HistoryContent::Message(reference) => {
                                Some(AnyObjectRef::MessageRevision(reference.clone()))
                            }
                            HistoryContent::Operation(reference) => {
                                Some(AnyObjectRef::Operation(reference.clone()))
                            }
                            HistoryContent::Checkpoint(reference) => {
                                Some(AnyObjectRef::ContextCheckpoint(reference.clone()))
                            }
                            HistoryContent::Opaque(_) => None,
                        }),
                );
                subjects
            }
            Self::HistoryMappingComplete { extent } => match &extent.original_owner {
                Some(OwnerRef::Agent { agent }) => vec![AnyObjectRef::Agent(agent.clone())],
                _ => vec![],
            },
            Self::Equivalence { left, right, .. } => vec![left.clone(), right.clone()],
            Self::Coverage { coverage } => vec![coverage.subject.clone()],
            Self::Diagnostic { diagnostic } => diagnostic.subject.iter().cloned().collect(),
        }
    }
}

impl Relation {
    pub fn subject_refs(&self) -> Vec<AnyObjectRef> {
        match self {
            Self::Spawn {
                parent,
                child,
                operation,
            } => std::iter::once(AnyObjectRef::Agent(child.clone()))
                .chain(std::iter::once(AnyObjectRef::Agent(parent.clone())))
                .chain(operation.iter().cloned().map(AnyObjectRef::Operation))
                .collect(),
            Self::Interaction {
                sender,
                recipient,
                delivery,
                operation,
            } => vec![
                AnyObjectRef::Agent(sender.clone()),
                AnyObjectRef::Agent(recipient.clone()),
            ]
            .into_iter()
            .chain(delivery.iter().cloned().map(AnyObjectRef::Delivery))
            .chain(operation.iter().cloned().map(AnyObjectRef::Operation))
            .collect(),
            Self::Trigger {
                delivery,
                activation,
            } => vec![
                AnyObjectRef::Activation(activation.clone()),
                AnyObjectRef::Delivery(delivery.clone()),
            ],
            Self::Nesting { enclosing, nested } => vec![
                AnyObjectRef::Operation(nested.clone()),
                AnyObjectRef::Operation(enclosing.clone()),
            ],
            Self::UsesResource {
                operation,
                resource,
            } => vec![
                AnyObjectRef::Operation(operation.clone()),
                AnyObjectRef::Resource(resource.clone()),
            ],
            Self::HistoryPredecessor { predecessor, entry }
            | Self::ReadingOrder {
                before: predecessor,
                after: entry,
            } => vec![
                AnyObjectRef::HistoryEntry(entry.clone()),
                AnyObjectRef::HistoryEntry(predecessor.clone()),
            ],
            Self::ContextLineage {
                recipient,
                checkpoint,
                original_owner,
            } => {
                let mut refs = vec![AnyObjectRef::Agent(recipient.clone())];
                if let Some(checkpoint) = checkpoint {
                    refs.push(AnyObjectRef::ContextCheckpoint(checkpoint.clone()));
                }
                if let OwnerRef::Agent { agent } = original_owner {
                    refs.push(AnyObjectRef::Agent(agent.clone()));
                }
                refs
            }
            Self::Representation {
                original,
                transformed,
                ..
            } => vec![
                AnyObjectRef::MessageRevision(transformed.clone()),
                AnyObjectRef::MessageRevision(original.clone()),
            ],
            Self::ContextMembership {
                checkpoint,
                selection,
                ..
            } => vec![
                AnyObjectRef::ContextCheckpoint(checkpoint.clone()),
                AnyObjectRef::HistorySelection(selection.clone()),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub claim_ref: ClaimRef,
    pub fact: Fact,
    pub evidence: Vec<ObservationRef>,
    pub rule_id: String,
    pub mapper_build: String,
    pub supersedes: Vec<ClaimRef>,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    #[error("a semantic claim must cite retained evidence")]
    MissingEvidence,
    #[error("a semantic claim must name its rule and mapper build")]
    MissingRule,
    #[error("a claim cannot supersede itself")]
    SelfCorrection,
    #[error("equivalence endpoints must have the same object kind")]
    DifferentKinds,
    #[error("a history selection extent must have ordered bounds and a mapping rule")]
    InvalidExtent,
    #[error(
        "a history content observation needs a source, rule and strictly ordered unique slots"
    )]
    InvalidHistoryContent,
    #[error("a monetary measurement must name its currency and pricing basis")]
    UnpricedCost,
    #[error("a cumulative sample must name its counter and reset domains")]
    MissingCounterDomain,
    #[error("byte ranges must have ordered bounds")]
    InvalidRange,
    #[error("a context summary cannot establish exact model context")]
    InexactContext,
    #[error("a coverage interval must use one source generation and ordered coordinates")]
    InvalidCoverageInterval,
}

impl Claim {
    /// Structural validation precedes semantic resolution. Cross-claim ownership,
    /// cycles and proof constraints are checked by the pure reducer at its fixed cut.
    pub fn validate(&self) -> Result<(), ClaimError> {
        if self.evidence.is_empty() {
            return Err(ClaimError::MissingEvidence);
        }
        if self.rule_id.trim().is_empty() || self.mapper_build.trim().is_empty() {
            return Err(ClaimError::MissingRule);
        }
        if self.supersedes.contains(&self.claim_ref) {
            return Err(ClaimError::SelfCorrection);
        }
        match &self.fact {
            Fact::HistoryContentObserved { observation } => {
                if observation.mapping_rule.trim().is_empty()
                    || observation.position.validate().is_err()
                    || observation
                        .slots
                        .windows(2)
                        .any(|slots| slots[0].slot >= slots[1].slot)
                {
                    return Err(ClaimError::InvalidHistoryContent);
                }
                Ok(())
            }
            Fact::HistoryMappingComplete { extent } => {
                if extent.start_inclusive >= extent.end_exclusive
                    || extent.mapping_rule.trim().is_empty()
                {
                    return Err(ClaimError::InvalidExtent);
                }
                extent
                    .source
                    .validate()
                    .map_err(|_| ClaimError::InvalidExtent)?;
                if let HistoryCoordinate::VendorOrdinal { domain_ref } = &extent.coordinate {
                    domain_ref
                        .validate()
                        .map_err(|_| ClaimError::InvalidExtent)?;
                }
                Ok(())
            }
            Fact::Equivalence { left, right, .. } if !left.same_kind(right) => {
                Err(ClaimError::DifferentKinds)
            }
            Fact::Declare {
                object: TraceObject::HistorySelection(selection),
            } => {
                if let SelectionMembership::SourceExtents { extents, .. } = &selection.membership {
                    for extent in extents {
                        if extent.start_inclusive > extent.end_exclusive
                            || extent.mapping_rule.trim().is_empty()
                        {
                            return Err(ClaimError::InvalidExtent);
                        }
                    }
                }
                Ok(())
            }
            Fact::Declare {
                object: TraceObject::Measurement(measurement),
            } => {
                if let Metric::Cost { pricing_basis } = &measurement.metric {
                    if pricing_basis.trim().is_empty()
                        || !matches!(&measurement.unit, MeasurementUnit::Currency { code } if !code.trim().is_empty())
                    {
                        return Err(ClaimError::UnpricedCost);
                    }
                }
                if let MeasurementMode::Cumulative {
                    counter_domain,
                    reset_epoch,
                    ..
                } = &measurement.mode
                {
                    if counter_domain
                        .as_ref()
                        .is_some_and(|value| value.trim().is_empty())
                        || reset_epoch
                            .as_ref()
                            .is_some_and(|value| value.trim().is_empty())
                    {
                        return Err(ClaimError::MissingCounterDomain);
                    }
                }
                Ok(())
            }
            Fact::DeliveryStage {
                stage: DeliveryStage::Included { coverage, .. },
                ..
            } => validate_ranges(coverage),
            Fact::Declare {
                object: TraceObject::ContextCheckpoint(checkpoint),
            } => {
                if matches!(checkpoint.coverage, InclusionCoverage::Exact)
                    && (checkpoint.basis == ContextBasis::Summary
                        || matches!(
                            checkpoint.content,
                            ContextContent::Summary { .. } | ContextContent::Unknown { .. }
                        ))
                {
                    return Err(ClaimError::InexactContext);
                }
                validate_ranges(&checkpoint.coverage)
            }
            Fact::Coverage { coverage } => {
                if let CoverageInterval::Source { first, through } = &coverage.interval {
                    if first.source != coverage.source
                        || first.source != through.source
                        || first.generation != through.generation
                        || !ordered_positions(&first.locator, &through.locator)
                    {
                        return Err(ClaimError::InvalidCoverageInterval);
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn ordered_positions(first: &SourceLocator, through: &SourceLocator) -> bool {
    match (first, through) {
        (
            SourceLocator::Jsonl {
                byte_start: a,
                record_ordinal: i,
                ..
            },
            SourceLocator::Jsonl {
                byte_start: b,
                record_ordinal: j,
                ..
            },
        ) => a <= b && i <= j,
        (SourceLocator::Frame { ordinal: a }, SourceLocator::Frame { ordinal: b }) => a <= b,
        // Opaque revision/occurrence keys establish a point, not a sortable range.
        (a, b) => a == b,
    }
}

fn validate_ranges(coverage: &InclusionCoverage) -> Result<(), ClaimError> {
    if let InclusionCoverage::ByteRanges { ranges, .. } = coverage {
        if ranges.iter().any(|range| range.start > range.end_exclusive) {
            return Err(ClaimError::InvalidRange);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentRef {
        AgentRef::Session(crate::SessionId::decode("env7fk2:codex:root").unwrap())
    }

    fn claim(fact: Fact) -> Claim {
        Claim {
            claim_ref: ClaimRef::native(agent(), "rules", vec!["claim-1".into()]).unwrap(),
            fact,
            evidence: vec![ObservationRef {
                issuer: Issuer::Environment(crate::EnvironmentId::parse("env7fk2").unwrap()),
                producer_id: "source-capture".into(),
                producer_epoch: "incarnation-1".into(),
                producer_seq: 1,
            }],
            rule_id: "fixture/explicit-native".into(),
            mapper_build: "fixture-1".into(),
            supersedes: vec![],
            source_revision: None,
        }
    }

    #[test]
    fn interpretations_without_retained_evidence_are_rejected() {
        let mut value = claim(Fact::AgentState {
            agent: agent(),
            state: AgentState::Exists,
        });
        assert_eq!(value.validate(), Ok(()));
        value.evidence.clear();
        assert_eq!(value.validate(), Err(ClaimError::MissingEvidence));
    }

    #[test]
    fn equivalence_cannot_turn_an_operation_into_an_agent() {
        let value = claim(Fact::Equivalence {
            left: AnyObjectRef::Agent(agent()),
            right: AnyObjectRef::Operation(
                OperationRef::native(agent(), "tool", vec!["spawn-1".into()]).unwrap(),
            ),
            rule: "invalid-tool-promotion".into(),
        });
        assert_eq!(value.validate(), Err(ClaimError::DifferentKinds));
    }

    #[test]
    fn a_correction_cannot_discard_its_own_claim() {
        let mut value = claim(Fact::AgentState {
            agent: agent(),
            state: AgentState::Exists,
        });
        value.supersedes.push(value.claim_ref.clone());
        assert_eq!(value.validate(), Err(ClaimError::SelfCorrection));
    }

    #[test]
    fn source_history_extents_require_units_order_and_mapping() {
        let source = SourceRef {
            issuer: Issuer::Environment(crate::EnvironmentId::parse("env7fk2").unwrap()),
            instance: "physical-file".into(),
        };
        let value = claim(Fact::Declare {
            object: TraceObject::HistorySelection(HistorySelection {
                selection_ref: SelectionRef::native(agent(), "history", vec!["selected".into()])
                    .unwrap(),
                agent: agent(),
                content_scope: HistoryContentScope::AllObservedAtCut,
                membership: SelectionMembership::SourceExtents {
                    extents: vec![SourceExtent {
                        source: source.clone(),
                        generation: 0,
                        coordinate: HistoryCoordinate::VendorOrdinal { domain_ref: source },
                        start_inclusive: 214,
                        end_exclusive: 20,
                        mapping_rule: "codex/vendor-ordinal".into(),
                        original_owner: None,
                    }],
                    inherited_selections: vec![],
                },
                excluded_heads: vec![],
            }),
        });
        assert_eq!(value.validate(), Err(ClaimError::InvalidExtent));
    }

    #[test]
    fn content_observations_refuse_ambiguous_slot_coordinates() {
        let entry = EntryRef::native(agent(), "entry", vec!["one".into()]).unwrap();
        let position = SourcePosition {
            source: SourceRef {
                issuer: Issuer::Server("fixture".into()),
                instance: "native".into(),
            },
            generation: 0,
            locator: SourceLocator::Frame { ordinal: 1 },
        };
        let slot = HistorySlot {
            slot: 0,
            content: HistoryContent::Opaque(crate::trace::PayloadRef::bytes("a".repeat(64)).unwrap()),
        };
        let observation = HistoryContentObservation {
            entry,
            position,
            mapping_rule: "native/entry".into(),
            slots: vec![slot.clone(), slot],
            complete: true,
        };
        let value = claim(Fact::HistoryContentObserved {
            observation: observation.clone(),
        });
        assert_eq!(value.validate(), Err(ClaimError::InvalidHistoryContent));
        let mut empty = observation;
        empty.slots.clear();
        assert!(claim(Fact::HistoryContentObserved {
            observation: empty.clone()
        })
        .validate()
        .is_ok());
        empty.mapping_rule.clear();
        assert_eq!(
            claim(Fact::HistoryContentObserved { observation: empty }).validate(),
            Err(ClaimError::InvalidHistoryContent)
        );
    }

    #[test]
    fn token_counts_cannot_be_published_as_unpriced_money() {
        let value = claim(Fact::Declare {
            object: TraceObject::Measurement(Measurement {
                model: None,
                measurement_ref: MeasurementRef::native(
                    agent(),
                    "usage",
                    vec!["response-1".into()],
                )
                .unwrap(),
                subject: AnyObjectRef::Agent(agent()),
                metric: Metric::Cost {
                    pricing_basis: "".into(),
                },
                unit: MeasurementUnit::Tokens,
                value: 30.into(),
                mode: MeasurementMode::Gauge,
                aggregation_scope: AggregationScope::NamedSubject,
                components: vec![],
            }),
        });
        assert_eq!(value.validate(), Err(ClaimError::UnpricedCost));
    }

    #[test]
    fn a_compaction_summary_cannot_claim_exact_model_context() {
        let value = claim(Fact::Declare {
            object: TraceObject::ContextCheckpoint(ContextCheckpoint {
                checkpoint_ref: CheckpointRef::native(agent(), "context", vec!["summary-1".into()])
                    .unwrap(),
                owner: OwnerRef::Agent { agent: agent() },
                basis: ContextBasis::Summary,
                content: ContextContent::Summary {
                    payload: super::super::PayloadRef::bytes("a".repeat(64)).unwrap(),
                },
                coverage: InclusionCoverage::Exact,
            }),
        });
        assert_eq!(value.validate(), Err(ClaimError::InexactContext));
    }

    #[test]
    fn coverage_cannot_cross_source_generations_or_reverse_positions() {
        let source = SourceRef {
            issuer: Issuer::Server("server".into()),
            instance: "live".into(),
        };
        let first = SourcePosition {
            source: source.clone(),
            generation: 1,
            locator: SourceLocator::Frame { ordinal: 4 },
        };
        for through in [
            SourcePosition {
                generation: 2,
                ..first.clone()
            },
            SourcePosition {
                locator: SourceLocator::Frame { ordinal: 3 },
                ..first.clone()
            },
            SourcePosition {
                source: SourceRef {
                    instance: "another-source".into(),
                    ..source.clone()
                },
                ..first.clone()
            },
        ] {
            let value = claim(Fact::Coverage {
                coverage: Coverage {
                    subject: AnyObjectRef::Agent(agent()),
                    source: source.clone(),
                    dimension: CoverageDimension::Messages,
                    interval: CoverageInterval::Source {
                        first: first.clone(),
                        through,
                    },
                    state: CoverageState::Complete,
                },
            });
            assert_eq!(value.validate(), Err(ClaimError::InvalidCoverageInterval));
        }
    }
}
