//! Bounded, vendor-independent reader DTOs consumed by every web reading surface.
//!
//! `docs/unified-trace-ui.md` owns interaction; the storage read protocol owns
//! authorization, snapshots, cursor scopes and byte/work bounds.

use serde::{Deserialize, Deserializer, Serialize};

use super::*;

/// Browser routes echo this opaque address; they never reconstruct native identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentAddress(String);
impl AgentAddress {
    pub fn of(agent: &AgentRef) -> Self {
        Self(agent.encode())
    }
    pub fn parse(value: impl Into<String>) -> Result<Self, TraceIdentityError> {
        let value = value.into();
        AgentRef::decode(&value)?;
        Ok(Self(value))
    }
    pub fn agent(&self) -> AgentRef {
        AgentRef::decode(&self.0).expect("validated agent address")
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for AgentAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// One object route handles all domain object kinds without a client-side alias map.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TraceAddress(String);
impl TraceAddress {
    pub fn of(object: &AnyObjectRef) -> Self {
        Self(object.encode())
    }
    pub fn parse(value: impl Into<String>) -> Result<Self, TraceIdentityError> {
        let value = value.into();
        AnyObjectRef::decode(&value)?;
        Ok(Self(value))
    }
    pub fn object(&self) -> AnyObjectRef {
        AnyObjectRef::decode(&self.0).expect("validated trace object address")
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AnyObjectRef {
    pub fn encode(&self) -> String {
        match self {
            Self::Agent(value) => value.encode(),
            Self::Activation(value) => value.encode(),
            Self::Message(value) => value.encode(),
            Self::MessageRevision(value) => value.encode(),
            Self::Delivery(value) => value.encode(),
            Self::Operation(value) => value.encode(),
            Self::Result(value) => value.encode(),
            Self::Resource(value) => value.encode(),
            Self::HistoryEntry(value) => value.encode(),
            Self::HistorySelection(value) => value.encode(),
            Self::ContextCheckpoint(value) => value.encode(),
            Self::Measurement(value) => value.encode(),
        }
    }
    pub fn decode(value: &str) -> Result<Self, TraceIdentityError> {
        let kind = match AgentRef::decode(value) {
            Ok(agent) => return Ok(Self::Agent(agent)),
            Err(TraceIdentityError::Kind { actual, .. }) => actual,
            Err(error) => return Err(error),
        };
        match kind.as_str() {
            "activation" => ActivationRef::decode(value).map(Self::Activation),
            "message" => MessageRef::decode(value).map(Self::Message),
            "message_revision" => MessageRevisionRef::decode(value).map(Self::MessageRevision),
            "delivery" => DeliveryRef::decode(value).map(Self::Delivery),
            "operation" => OperationRef::decode(value).map(Self::Operation),
            "result" => ResultRef::decode(value).map(Self::Result),
            "resource" => ResourceRef::decode(value).map(Self::Resource),
            "history_entry" => EntryRef::decode(value).map(Self::HistoryEntry),
            "history_selection" => SelectionRef::decode(value).map(Self::HistorySelection),
            "context_checkpoint" => CheckpointRef::decode(value).map(Self::ContextCheckpoint),
            "measurement" => MeasurementRef::decode(value).map(Self::Measurement),
            _ => Err(TraceIdentityError::Kind {
                expected: "a trace object".into(),
                actual: kind,
            }),
        }
    }
}
impl<'de> Deserialize<'de> for TraceAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadView {
    pub token: String,
    pub generation: String,
    pub evidence_cut: u64,
    pub view_revision: u64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadLimits {
    pub max_items: usize,
    pub max_summary_bytes: usize,
    pub max_preview_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_change_items: usize,
    pub max_mounted_rows: usize,
    pub max_metadata_cache_bytes: usize,
    pub max_payload_cache_bytes: usize,
    pub max_hidden_locations: usize,
    pub max_visible_interests: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadPageState {
    Complete,
    More,
    WorkBudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCount {
    pub value: Option<u64>,
    pub scope: String,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadPage<T> {
    pub view: ReadView,
    pub items: Vec<T>,
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    pub has_before: bool,
    pub has_after: bool,
    pub total: ReadCount,
    pub state: ReadPageState,
    pub returned_items: usize,
    pub returned_bytes: usize,
    pub limits: ReadLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLink {
    pub address: AgentAddress,
    pub name: Option<String>,
    pub identity_suffix: String,
    pub vendor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadAvailability {
    Available,
    Pending { reason: String },
    Partial { reason: String },
    Unavailable { reason: String },
    Redacted { reason: String },
    Opaque { reason: String },
    Conflicted { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCoverage {
    pub dimension: CoverageDimension,
    pub state: ReadAvailability,
    pub evidence: Option<TraceAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadExecutionState {
    Unavailable,
    ObservedActive {
        last_observed_at_ms: Option<i64>,
    },
    Terminated {
        outcome: TerminalOutcome,
        reason: Option<String>,
    },
    ObservationEnded {
        last_observed_at_ms: Option<i64>,
    },
    Conflicted {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadDurationBasis {
    SourceWall,
    SourceMonotonic,
    VendorReported,
    ServerObserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadDuration {
    pub value: serde_json::Number,
    pub unit: TimeUnit,
    pub basis: ReadDurationBasis,
    pub clock_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionSummary {
    pub reference: Option<TraceAddress>,
    pub state: ReadExecutionState,
    pub durations: Vec<ReadDuration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryLocation {
    pub selection: Option<SelectionRef>,
    pub label: String,
    pub is_current: bool,
    pub selection_known: bool,
    pub availability: ReadAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileContext {
    pub session_id: String,
    pub environment: String,
    pub workdir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentHeader {
    pub agent: AgentLink,
    pub environment: String,
    pub environment_name: Option<String>,
    pub model: Option<String>,
    pub own_execution: ExecutionSummary,
    pub history: HistoryLocation,
    pub parent: Option<AgentLink>,
    pub origin: Option<AgentLink>,
    pub coverage: Vec<ReadCoverage>,
    pub descendant_count: ReadCount,
    pub continuing_resources: ReadCount,
    pub required_input: ReadCount,
    pub file_context: Option<ReadFileContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadAuthor {
    Human {
        label: String,
    },
    Agent {
        agent: Option<AgentLink>,
        label: String,
    },
    Harness {
        label: String,
    },
    Tool {
        operation: Option<TraceAddress>,
        label: String,
    },
    External {
        label: String,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PayloadAccess {
    pub payload_ref: PayloadRef,
    pub body_revision: String,
    pub representation: Representation,
    pub via_ref: TraceAddress,
    pub containing_agent: AgentAddress,
    pub selection: Option<SelectionRef>,
    pub media_type: Option<String>,
    pub encoding: PayloadEncoding,
    pub total_bytes: Option<u64>,
    pub available_ranges: Vec<PayloadRange>,
    pub availability: ReadAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartSummary {
    pub part_ref: MessagePartRef,
    pub kind: ContentKind,
    pub preview: String,
    pub preview_truncated: bool,
    pub payload: PayloadAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadDelivery {
    pub reference: TraceAddress,
    pub sender: ReadAuthor,
    pub stages: Vec<DeliveryStage>,
    pub availability: ReadAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOrderBasis {
    NativePredecessor,
    ExplicitOrder,
    PresentationTiebreak,
}

/// Unit addresses are presentation addresses anchored in actual objects, never
/// new Session/Activation identity. The server keeps them stable across regrouping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadingUnit {
    pub address: String,
    pub anchor: TraceAddress,
    pub order_basis: ReadOrderBasis,
    pub source_surface: String,
    pub activation: Option<ExecutionSummary>,
    pub content: ReadingContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadingContent {
    Message {
        message_ref: MessageRef,
        revision_ref: MessageRevisionRef,
        author: ReadAuthor,
        purpose: MessagePurpose,
        delivery: Option<ReadDelivery>,
        parts: Vec<PartSummary>,
        parts_total: usize,
        parts_complete: bool,
        parts_cursor: Option<String>,
        original_owner: Option<AgentLink>,
    },
    Work {
        owner: Option<AgentLink>,
        operations: ReadCount,
        agents: ReadCount,
        assignments: ReadCount,
        failed_operations: ReadCount,
        active_resources: ReadCount,
        required_input: ReadCount,
        operations_anchor: TraceAddress,
    },
    ActivationBoundary {
        execution: ExecutionSummary,
        trigger_deliveries: Vec<TraceAddress>,
        trigger_count: ReadCount,
    },
    ContextBoundary {
        checkpoint: TraceAddress,
        label: String,
        summary: Option<PartSummary>,
    },
    HistoryBoundary {
        selection: SelectionRef,
        label: String,
    },
    Unknown {
        label: String,
        reason: String,
        payloads: Vec<PayloadAccess>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceProgress {
    pub evidence_cut: u64,
    pub projection_cut: u64,
    pub search_cut: Option<u64>,
    pub pending_outbox: u64,
    pub oldest_outbox_age_ms: Option<u64>,
    pub source_connected: bool,
    pub fault: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadControlTarget {
    Session {
        agent: AgentAddress,
        session_id: String,
    },
    Embedded {
        agent: AgentAddress,
        host_session_id: String,
        namespace: String,
        native_child_id: String,
        route: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadControlAction {
    Send,
    Steer,
    Queue,
    NativeSteer,
    NativeQueue,
    Stop,
    Interrupt,
    Approval,
    Configure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadControlAvailability {
    Available {
        target: ReadControlTarget,
        actions: Vec<ReadControlAction>,
        lease: Option<String>,
        affected_execution: Option<TraceAddress>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReaderBootstrap {
    pub view: ReadView,
    pub header: AgentHeader,
    pub window: ReadPage<ReadingUnit>,
    pub change_cursor: String,
    pub progress: TraceProgress,
    pub control: ReadControlAvailability,
    pub authorization_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDirectoryRow {
    pub agent: AgentLink,
    pub relation: String,
    pub own_execution: ExecutionSummary,
    pub activations: ReadCount,
    pub required_input: ReadCount,
    pub coverage: Vec<ReadCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryChoice {
    pub selection: SelectionRef,
    pub label: String,
    pub is_current: bool,
    pub availability: ReadAvailability,
    pub entry_count: ReadCount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadResultOccurrence {
    pub reference: TraceAddress,
    pub disposition: ResultDisposition,
    pub representation: Representation,
    pub label: String,
    pub payloads: Vec<PayloadAccess>,
    pub availability: ReadAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRelation {
    pub target: TraceAddress,
    pub containing_agent: AgentAddress,
    pub label: String,
    pub agent: Option<AgentLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadResourceState {
    Created,
    Running,
    Yielded,
    Closed {
        outcome: TerminalOutcome,
        reason: Option<String>,
    },
    GoalUpdated {
        status: String,
    },
    ScheduleRegistered,
    ScheduleCancelled,
    ScheduleFired {
        firing_key: Vec<String>,
        delivery: Option<TraceAddress>,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadResourceObservation {
    pub claim_ref: ClaimRef,
    pub source: Option<SourcePosition>,
    pub order_basis: ReadOrderBasis,
    pub state: ReadResourceState,
    pub payloads: Vec<PayloadAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadRevisionForm {
    Complete,
    Append { predecessor: Option<TraceAddress> },
    Correction { replaces: TraceAddress },
    Replacement,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReadObjectContent {
    Operation {
        category: OperationCategory,
        input: Vec<PayloadAccess>,
        results: Vec<ReadResultOccurrence>,
        results_cursor: Option<String>,
        resources: Vec<ReadRelation>,
        resources_cursor: Option<String>,
    },
    Resource {
        resource_kind: ResourceKind,
        observations: Vec<ReadResourceObservation>,
        observations_cursor: Option<String>,
    },
    Revision {
        representation: Representation,
        form: ReadRevisionForm,
        payloads: Vec<PayloadAccess>,
        payloads_cursor: Option<String>,
    },
    Context {
        basis: ContextBasis,
        coverage: InclusionCoverage,
        memberships: Vec<ReadRelation>,
        memberships_cursor: Option<String>,
        payloads: Vec<PayloadAccess>,
    },
    Measurement {
        metric: Metric,
        model: Option<MeasurementModel>,
        unit: MeasurementUnit,
        value: serde_json::Number,
        scope: String,
        mode: MeasurementMode,
    },
    Generic {
        label: String,
        payloads: Vec<PayloadAccess>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadObjectDetail {
    pub view: ReadView,
    pub reference: TraceAddress,
    pub title: String,
    pub owner: Option<AgentLink>,
    pub execution: Option<ExecutionSummary>,
    pub availability: ReadAvailability,
    pub content: ReadObjectContent,
    pub relations: Vec<ReadRelation>,
    pub relations_cursor: Option<String>,
    pub coverage: Vec<ReadCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadSearchHit {
    pub anchor: TraceAddress,
    pub agent: AgentLink,
    pub history: HistoryLocation,
    pub excerpt: String,
    pub payload: Option<PayloadAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TraceChange {
    Revision {
        cursor: String,
        generation: String,
        evidence_cut:u64,
        view_revision:u64,
        agents: Vec<AgentAddress>,
        changed: Vec<TraceAddress>,
        removed: Vec<TraceAddress>,
        moved: Vec<TraceAddress>,
    },
    ResetRequired {
        reason: String,
    },
    AccessChanged {
        authorization_scope: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCommandRequest {
    pub target: ReadControlTarget,
    pub action: ReadControlAction,
    pub lease: Option<String>,
    pub affected_execution: Option<TraceAddress>,
    pub idempotency_key: String,
    pub text: Option<String>,
    pub attachments: Vec<ReadAttachment>,
    pub config: Option<ReadConfigChange>,
    pub answer: Option<ReadRequestAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadAttachment {
    pub name: String,
    pub r#type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadConfigChange {
    pub id: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRequestAnswer {
    pub request_id: String,
    pub option_id: String,
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadCommandReceipt {
    pub command_id: String,
    pub observation_ref: ObservationRef,
    pub state: ReadCommandState,
    pub revision: u64,
    pub text: Option<String>,
    pub error: Option<String>,
    pub editable: bool,
    pub cancellable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadCommandState {
    Accepted,
    Queued,
    ReceiverAccepted,
    Admitted,
    Unconfirmed,
    Rejected,
    Cancelled,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_addresses_preserve_embedded_scope_and_reject_cross_kind_routes() {
        let agent = AgentRef::Embedded {
            anchor_session: crate::SessionId::decode("env:claude:root").unwrap(),
            namespace: "claude.agent".into(),
            native_child_id: "Child:大小写".into(),
        };
        let address = AgentAddress::of(&agent);
        assert_eq!(
            serde_json::to_value(&address).unwrap(),
            serde_json::Value::String(agent.encode())
        );
        assert_eq!(
            AgentAddress::parse(address.as_str()).unwrap().agent(),
            agent
        );
        let object = AnyObjectRef::Operation(
            OperationRef::native(agent, "tool", vec!["call-a".into()]).unwrap(),
        );
        let address = TraceAddress::of(&object);
        assert_eq!(
            TraceAddress::parse(address.as_str()).unwrap().object(),
            object
        );
        assert!(AgentAddress::parse(address.as_str()).is_err());
    }
    #[test]
    fn reader_states_serialize_without_fabricating_execution_or_control() {
        assert_eq!(
            serde_json::to_string(&ReadExecutionState::ObservationEnded {
                last_observed_at_ms: Some(42)
            })
            .unwrap(),
            r#"{"kind":"observation_ended","lastObservedAtMs":42}"#
        );
        let unavailable: ReadControlAvailability = serde_json::from_str(
            r#"{"kind":"unavailable","reason":"Embedded child has no independent control route"}"#,
        )
        .unwrap();
        assert!(matches!(
            unavailable,
            ReadControlAvailability::Unavailable { .. }
        ));
        assert!(serde_json::from_str::<ReadControlAvailability>(r#"{"kind":"available","target":{"kind":"session","sessionId":"root"},"actions":["send"],"lease":null,"affectedExecution":null}"#).is_err());
    }
}
