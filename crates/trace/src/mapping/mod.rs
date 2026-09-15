//! Per-vendor interpretation with explicit checkpointed source state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;

pub mod codex;
pub mod server;

/// The worker resolves these requests from the same retained evidence cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ArchiveRequest {
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
    pub evidence: Vec<ObservationRef>,
    /// A stable source selector and decoding rule, not a claim of original lexical bytes.
    pub selector: String,
    pub rule: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapperState {
    /// State is isolated by source/generation and mapper build by the worker.
    pub governing_agent: Option<AgentRef>,
    pub current_activation: Option<ActivationRef>,
    pub previous_entry: Option<EntryRef>,
    pub values: BTreeMap<String, serde_json::Value>,
}

/// Only evidence-proved inventory and correlation data may populate this context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveContext {
    pub agents: BTreeMap<AgentRef, AgentThread>,
    pub correlations: BTreeMap<String, AnyObjectRef>,
    pub source_prefixes: BTreeMap<SourceRef, Vec<SourceExtent>>,
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
    pub claims: Vec<Claim>,
    pub payloads: Vec<DerivedPayload>,
    pub state: MapperState,
    pub requests: Vec<ArchiveRequest>,
    pub diagnostics: Vec<MappingDiagnostic>,
    pub correlations: BTreeMap<String, AnyObjectRef>,
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

/// Malformed/unsupported vendor data returns diagnostic claims rather than a silent
/// empty parse. Envelope/storage failure remains the worker's separate admission error.
pub trait SemanticMapper {
    fn build(&self) -> &'static str;
    fn map(&self, input: MapInput<'_>) -> MapOutput;
}

pub fn derived_payload(
    input: &MapInput<'_>,
    bytes: Vec<u8>,
    selector: impl Into<String>,
    rule: impl Into<String>,
) -> DerivedPayload {
    DerivedPayload {
        reference: PayloadRef::bytes(blake3::hash(&bytes).to_hex())
            .expect("BLAKE3 is a valid digest"),
        bytes,
        evidence: vec![input.canonical_observation.clone()],
        selector: selector.into(),
        rule: rule.into(),
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
