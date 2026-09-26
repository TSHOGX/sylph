//! Closed domain objects. Identity, authorship, membership and execution are separate.

use serde::{Deserialize, Serialize};

use super::{
    capture::{PayloadEncoding, PayloadRef, PayloadUse, TimeEvidence},
    identity::*,
};

/// Absent or unresolved ownership never creates an agent or assigns work to the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OwnerRef {
    /// The evidence did not name an owner; the object namespace is not an actor.
    Unknown,
    Agent {
        agent: AgentRef,
    },
    Unresolved {
        scope: AgentRef,
        namespace: String,
        native_key: Vec<String>,
        reason: String,
    },
}

/// A typed address accepted by object reads and evidence relations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum AnyObjectRef {
    Agent(AgentRef),
    Activation(ActivationRef),
    Message(MessageRef),
    MessageRevision(MessageRevisionRef),
    Delivery(DeliveryRef),
    Operation(OperationRef),
    Result(ResultRef),
    Resource(ResourceRef),
    HistoryEntry(EntryRef),
    HistorySelection(SelectionRef),
    ContextCheckpoint(CheckpointRef),
    Measurement(MeasurementRef),
}

impl AnyObjectRef {
    /// Equivalence is only meaningful within one domain object kind.
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub fn scope(&self) -> &AgentRef {
        match self {
            Self::Agent(v) => v,
            Self::Activation(v) => v.scope(),
            Self::Message(v) => v.scope(),
            Self::MessageRevision(v) => v.scope(),
            Self::Delivery(v) => v.scope(),
            Self::Operation(v) => v.scope(),
            Self::Result(v) => v.scope(),
            Self::Resource(v) => v.scope(),
            Self::HistoryEntry(v) => v.scope(),
            Self::HistorySelection(v) => v.scope(),
            Self::ContextCheckpoint(v) => v.scope(),
            Self::Measurement(v) => v.scope(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentThread {
    pub agent_ref: AgentRef,
    pub vendor: crate::AgentName,
    pub name: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activation {
    pub activation_ref: ActivationRef,
    pub owner: OwnerRef,
}

/// Protocol role never supplies the author of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Author {
    Human { subject: Option<String> },
    Agent { agent: OwnerRef },
    Harness { identity: Option<String> },
    Tool { operation: Option<OperationRef> },
    External { identity: Option<String> },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePurpose {
    TaskInput,
    Progress,
    Answer,
    Context,
    Notice,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductionScope {
    pub owner: OwnerRef,
    pub activation: Option<ActivationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_ref: MessageRef,
    pub author: Author,
    pub protocol_role: Option<ProtocolRole>,
    pub purpose: MessagePurpose,
    pub production: Option<ProductionScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    DisclosedReasoning,
    Image,
    Audio,
    Video,
    Structured,
    Attachment,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentPart {
    pub part_ref: MessagePartRef,
    pub kind: ContentKind,
    pub payload: PayloadRef,
    pub media_type: Option<String>,
    pub encoding: PayloadEncoding,
}

/// Context in which immutable payload bytes were observed or submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    Runtime,
    CallerResult,
    Conversation,
    Submitted,
}

/// Result occurrences have output visibility; submitted input is a message revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultRepresentation {
    Runtime,
    CallerResult,
    Conversation,
}

impl From<ResultRepresentation> for Representation {
    fn from(value: ResultRepresentation) -> Self {
        match value {
            ResultRepresentation::Runtime => Self::Runtime,
            ResultRepresentation::CallerResult => Self::CallerResult,
            ResultRepresentation::Conversation => Self::Conversation,
        }
    }
}

/// A source prefix is explicit; absence does not imply a snapshot equals prior deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePrefix {
    pub through: SourcePosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RevisionForm {
    Complete,
    Append {
        predecessor: Option<MessageRevisionRef>,
    },
    Replacement {
        covers: Option<SourcePrefix>,
    },
    Final {
        covers: Option<SourcePrefix>,
    },
    Correction {
        replaces: MessageRevisionRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRevision {
    pub revision_ref: MessageRevisionRef,
    pub message: MessageRef,
    pub representation: Representation,
    pub form: RevisionForm,
    /// Array order is source part order. Empty complete content is a valid revision.
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Recipient {
    Agent { agent: OwnerRef },
    Operation { operation: OperationRef },
    Human { subject: Option<String> },
    External { identity: Option<String> },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeliveryRoute {
    ServerCommand {
        command_id: String,
    },
    VendorMailbox {
        namespace: String,
        occurrence: Vec<String>,
    },
    NativeHistory,
    ToolReturn {
        operation: OperationRef,
    },
    External {
        namespace: String,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delivery {
    pub delivery_ref: DeliveryRef,
    pub sender: Author,
    pub recipient: Recipient,
    pub route: DeliveryRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum InclusionTarget {
    Inference { operation: OperationRef },
    Checkpoint { checkpoint: CheckpointRef },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum InclusionCoverage {
    Exact,
    Parts {
        parts: Vec<MessagePartRef>,
    },
    ByteRanges {
        payload: PayloadRef,
        ranges: Vec<ByteRange>,
    },
    Summary,
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub start: u64,
    pub end_exclusive: u64,
}

/// Stages accumulate independently; their declaration order is not a state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeliveryStage {
    Requested,
    Accepted,
    Queued,
    Admitted {
        entry: Option<EntryRef>,
    },
    Included {
        target: InclusionTarget,
        coverage: InclusionCoverage,
    },
    Rejected {
        reason: String,
    },
    Cancelled {
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum RequesterRef {
    Agent(AgentRef),
    Activation(ActivationRef),
    Operation(OperationRef),
    Delivery(DeliveryRef),
    Resource(ResourceRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAction {
    Launch,
    Poll,
    Write,
    Resize,
    Close,
}

/// Categories express vendor-proven behavior. A tool name alone cannot select delegation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OperationCategory {
    Tool {
        namespace: Option<String>,
        name: String,
    },
    Inference {
        model: Option<String>,
    },
    Delegation,
    Communication,
    Wait,
    TerminalIo {
        action: TerminalAction,
    },
    CodeCell {
        language: Option<String>,
    },
    Edit,
    Extension {
        namespace: String,
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentState {
    Collecting,
    Sealed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation_ref: OperationRef,
    pub owner: OwnerRef,
    pub activation: Option<ActivationRef>,
    pub requester: Option<RequesterRef>,
    pub category: OperationCategory,
    pub input_revision: Option<PayloadUse>,
    pub argument_state: ArgumentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultDisposition {
    Progress,
    Yield,
    FinalReturn,
    Refusal,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultOccurrence {
    pub result_ref: ResultRef,
    /// This can name an invocation whose arguments have never been observed.
    pub operation: OperationRef,
    pub recipient: Option<Recipient>,
    pub disposition: ResultDisposition,
    pub representation: ResultRepresentation,
    pub payload: Option<PayloadUse>,
    pub delivery: Option<DeliveryRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ResourceKind {
    Terminal {
        native_handle: String,
        incarnation: String,
    },
    CodeCell {
        native_handle: String,
        incarnation: String,
    },
    Goal,
    Schedule,
    Artifact {
        media_type: Option<String>,
    },
    Extension {
        namespace: String,
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub resource_ref: ResourceRef,
    pub creator: Option<OperationRef>,
    pub host: Option<OwnerRef>,
    pub kind: ResourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ExecutionCloseReason {
    VendorStop,
    BudgetExhausted,
    ServerInterrupt,
    ProcessExit {
        code: Option<i32>,
        signal: Option<String>,
    },
    ObservationLoss,
    GrantRevoked,
    Extension {
        namespace: String,
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum ExecutionRef {
    Activation(ActivationRef),
    Operation(OperationRef),
    Resource(ResourceRef),
}

impl ExecutionRef {
    pub fn object_ref(&self) -> AnyObjectRef {
        match self {
            Self::Activation(v) => AnyObjectRef::Activation(v.clone()),
            Self::Operation(v) => AnyObjectRef::Operation(v.clone()),
            Self::Resource(v) => AnyObjectRef::Resource(v.clone()),
        }
    }
}

/// Request, termination and vendor outcome are different evidence families.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ExecutionBoundary {
    Started {
        time: Option<TimeEvidence>,
    },
    Terminated {
        time: Option<TimeEvidence>,
        reason: Option<ExecutionCloseReason>,
    },
    Outcome {
        outcome: TerminalOutcome,
        reason: Option<ExecutionCloseReason>,
        detail: Option<PayloadRef>,
    },
    InterruptRequested {
        command_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ResourceState {
    Created,
    Running,
    Yielded,
    Closed {
        outcome: TerminalOutcome,
        reason: Option<ExecutionCloseReason>,
    },
    GoalUpdated {
        status: String,
        detail: Option<PayloadRef>,
    },
    ScheduleRegistered {
        registration: PayloadRef,
    },
    ScheduleCancelled,
    ScheduleFired {
        firing_key: Vec<String>,
        delivery: Option<DeliveryRef>,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum HistoryContent {
    Message(MessageRevisionRef),
    Operation(OperationRef),
    Checkpoint(CheckpointRef),
    Opaque(PayloadRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryEntry {
    pub entry_ref: EntryRef,
    pub containing_agent: AgentRef,
    pub predecessor: Option<EntryRef>,
    pub original_owner: Option<OwnerRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySlot {
    pub slot: u32,
    pub content: HistoryContent,
}

/// One immutable source observation of an entry's ordered content. Topology does
/// not change when another source observes an additional revision in a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryContentObservation {
    pub entry: EntryRef,
    pub position: SourcePosition,
    pub mapping_rule: String,
    pub slots: Vec<HistorySlot>,
    pub complete: bool,
}

impl HistoryContentObservation {
    pub fn complete(
        entry: EntryRef,
        position: SourcePosition,
        mapping_rule: impl Into<String>,
        contents: Vec<HistoryContent>,
    ) -> Self {
        Self {
            entry,
            position,
            mapping_rule: mapping_rule.into(),
            slots: contents
                .into_iter()
                .enumerate()
                .map(|(slot, content)| HistorySlot {
                    slot: u32::try_from(slot).expect("bounded history content slots"),
                    content,
                })
                .collect(),
            complete: true,
        }
    }
}

/// Coordinates retain their domain; a recorded vendor ordinal is never a line number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HistoryCoordinate {
    VendorOrdinal { domain_ref: SourceRef },
    LocalRecordIndex,
    ByteRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceExtent {
    pub source: SourceRef,
    pub generation: u64,
    pub coordinate: HistoryCoordinate,
    pub start_inclusive: u64,
    pub end_exclusive: u64,
    pub mapping_rule: String,
    pub original_owner: Option<OwnerRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SelectionMembership {
    EntryPath {
        head: EntryRef,
    },
    EntrySet {
        entries: Vec<EntryRef>,
    },
    SourceExtents {
        extents: Vec<SourceExtent>,
        inherited_selections: Vec<SelectionRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySelection {
    pub selection_ref: SelectionRef,
    pub agent: AgentRef,
    pub membership: SelectionMembership,
    pub content_scope: HistoryContentScope,
    pub excluded_heads: Vec<EntryRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HistoryContentScope {
    /// No source selected one content observation. Alternatives remain visible
    /// at the query cut; this does not choose the latest captured revision.
    AllObservedAtCut,
    ObservationSet {
        observations: Vec<ClaimRef>,
    },
    /// One verified snapshot response, optionally extending an earlier snapshot
    /// from the same source generation. Its content is independent of its head.
    Snapshot {
        position: SourcePosition,
        mapping_rule: String,
        previous: Option<SelectionRef>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBasis {
    ObservedRequest,
    NativeReplacement,
    InheritedMembership,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ContextContent {
    Ordered {
        entries: Vec<HistoryContent>,
        settings: Option<PayloadRef>,
    },
    Selection {
        selection: SelectionRef,
    },
    OpaqueReplacement {
        payload: PayloadRef,
    },
    Summary {
        payload: PayloadRef,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCheckpoint {
    pub checkpoint_ref: CheckpointRef,
    pub owner: OwnerRef,
    pub basis: ContextBasis,
    pub content: ContextContent,
    pub coverage: InclusionCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationScope {
    SelectedHistory,
    LocalAuthoredWork,
    RetainedArchive,
    NamedSubject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Metric {
    InputTokens,
    OutputTokens,
    CachedTokens,
    ReasoningTokens,
    TotalTokens,
    WallElapsed,
    ActiveDuration,
    ReportedDuration,
    Cost { pricing_basis: String },
    Extension { namespace: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MeasurementUnit {
    Tokens,
    Seconds,
    Milliseconds,
    Nanoseconds,
    Bytes,
    Count,
    Currency { code: String },
    Extension { namespace: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MeasurementMode {
    Interval {
        interval_ref: AnyObjectRef,
    },
    Cumulative {
        counter_domain: Option<String>,
        reset_epoch: Option<String>,
        sample_position: SourcePosition,
    },
    Gauge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentRelationship {
    Included,
    Disjoint,
    Overlapping,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementComponent {
    pub measurement: MeasurementRef,
    pub relationship: ComponentRelationship,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeasurementModel {
    pub reported_name: String,
    pub canonical_name: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    pub measurement_ref: MeasurementRef,
    pub subject: AnyObjectRef,
    pub model: Option<MeasurementModel>,
    pub metric: Metric,
    pub unit: MeasurementUnit,
    pub value: serde_json::Number,
    pub mode: MeasurementMode,
    pub aggregation_scope: AggregationScope,
    pub components: Vec<MeasurementComponent>,
}

/// Core objects have typed bodies. Extra vendor fields remain in retained captures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "object", rename_all = "snake_case")]
pub enum TraceObject {
    Agent(AgentThread),
    Activation(Activation),
    Message(Message),
    MessageRevision(MessageRevision),
    Delivery(Delivery),
    Operation(Operation),
    Result(ResultOccurrence),
    Resource(Resource),
    HistoryEntry(HistoryEntry),
    HistorySelection(HistorySelection),
    ContextCheckpoint(ContextCheckpoint),
    Measurement(Measurement),
}

impl TraceObject {
    pub fn payload_refs(&self) -> Vec<&PayloadRef> {
        match self {
            Self::MessageRevision(value) => value.parts.iter().map(|part| &part.payload).collect(),
            Self::Operation(value) => value
                .input_revision
                .iter()
                .map(|payload| &payload.reference)
                .collect(),
            Self::Result(value) => value
                .payload
                .iter()
                .map(|payload| &payload.reference)
                .collect(),
            Self::ContextCheckpoint(value) => {
                let mut refs = match &value.content {
                    ContextContent::Ordered { settings, .. } => settings.iter().collect(),
                    ContextContent::OpaqueReplacement { payload }
                    | ContextContent::Summary { payload } => vec![payload],
                    _ => vec![],
                };
                if let InclusionCoverage::ByteRanges { payload, .. } = &value.coverage {
                    refs.push(payload);
                }
                refs
            }
            _ => vec![],
        }
    }

    pub fn object_ref(&self) -> AnyObjectRef {
        match self {
            Self::Agent(v) => AnyObjectRef::Agent(v.agent_ref.clone()),
            Self::Activation(v) => AnyObjectRef::Activation(v.activation_ref.clone()),
            Self::Message(v) => AnyObjectRef::Message(v.message_ref.clone()),
            Self::MessageRevision(v) => AnyObjectRef::MessageRevision(v.revision_ref.clone()),
            Self::Delivery(v) => AnyObjectRef::Delivery(v.delivery_ref.clone()),
            Self::Operation(v) => AnyObjectRef::Operation(v.operation_ref.clone()),
            Self::Result(v) => AnyObjectRef::Result(v.result_ref.clone()),
            Self::Resource(v) => AnyObjectRef::Resource(v.resource_ref.clone()),
            Self::HistoryEntry(v) => AnyObjectRef::HistoryEntry(v.entry_ref.clone()),
            Self::HistorySelection(v) => AnyObjectRef::HistorySelection(v.selection_ref.clone()),
            Self::ContextCheckpoint(v) => AnyObjectRef::ContextCheckpoint(v.checkpoint_ref.clone()),
            Self::Measurement(v) => AnyObjectRef::Measurement(v.measurement_ref.clone()),
        }
    }
}
