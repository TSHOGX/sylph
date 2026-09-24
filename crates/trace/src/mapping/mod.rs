//! Per-vendor interpretation with explicit checkpointed source state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;

pub mod attachment;
pub mod claude;
pub mod codex;
pub mod inventory;
pub mod observation;
pub mod owner_resolution;
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
    ContinueOwnerResolution,
    ContinueCopies,
    ContinueForwarding,
    HistoryLocator {
        issuer: Issuer,
        physical_id: String,
    },
    HistorySource {
        source: SourceRef,
        generation: u64,
    },
    HistoryContent {
        entry: EntryRef,
        position: SourcePosition,
        mapping_rule: String,
    },
    CapturedPayload {
        parent: ObservationRef,
        slot: String,
    },
    AttachmentTarget {
        parent: ObservationRef,
        slot: String,
        locator: String,
    },
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

impl ArchiveRequest {
    pub fn is_continuation(&self) -> bool {
        matches!(
            self,
            Self::ContinueHistory
                | Self::ContinueOwnerResolution
                | Self::ContinueCopies
                | Self::ContinueForwarding
        )
    }
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

impl DerivedPayload {
    pub fn use_ref(&self) -> PayloadUse {
        PayloadUse {
            reference: self.reference.clone(),
            format: PayloadFormat {
                media_type: Some(self.media_type.clone()),
                encoding: Some(self.encoding),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapperState {
    /// State is isolated by source/generation and mapper build by the worker.
    pub governing_agent: Option<AgentRef>,
    pub current_activation: Option<ActivationRef>,
    pub previous_entry: Option<EntryRef>,
    pub values: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedPayloadBinding {
    pub parent: ObservationRef,
    pub slot: String,
    pub attachment: CaptureAttachment,
    pub evidence: Vec<ObservationRef>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPayload {
    pub bindings: Vec<CapturedPayloadBinding>,
    pub bytes: Option<Vec<u8>>,
    pub limited: bool,
}
pub fn captured_payload_key(parent: &ObservationRef, slot: &str) -> String {
    serde_json::to_string(&(parent, slot)).expect("typed captured payload key")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedSource {
    pub definition: SourceDefinition,
    pub observation: ObservationRef,
}

/// Only evidence-proved inventory and correlation data may populate this context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveContext {
    pub semantic: std::sync::Arc<ArchiveSemantic>,
    pub captured_payloads: imbl::OrdMap<String, CapturedPayload>,
    pub correlations: imbl::OrdMap<String, AnyObjectRef>,
    pub source_definitions: imbl::OrdMap<(SourceRef, u64), RecordedSource>,
    pub history_records: codex::history::HistoryRecords,
    pub observations: imbl::OrdSet<ObservationRef>,
    pub correlation_proofs: CorrelationProofs,
    pub owner_resolution_plans: owner_resolution::PlanIndex,
    pub forwarding_plans: claude::forwarding::Plans,
    pub pi_copies: pi::copies::Index,
    pub source_owners: imbl::OrdMap<(SourceRef, u64), SourceOwner>,
    pub observation_sources: imbl::OrdMap<ObservationRef, SourcePosition>,
    pub claims: imbl::OrdMap<ClaimRef, Claim>,
}

/// One immutable reduction supplies these facts to many source observations.
/// Sharing this value avoids copying every historical object per captured record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveSemantic {
    pub agents: imbl::OrdMap<AgentRef, AgentThread>,
    pub history_entries: imbl::OrdMap<EntryRef, HistoryEntry>,
    pub history_entry_proofs: imbl::OrdMap<EntryRef, imbl::OrdMap<ClaimRef, ArchiveEntryProof>>,
    pub history_contents: imbl::OrdMap<EntryRef, imbl::Vector<ArchiveHistoryContent>>,
    pub operations: imbl::OrdMap<OperationRef, Operation>,
    pub object_evidence: imbl::OrdMap<AnyObjectRef, imbl::Vector<ObservationRef>>,
    pub result_operations: imbl::OrdMap<ResultRef, OperationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntryProof {
    /// The declaration's source view, kept separate from other observations of
    /// the same native entry so derived membership has reproducible provenance.
    pub position: SourcePosition,
    pub evidence: Vec<ObservationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveHistoryContent {
    pub claim_ref: ClaimRef,
    pub observation: HistoryContentObservation,
    pub evidence: Vec<ObservationRef>,
}
impl ArchiveContext {
    /// A single correlation lookup uses one deterministic sufficient proof. A
    /// reactive join enumerates the alternatives instead of conjoining them.
    pub fn correlation_evidence(&self, key: &str) -> Option<&Vec<ObservationRef>> {
        let proofs = self.correlation_proofs.get(key)?;
        unique_correlation_reference(proofs)?;
        Some(&proofs.get_min()?.evidence)
    }

    pub fn retain_correlation_evidence(&mut self, key: String, evidence: Vec<ObservationRef>) {
        let reference = self
            .correlations
            .get(&key)
            .expect("a correlation proof names its target")
            .clone();
        retain_correlation_proof(
            &mut self.correlation_proofs,
            key.clone(),
            reference,
            evidence,
        );
        if let Some(proofs) = self.correlation_proofs.get(&key) {
            refresh_correlation_reference(&mut self.correlations, key, proofs);
        } else {
            self.correlations.remove(&key);
        }
    }

    pub fn extend_correlation_evidence(
        &mut self,
        evidence: impl IntoIterator<Item = (String, Vec<ObservationRef>)>,
    ) {
        for (key, proof) in evidence {
            self.correlation_proofs.remove(&key);
            self.retain_correlation_evidence(key, proof);
        }
    }

    pub fn semantic_mut(&mut self) -> &mut ArchiveSemantic {
        std::sync::Arc::make_mut(&mut self.semantic)
    }

    /// Frozen-history resolution asks for these witnesses explicitly. Ordinary
    /// records share the claim snapshot without traversing historical claims.
    pub fn source_prefixes(&self) -> BTreeMap<SourceRef, Vec<&SourceExtent>> {
        let mut prefixes = BTreeMap::<_, Vec<_>>::new();
        for claim in self.claims.values() {
            if let Fact::HistoryMappingComplete { extent } = &claim.fact {
                prefixes
                    .entry(extent.source.clone())
                    .or_default()
                    .push(extent);
            }
        }
        prefixes
    }

    pub fn native_history_positions(&self) -> BTreeMap<EntryRef, SourcePosition> {
        self.claims
            .values()
            .filter_map(|claim| match &claim.fact {
                Fact::HistoryPosition {
                    entry,
                    position,
                    coordinate: HistoryCoordinate::LocalRecordIndex,
                    mapping_rule,
                    ..
                } if mapping_rule == "codex/native-history" => {
                    Some((entry.clone(), position.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

/// A certificate's target is immutable, including when another target conflicts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CorrelationProof {
    pub reference: AnyObjectRef,
    pub evidence: Vec<ObservationRef>,
}
/// Each evidence vector is a conjunction; certificates are independent alternatives.
pub type CorrelationProofs = imbl::OrdMap<String, imbl::OrdSet<CorrelationProof>>;

pub fn unique_correlation_reference(
    proofs: &imbl::OrdSet<CorrelationProof>,
) -> Option<&AnyObjectRef> {
    let first = &proofs.get_min()?.reference;
    // Certificates sort by target first. The extremes establish agreement without
    // scanning every corroborating observation at each new header.
    (first == &proofs.get_max()?.reference).then_some(first)
}

pub fn refresh_correlation_reference(
    references: &mut imbl::OrdMap<String, AnyObjectRef>,
    key: String,
    proofs: &imbl::OrdSet<CorrelationProof>,
) {
    if let Some(reference) = unique_correlation_reference(proofs) {
        references.insert(key, reference.clone());
    } else {
        references.remove(&key);
    }
}

pub fn retain_correlation_proof(
    proofs: &mut CorrelationProofs,
    key: String,
    reference: AnyObjectRef,
    mut evidence: Vec<ObservationRef>,
) {
    evidence.sort();
    evidence.dedup();
    if !evidence.is_empty() {
        proofs.entry(key).or_default().insert(CorrelationProof {
            reference,
            evidence,
        });
    }
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
    /// The current record cannot advance this source's semantic state until its
    /// payload dependencies resolve. Independent archive joins do not set this.
    pub blocks_source: bool,
    pub capture_payload_bindings: Vec<CapturedPayloadBinding>,
    pub claims: Vec<Claim>,
    pub payloads: Vec<DerivedPayload>,
    pub state: MapperState,
    pub requests: Vec<ArchiveRequest>,
    pub diagnostics: Vec<MappingDiagnostic>,
    pub correlations: BTreeMap<String, AnyObjectRef>,
    pub correlation_evidence: BTreeMap<String, Vec<ObservationRef>>,
    /// These identity witnesses are alternatives used by reactive joins. Other
    /// correlations retain one current checkpoint witness, not a polling log.
    pub corroborating_correlations: std::collections::BTreeSet<String>,
    pub owner_resolution_plans: Vec<owner_resolution::Plan>,
    pub forwarding_plans: Vec<claude::forwarding::Plan>,
    pub pi_copy_witnesses: Vec<pi::copies::Witness>,
    pub pi_copy_plans: Vec<pi::copies::Plan>,
    pub bound_agents: Vec<AgentRef>,
    pub source_owners: Vec<SourceOwnership>,
    pub source_definitions: Vec<RecordedSource>,
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

pub fn native_source_owner_key(issuer: &Issuer, vendor: &str, locator: &str) -> String {
    serde_json::to_string(&("native-source-owner", issuer, vendor, locator))
        .expect("typed source owner key")
}

pub fn attachment_key(parent: &ObservationRef, slot: &str, locator: &str) -> String {
    serde_json::to_string(&("source-attachment", parent, slot, locator))
        .expect("typed attachment relation key")
}

pub fn command_identity_key(issuer: &Issuer, command_id: &str) -> String {
    serde_json::to_string(&("command_identity", issuer, command_id))
        .expect("typed command identity serialization")
}

pub const DERIVED_PAYLOAD_CHUNK_BYTES: usize = 1024 * 1024;

/// Optional pure classifier for independently ordered lanes in one vendor transport.
pub type SourcePartitioner = fn(&[u8]) -> Result<String, DiagnosticCode>;

/// Malformed/unsupported vendor data returns diagnostic claims rather than a silent
/// empty parse. Envelope/storage failure remains the worker's separate admission error.
pub trait SemanticMapper: Send + Sync {
    fn source_partitioner(&self, _capture: &Capture) -> Option<SourcePartitioner> {
        None
    }
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
            // A physical source can be retained by independent authorization
            // scopes. Its domain objects keep their source/native identities;
            // each canonical capture supplies a distinct immutable proof claim.
            vec![
                build.into(),
                slot.into(),
                input.canonical_observation.encode(),
            ],
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

const REFINEMENT_REQUESTS: &str = "refinement-requests";

pub(crate) fn resume_refinements(input: &MapInput<'_>) -> Option<MapOutput> {
    let owners = owner_resolution::has_continuation(input);
    let copies = pi::copies::has_continuation(input);
    let forwarding = claude::forwarding::has_continuation(input);
    if !owners && !copies && !forwarding {
        return None;
    }
    let mut output = MapOutput {
        state: input.state.clone(),
        continuation_state: Some(input.state.clone()),
        ..Default::default()
    };
    if owners {
        owner_resolution::refine(input, &mut output);
    }
    if copies {
        pi::copies::refine(input, &mut output);
    }
    if forwarding {
        claude::forwarding::refine(input, &mut output);
    }
    finish_refinements(input, &mut output);
    Some(output)
}

pub(crate) fn finish_refinements(input: &MapInput<'_>, output: &mut MapOutput) {
    let mut deferred: Vec<ArchiveRequest> = match input
        .state
        .values
        .get(REFINEMENT_REQUESTS)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
    {
        Ok(value) => value.unwrap_or_default(),
        Err(_) => {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::DecodeFailure,
                message: "The deferred mapping requests are invalid".into(),
            });
            return;
        }
    };
    // History expansion belongs to its vendor mapper. Preserve its request and
    // cursor while the independent metadata refinements finish their own pages.
    let auxiliary = |request: &ArchiveRequest| {
        matches!(
            request,
            ArchiveRequest::ContinueOwnerResolution
                | ArchiveRequest::ContinueCopies
                | ArchiveRequest::ContinueForwarding
        )
    };
    deferred.extend(
        output
            .requests
            .iter()
            .filter(|request| !auxiliary(request))
            .cloned(),
    );
    output.requests.retain(auxiliary);
    output.state.values.remove(REFINEMENT_REQUESTS);
    if let Some(state) = &mut output.continuation_state {
        state.values.remove(REFINEMENT_REQUESTS);
    }
    if output.requests.is_empty() {
        deferred.dedup();
        output.requests = deferred;
    } else if !deferred.is_empty() {
        output
            .continuation_state
            .get_or_insert_with(|| input.state.clone())
            .values
            .insert(
                REFINEMENT_REQUESTS.into(),
                serde_json::to_value(deferred).unwrap(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_capture_proofs_share_domain_identity_without_reusing_a_claim_identity() {
        let agent =
            AgentRef::Session(sylph_protocol::SessionId::decode("env:codex:native").unwrap());
        let issuer = Issuer::Environment(agent.environment().clone());
        let first = Capture {
            observation_ref: ObservationRef {
                issuer: issuer.clone(),
                producer_id: "capture".into(),
                producer_epoch: "one".into(),
                producer_seq: 1,
            },
            source_position: SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: "native-file".into(),
                },
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: 0,
                    byte_end: 2,
                    record_ordinal: 0,
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("launch-scope").unwrap(),
            declared_subject_refs: vec![agent.clone()],
            captured_at: TimeEvidence {
                domain: "capture".into(),
                value: 0,
                unit: TimeUnit::Milliseconds,
                precision: Some(1),
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        let mut independent = first.clone();
        independent.observation_ref.producer_seq = 2;
        independent.ingestion_authorization_epoch = IngestionEpoch::new("agent-scope").unwrap();
        let state = MapperState::default();
        let archive = ArchiveContext::default();
        let object = TraceObject::Agent(AgentThread {
            agent_ref: agent.clone(),
            vendor: agent.anchor_session().agent.clone(),
            name: None,
            model: None,
        });
        let map = |capture: &Capture, canonical: &ObservationRef| {
            claim(
                &MapInput {
                    capture,
                    canonical_observation: canonical,
                    raw: b"{}",
                    state: &state,
                    archive: &archive,
                },
                &agent,
                "/agent",
                "proof-test",
                Fact::Declare {
                    object: object.clone(),
                },
            )
        };
        let a = map(&first, &first.observation_ref);
        let b = map(&independent, &independent.observation_ref);
        assert_ne!(a.claim_ref, b.claim_ref);
        assert_eq!(a.fact, b.fact);
        assert_eq!(a.evidence, vec![first.observation_ref.clone()]);
        assert_eq!(b.evidence, vec![independent.observation_ref.clone()]);
        let mut alias = first.clone();
        alias.observation_ref.producer_seq = 99;
        assert_eq!(
            map(&alias, &first.observation_ref),
            a,
            "reread receipts must reuse the canonical occurrence's claim"
        );
        let mut reducer = crate::reducer::Reducer::default();
        reducer
            .apply(
                &[a, b],
                &[first.observation_ref, independent.observation_ref]
                    .into_iter()
                    .collect(),
                10,
            )
            .unwrap();
        let resolved = reducer.resolve();
        assert_eq!(resolved.objects.len(), 1);
        assert_eq!(
            resolved
                .object(&AnyObjectRef::Agent(agent))
                .unwrap()
                .declaration
                .known(),
            Some(&object)
        );
    }
}
