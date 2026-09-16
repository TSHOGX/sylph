//! Per-vendor interpretation with explicit checkpointed source state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;

pub mod claude;
pub mod codex;
pub mod inventory;
pub mod attachment;
pub mod observation;
pub mod pi;
pub mod server;

/// The worker resolves these requests from the same retained evidence cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ArchiveRequest {
    ContinueHistory,
    HistoryLocator { issuer: Issuer, physical_id: String },
    HistorySource { source:SourceRef, generation:u64 },
    CapturedPayload {parent:ObservationRef,slot:String},
    AttachmentTarget {parent:ObservationRef,slot:String,locator:String},
    Object {
        object_ref: AnyObjectRef,
    },
    Observation {
        observation: ObservationRef,
    },
    CommandIdentity {
        issuer: Issuer,
        command_id: String,
    },
    Source {
        source: SourceRef,
        generation: u64,
    },
    Agent {
        agent: AgentRef,
    },
    Correlation {
        agent: AgentRef,
        namespace: String,
        key: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedPayload {
    pub reference: PayloadRef,
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub encoding: PayloadEncoding,
    pub evidence: Vec<ObservationRef>,
    /// A stable source selector and decoding rule, not a claim of original lexical bytes.
    pub selector: String,
    pub rule: String,
    pub manifest: Option<PayloadManifest>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapperState {
    /// State is isolated by source/generation and mapper build by the worker.
    pub governing_agent: Option<AgentRef>,
    pub current_activation: Option<ActivationRef>,
    pub previous_entry: Option<EntryRef>,
    pub values: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
pub struct CapturedPayloadBinding {
    pub parent:ObservationRef,
    pub slot:String,
    pub attachment:CaptureAttachment,
    pub evidence:Vec<ObservationRef>,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct CapturedPayload {pub bindings:Vec<CapturedPayloadBinding>,pub bytes:Option<Vec<u8>>,pub limited:bool}
pub fn captured_payload_key(parent:&ObservationRef,slot:&str)->String {serde_json::to_string(&(parent,slot)).expect("typed captured payload key")}

/// Only evidence-proved inventory and correlation data may populate this context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveContext {
    pub captured_payloads:BTreeMap<String,CapturedPayload>,
    pub agents: BTreeMap<AgentRef, AgentThread>,
    pub correlations: BTreeMap<String, AnyObjectRef>,
    pub source_prefixes: BTreeMap<SourceRef, Vec<SourceExtent>>,
    pub source_definitions: BTreeMap<(SourceRef,u64), codex::history::RecordedSource>,
    pub history_records: codex::history::HistoryRecords,
    pub native_history_positions: BTreeMap<EntryRef, SourcePosition>,
    pub observations: std::collections::BTreeSet<ObservationRef>,
    pub correlation_evidence: BTreeMap<String, Vec<ObservationRef>>,
    pub history_entries: BTreeMap<EntryRef, HistoryEntry>,
    pub operations: BTreeMap<OperationRef, Operation>,
    pub source_owners: BTreeMap<(SourceRef, u64), SourceOwner>,
    pub object_evidence: BTreeMap<AnyObjectRef, Vec<ObservationRef>>,
    pub observation_sources: BTreeMap<ObservationRef, SourcePosition>,
    pub result_operations:BTreeMap<ResultRef,OperationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOwner {
    pub agent: AgentRef,
    pub evidence: Vec<ObservationRef>,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOwnership {
    pub source: SourceRef,
    pub generation: u64,
    pub owner: SourceOwner,
}

pub struct MapInput<'a> {
    pub capture: &'a Capture,
    /// A reread receipt resolves to the first retained observation of the same occurrence.
    pub canonical_observation: &'a ObservationRef,
    pub raw: &'a [u8],
    pub state: &'a MapperState,
    pub archive: &'a ArchiveContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapOutput {
    pub capture_payload_bindings:Vec<CapturedPayloadBinding>,
    pub claims: Vec<Claim>,
    pub payloads: Vec<DerivedPayload>,
    pub state: MapperState,
    pub requests: Vec<ArchiveRequest>,
    pub diagnostics: Vec<MappingDiagnostic>,
    pub correlations: BTreeMap<String, AnyObjectRef>,
    pub correlation_evidence: BTreeMap<String, Vec<ObservationRef>>,
    pub bound_agents: Vec<AgentRef>,
    pub source_owners: Vec<SourceOwnership>,
    pub source_definitions: Vec<codex::history::RecordedSource>,
    pub history_records: Vec<codex::history::HistoryRecord>,
    pub continuation_state: Option<MapperState>,
}

/// A source without an evidenced agent can still report why mapping is unavailable.
/// Its diagnostic does not require inventing a Session or object identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingDiagnostic {
    pub evidence: Vec<ObservationRef>,
    pub code: DiagnosticCode,
    pub message: String,
}

pub fn correlation_key(agent: &AgentRef, namespace: &str, key: &[String]) -> String {
    serde_json::to_string(&(agent, namespace, key)).expect("typed correlation key serialization")
}

pub fn attachment_key(parent:&ObservationRef,slot:&str,locator:&str)->String {serde_json::to_string(&("source-attachment",parent,slot,locator)).expect("typed attachment relation key")}

pub fn command_identity_key(issuer: &Issuer, command_id: &str) -> String {
    serde_json::to_string(&("command_identity", issuer, command_id))
        .expect("typed command identity serialization")
}

pub const DERIVED_PAYLOAD_CHUNK_BYTES: usize = 1024 * 1024;

/// Malformed/unsupported vendor data returns diagnostic claims rather than a silent
/// empty parse. Envelope/storage failure remains the worker's separate admission error.
pub trait SemanticMapper: Send + Sync {
    fn build(&self) -> &'static str;
    fn map(&self, input: MapInput<'_>) -> MapOutput;
}

pub fn derived_payload(
    input: &MapInput<'_>,
    bytes: Vec<u8>,
    selector: impl Into<String>,
    rule: impl Into<String>,
) -> DerivedPayload {
    derived_payload_with_media(
        input,
        bytes,
        selector,
        rule,
        "application/octet-stream",
        PayloadEncoding::Binary,
    )
}

pub fn derived_payload_with_media(
    input: &MapInput<'_>,
    bytes: Vec<u8>,
    selector: impl Into<String>,
    rule: impl Into<String>,
    media_type: impl Into<String>,
    encoding: PayloadEncoding,
) -> DerivedPayload {
    let media_type = media_type.into();
    let manifest = (bytes.len() > DERIVED_PAYLOAD_CHUNK_BYTES).then(|| PayloadManifest {
        chunks: bytes
            .chunks(DERIVED_PAYLOAD_CHUNK_BYTES)
            .enumerate()
            .map(|(index, bytes)| PayloadChunk {
                digest: PayloadRef::bytes(blake3::hash(bytes).to_hex()).expect("BLAKE3 digest"),
                chunk_offset: 0,
                length: bytes.len() as u64,
                payload_offset: (index * DERIVED_PAYLOAD_CHUNK_BYTES) as u64,
            })
            .collect(),
        media_type: media_type.clone(),
        encoding,
        decoded_length: Some(bytes.len() as u64),
        completeness: PayloadCompleteness::Complete,
    });
    let reference = match &manifest {
        Some(manifest) => PayloadRef::manifest(
            blake3::hash(
                &manifest
                    .canonical_bytes()
                    .expect("complete derived payload manifest"),
            )
            .to_hex(),
        )
        .expect("BLAKE3 digest"),
        None => PayloadRef::bytes(blake3::hash(&bytes).to_hex()).expect("BLAKE3 digest"),
    };
    DerivedPayload {
        reference,
        bytes,
        media_type,
        encoding,
        evidence: vec![input.canonical_observation.clone()],
        selector: selector.into(),
        rule: rule.into(),
        manifest,
    }
}

pub fn claim(input: &MapInput<'_>, scope: &AgentRef, slot: &str, build: &str, fact: Fact) -> Claim {
    Claim {
        claim_ref: ClaimRef::occurrence(
            scope.clone(),
            input.capture.source_position.clone(),
            vec![build.into(), slot.into()],
        )
        .expect("validated capture position and nonempty mapper selector"),
        fact,
        evidence: vec![input.canonical_observation.clone()],
        rule_id: slot.into(),
        mapper_build: build.into(),
        supersedes: vec![],
        source_revision: None,
    }
}
