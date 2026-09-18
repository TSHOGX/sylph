//! Bounded archive work connects durable admission to the pure semantic library.
//!
//! The worker never acknowledges a producer, opens vendor paths or dispatches controls.
//! Its checkpoint advances with a complete projection unit. A cancelled or failed unit
//! is retried from retained captures and the previous committed source state.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

mod budget;
mod provenance;
mod reading;
mod reconciliation;
pub(super) use provenance::{ObjectProvenance, ProvenanceMap};
mod semantic;
pub use semantic::SemanticWork;
use sylph_protocol::trace::*;
use sylph_trace::{
    mapping::{
        self, ArchiveContext, ArchiveRequest, DerivedPayload, MapInput, MapOutput, MapperState,
        MappingDiagnostic, SemanticMapper,
    },
    reducer::{
        IndependentAppend, MessageUpdate, Reducer, Reduction, ReductionChanges, ReductionWork,
        Resolution,
    },
};
use tokio::sync::{Mutex, Semaphore};

use super::{
    evidence::catalogue::{CatalogueError, EvidenceCatalogue, PinOwnerKind},
    evidence::{AdmittedCapture, EvidenceError, EvidenceStore, ReceiptDisposition},
    generation::{
        Collection, GenerationError, GenerationStore, HistoryMode, MembershipChange,
        MembershipFilter, ObjectUpdate, ProjectionBatch, QueryScope, RelationFilter, Snapshot,
    },
};

#[derive(Debug, Clone)]
pub struct WorkerLimits {
    pub max_decode_bytes: usize,
    pub max_claims_per_observation: usize,
    pub max_reducer_claims: usize,
    pub max_incremental_dependencies: usize,
    pub max_pending: usize,
    pub max_state_bytes: usize,
    pub max_resolution_passes: usize,
    pub concurrent_cpu_work: usize,
    pub max_json_nodes: usize,
    pub max_json_depth: usize,
    pub max_identity_bytes: usize,
    pub max_unit_observations: usize,
    pub max_unit_input_bytes: usize,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            max_decode_bytes: 16 * 1024 * 1024,
            max_claims_per_observation: 512,
            max_reducer_claims: 250_000,
            max_incremental_dependencies: 1_000_000,
            max_pending: 4096,
            max_state_bytes: 64 * 1024 * 1024,
            max_resolution_passes: 8,
            concurrent_cpu_work: 2,
            max_json_nodes: 8192,
            max_json_depth: 64,
            max_identity_bytes: 4096,
            max_unit_observations: 32,
            max_unit_input_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error(transparent)]
    Reduction(#[from] sylph_trace::reducer::ReductionError),
    #[error("projection resource limit: {0}")]
    Limit(&'static str),
    #[error("projection worker failed: {0}")]
    Task(String),
    #[error("the generation pins another mapper build for {0}")]
    MapperBuild(String),
    #[error("a scoped archive correlation has incompatible targets")]
    CorrelationConflict,
    #[error("the requested evidence cut is not yet admitted")]
    FutureCut,
}

type Result<T> = std::result::Result<T, WorkerError>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkerProgress {
    pub evidence_cut: u64,
    pub projected_cut: u64,
    pub pending_mappings: usize,
    pub diagnostics: Vec<MappingDiagnostic>,
    pub suspended_sources: usize,
    pub unmapped_observations: usize,
    /// Rows constructed by reader materialization during this worker pass.
    pub reader_rows_materialized: usize,
    pub message_updates: usize,
    pub reduction: ReductionWork,
    pub membership: MembershipWork,
    pub reader_append: sylph_trace::reading::AppendWork,
    pub semantic: SemanticWork,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipWork {
    pub entries_examined: usize,
    pub objects_examined: usize,
    pub changes_emitted: usize,
    pub full_reconciliations: usize,
}

impl MembershipWork {
    fn accumulate(&mut self, other: &Self) {
        self.entries_examined += other.entries_examined;
        self.objects_examined += other.objects_examined;
        self.changes_emitted += other.changes_emitted;
        self.full_reconciliations += other.full_reconciliations;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CorrelationBinding {
    reference: AnyObjectRef,
    evidence: Vec<ObservationRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingMapping {
    admitted: AdmittedMapping,
    state: MapperState,
    requests: Vec<ArchiveRequest>,
    /// A stateful payload wait holds ordered successors, while independent joins
    /// and frozen-history expansion keep their own retry state.
    source_order: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceReplayCursor {
    source: SourceRef,
    generation: u64,
    epoch: IngestionEpoch,
    after: u64,
    through: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdmittedMapping {
    admission_seq: u64,
    occurrence_id: u64,
    canonical_observation: ObservationRef,
    capture: Capture,
}

impl From<AdmittedCapture> for AdmittedMapping {
    fn from(value: AdmittedCapture) -> Self {
        Self {
            admission_seq: value.admission_seq,
            occurrence_id: value.occurrence_id,
            canonical_observation: value.canonical_observation_ref,
            capture: value.capture,
        }
    }
}

#[derive(Clone, Default)]
struct WorkerState {
    // Absence distinguishes an uninitialized cache from a committed cut-zero
    // generation. Same-cut continuations still replace its semantic revision.
    snapshot: Option<Snapshot>,
    reducer: Reducer,
    reduction: Arc<Reduction>,
    // A candidate shares tree roots and heavy leaf values. Persistent maps clone
    // neighboring values on leaf mutation; Arc keeps that cost off mapper bodies.
    source_states: imbl::OrdMap<String, Arc<MapperState>>,
    correlations: imbl::OrdMap<String, AnyObjectRef>,
    correlation_evidence: imbl::OrdMap<String, Vec<ObservationRef>>,
    pending: imbl::OrdMap<String, Arc<PendingMapping>>,
    source_replays: imbl::OrdMap<String, SourceReplayCursor>,
    available_evidence: imbl::OrdSet<ObservationRef>,
    gaps: imbl::OrdMap<String, SourceGap>,
    source_owners: imbl::OrdMap<(SourceRef, u64), mapping::SourceOwner>,
    source_definitions: imbl::OrdMap<(SourceRef, u64), mapping::codex::history::RecordedSource>,
    history_records: mapping::codex::history::HistoryRecords,
    captured_payloads: imbl::OrdMap<String, imbl::Vector<mapping::CapturedPayloadBinding>>,
    observation_sources: imbl::OrdMap<ObservationRef, SourcePosition>,
    previews: sylph_trace::reading::PayloadPreviews,
    object_provenance: ProvenanceMap,
    provenance_index: Option<provenance::SupportIndex>,
    reading: Option<Arc<reading::MaterializedReading>>,
    archive_semantic: Option<semantic::SemanticCache>,
    semantic_work: semantic::SemanticWork,
}

impl WorkerState {
    fn committed_snapshot(&self) -> &Snapshot {
        self.snapshot
            .as_ref()
            .expect("worker state is loaded before projection")
    }

    fn cut(&self) -> u64 {
        self.committed_snapshot().evidence_cut
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceGap {
    first: ObservationRef,
    through: ObservationRef,
    code: DiagnosticCode,
    reason: String,
    unmapped: u64,
}

struct RecordMapping {
    output: MapOutput,
    gap: Option<(DiagnosticCode, String)>,
}

struct ProjectionUnit {
    next: WorkerState,
    batch: ProjectionBatch,
    diagnostics: Vec<MappingDiagnostic>,
    unmapped: usize,
    scopes: Vec<(IngestionEpoch, u64)>,
    payloads: Vec<(IngestionEpoch, DerivedPayload)>,
    composed_payloads: Vec<(IngestionEpoch, PayloadRef, PayloadManifest)>,
    bindings: Vec<(IngestionEpoch, ObservationRef, AgentRef)>,
    pins: BTreeSet<(PayloadRef, IngestionEpoch)>,
    reader_rows_materialized: usize,
    message_updates: usize,
    reduction_changes: ReductionChanges,
    reduction_work: ReductionWork,
    membership_work: MembershipWork,
    reader_append_work: sylph_trace::reading::AppendWork,
    append: Option<IndependentAppend>,
    message_update: Option<MessageUpdate>,
    append_eligible: bool,
}

impl ProjectionUnit {
    fn new(state: &WorkerState, through: u64) -> Self {
        let mut next = state.clone();
        next.semantic_work = semantic::SemanticWork::default();
        Self {
            next,
            batch: ProjectionBatch {
                through_cut: through,
                ..ProjectionBatch::default()
            },
            diagnostics: vec![],
            unmapped: 0,
            scopes: vec![],
            payloads: vec![],
            composed_payloads: vec![],
            bindings: vec![],
            pins: BTreeSet::new(),
            reader_rows_materialized: 0,
            message_updates: 0,
            reduction_changes: ReductionChanges::default(),
            reduction_work: ReductionWork::default(),
            membership_work: MembershipWork::default(),
            reader_append_work: sylph_trace::reading::AppendWork::default(),
            append: None,
            message_update: None,
            append_eligible: true,
        }
    }
    fn gap(
        &mut self,
        key: &str,
        admitted: &AdmittedMapping,
        code: DiagnosticCode,
        reason: String,
    ) -> Result<()> {
        let existed = self.next.gaps.contains_key(key);
        let gap = self
            .next
            .gaps
            .entry(key.into())
            .or_insert_with(|| SourceGap {
                first: admitted.canonical_observation.clone(),
                through: admitted.canonical_observation.clone(),
                code,
                reason: reason.clone(),
                unmapped: 0,
            });
        gap.through = admitted.canonical_observation.clone();
        gap.unmapped = gap.unmapped.saturating_add(1);
        self.batch
            .mapper_states
            .insert(format!("gap:{key}"), serde_json::to_value(gap)?);
        self.unmapped += 1;
        if !existed {
            self.diagnostics.push(MappingDiagnostic {
                evidence: vec![admitted.canonical_observation.clone()],
                code,
                message: reason,
            });
        }
        Ok(())
    }
}

struct OutputContext<'a> {
    admitted: &'a AdmittedMapping,
    original_state: &'a MapperState,
}

#[derive(Clone)]
pub struct ProjectionWorker {
    evidence: EvidenceStore,
    catalogue: EvidenceCatalogue,
    limits: WorkerLimits,
    cpu: Arc<Semaphore>,
    states: Arc<Mutex<BTreeMap<String, Arc<Mutex<WorkerState>>>>>,
    #[cfg(test)]
    mapping_threads: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>,
    #[cfg(test)]
    full_reader_rebuilds: bool,
}

impl ProjectionWorker {
    pub(crate) async fn projected_snapshot(
        &self,
        generation: &GenerationStore,
    ) -> Result<(Snapshot, Arc<Reduction>, ProvenanceMap)> {
        let entry = {
            let mut states = self.states.lock().await;
            states
                .entry(generation.manifest().generation.clone())
                .or_default()
                .clone()
        };
        let mut state = entry.lock().await;
        let checkpoint = generation.checkpoint().await?;
        if state.snapshot.as_ref() != Some(&checkpoint.snapshot) {
            *state = self.load_state(generation, &checkpoint.snapshot).await?.0;
        }
        Ok((
            checkpoint.snapshot,
            state.reduction.clone(),
            state.object_provenance.clone(),
        ))
    }
    pub fn new(
        evidence: EvidenceStore,
        catalogue: EvidenceCatalogue,
        limits: WorkerLimits,
    ) -> Result<Self> {
        if [
            limits.max_decode_bytes,
            limits.max_claims_per_observation,
            limits.max_reducer_claims,
            limits.max_incremental_dependencies,
            limits.max_pending,
            limits.max_state_bytes,
            limits.max_resolution_passes,
            limits.concurrent_cpu_work,
            limits.max_json_nodes,
            limits.max_json_depth,
            limits.max_identity_bytes,
            limits.max_unit_observations,
            limits.max_unit_input_bytes,
        ]
        .contains(&0)
            || evidence.limits().max_chunk_bytes < mapping::DERIVED_PAYLOAD_CHUNK_BYTES
        {
            return Err(WorkerError::Limit("incompatible worker bounds"));
        }
        Ok(Self {
            evidence,
            catalogue,
            cpu: Arc::new(Semaphore::new(limits.concurrent_cpu_work)),
            limits,
            states: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            mapping_threads: Arc::new(std::sync::Mutex::new(Vec::new())),
            #[cfg(test)]
            full_reader_rebuilds: false,
        })
    }

    pub async fn has_continuations(&self, generation: &GenerationStore) -> Result<bool> {
        let state = {
            self.states
                .lock()
                .await
                .get(&generation.manifest().generation)
                .cloned()
        };
        let state = match state.as_ref() {
            Some(state) => Some(state.lock().await),
            None => None,
        };
        let checkpoint = generation.checkpoint().await?;
        if let Some(state) = state.as_ref() {
            if state.snapshot.as_ref() == Some(&checkpoint.snapshot) {
                return Ok(has_ready_continuations(
                    &state.pending,
                    &state.source_replays,
                ));
            }
        }
        let mut pending = imbl::OrdMap::new();
        let mut replays = imbl::OrdMap::new();
        for (key, value) in generation.mapper_states(&checkpoint.snapshot).await? {
            if value.is_null() {
                continue;
            }
            if key.starts_with("pending:") {
                pending.insert(key, Arc::new(serde_json::from_value(value)?));
            } else if let Some(key) = key.strip_prefix("source_replay:") {
                replays.insert(key.into(), serde_json::from_value(value)?);
            }
        }
        Ok(has_ready_continuations(&pending, &replays))
    }

    pub async fn catch_up(
        &self,
        generation: &GenerationStore,
        token: u64,
        through_cut: u64,
    ) -> Result<WorkerProgress> {
        let evidence_cut = self.evidence.evidence_cut().await?;
        if through_cut > evidence_cut {
            return Err(WorkerError::FutureCut);
        }
        generation.resume_reconciliation(token).await?;
        let held = {
            let mut states = self.states.lock().await;
            states
                .entry(generation.manifest().generation.clone())
                .or_default()
                .clone()
        };
        let mut state = held.lock().await;
        let checkpoint = generation.checkpoint().await?;
        let mut restoration_work = ReductionWork::default();
        if state.snapshot.as_ref() != Some(&checkpoint.snapshot) {
            let (restored, work) = self.load_state(generation, &checkpoint.snapshot).await?;
            *state = restored;
            restoration_work = work;
        }
        let mut progress = WorkerProgress {
            evidence_cut,
            projected_cut: state.cut(),
            pending_mappings: state.pending.len(),
            suspended_sources: state.gaps.len(),
            reduction: restoration_work,
            ..WorkerProgress::default()
        };
        while state.cut() < through_cut
            || has_ready_continuations(&state.pending, &state.source_replays)
        {
            let page = if state.cut() < through_cut {
                let limit = self
                    .limits
                    .max_unit_observations
                    .min(self.evidence.limits().max_read_items)
                    .min(usize::try_from(through_cut - state.cut()).unwrap_or(usize::MAX));
                Some(self.evidence.admissions_after(state.cut(), limit).await?)
            } else {
                None
            };
            if page
                .as_ref()
                .is_some_and(|page| page.next_after <= state.cut())
            {
                return Err(WorkerError::FutureCut);
            }
            let mut captures = Vec::new();
            let mut input_bytes = 0u64;
            let mut unit_cut = page
                .as_ref()
                .map_or(state.cut(), |page| page.next_after.min(through_cut));
            for capture in page
                .into_iter()
                .flat_map(|page| page.captures)
                .filter(|capture| capture.admission_seq <= through_cut)
            {
                let length = self
                    .evidence
                    .payload_length(&capture.capture.payload_ref)
                    .await?
                    .unwrap_or(self.limits.max_unit_input_bytes as u64);
                if !captures.is_empty()
                    && input_bytes.saturating_add(length) > self.limits.max_unit_input_bytes as u64
                {
                    unit_cut = captures
                        .last()
                        .map(|capture: &AdmittedCapture| capture.admission_seq)
                        .unwrap();
                    break;
                }
                input_bytes = input_bytes.saturating_add(length);
                captures.push(capture);
            }
            let unit = loop {
                let prepared = self
                    .prepare_unit(generation, &state, &captures, unit_cut)
                    .await?;
                // Oversized candidates still spent this work before batch splitting.
                progress.reader_rows_materialized += prepared.reader_rows_materialized;
                progress.message_updates += prepared.message_updates;
                progress.reduction.accumulate(&prepared.reduction_work);
                progress.membership.accumulate(&prepared.membership_work);
                progress
                    .reader_append
                    .accumulate(&prepared.reader_append_work);
                progress.semantic.accumulate(&prepared.next.semantic_work);
                let mut validation = generation.validate_batch(&prepared.batch);
                if captures.len() <= 1
                    && validation
                        .as_ref()
                        .err()
                        .is_some_and(reconciliation::is_aggregate_limit)
                {
                    validation = reconciliation::validate(generation, &prepared.batch);
                }
                match validation {
                    Ok(()) => break prepared,
                    Err(GenerationError::Limit(_)) if captures.len() > 1 => {
                        captures.truncate(captures.len().div_ceil(2));
                        unit_cut = captures.last().unwrap().admission_seq;
                    }
                    Err(GenerationError::Limit(reason)) if captures.len() == 1 => {
                        // A single retained record can exceed projection expansion
                        // bounds. Keep it and suspend this source instead of losing
                        // bytes or reusing potentially stale lifecycle state.
                        let mut limited = ProjectionUnit::new(&state, unit_cut);
                        let admitted = AdmittedMapping::from(captures[0].clone());
                        let proof = self
                            .catalogue
                            .scope_proof(&admitted.capture.ingestion_authorization_epoch)
                            .await?;
                        let vendor = mapper_vendor(&admitted.capture, &proof.scope);
                        let key = source_key(&admitted, mapper(vendor)?.build())?;
                        limited.gap(&key,&admitted,DiagnosticCode::ResourceLimit,format!("Projection expansion exceeds {reason}; this source requires reprojection"))?;
                        limited
                            .batch
                            .processed_occurrences
                            .push(admitted.occurrence_id);
                        limited.scopes.push((
                            admitted.capture.ingestion_authorization_epoch.clone(),
                            admitted.admission_seq,
                        ));
                        generation.validate_batch(&limited.batch)?;
                        break limited;
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            self.commit_effects(generation, &unit).await?;
            let committed =
                reconciliation::apply(generation, token, state.committed_snapshot(), &unit.batch)
                    .await?;
            // No mutable source state or graph escapes before the atomic generation
            // commit. Cancellation drops this unit and leaves the prior cache intact.
            progress.projected_cut = committed.snapshot.evidence_cut;
            progress.pending_mappings = unit.next.pending.len();
            progress.suspended_sources = unit.next.gaps.len();
            progress.unmapped_observations += unit.unmapped;
            progress.diagnostics.extend(unit.diagnostics);
            *state = unit.next;
            state.snapshot = Some(committed.snapshot);
            tokio::task::yield_now().await;
        }
        Ok(progress)
    }

    async fn prepare_unit(
        &self,
        generation: &GenerationStore,
        state: &WorkerState,
        captures: &[AdmittedCapture],
        through: u64,
    ) -> Result<ProjectionUnit> {
        let mut unit = ProjectionUnit::new(state, through);
        unit.batch.continuation = through == state.cut();
        let mut processed = BTreeSet::new();
        for captured in captures {
            unit.next
                .available_evidence
                .insert(captured.capture.observation_ref.clone());
            unit.next
                .available_evidence
                .insert(captured.canonical_observation_ref.clone());
            if !processed.insert(captured.occurrence_id)
                || generation
                    .has_processed_occurrence(captured.occurrence_id)
                    .await?
            {
                continue;
            }
            unit.next.observation_sources.insert(
                captured.capture.observation_ref.clone(),
                captured.capture.source_position.clone(),
            );
            unit.next.observation_sources.insert(
                captured.canonical_observation_ref.clone(),
                captured.capture.source_position.clone(),
            );
            let admitted = AdmittedMapping::from(captured.clone());
            let proof = self
                .catalogue
                .scope_proof(&admitted.capture.ingestion_authorization_epoch)
                .await?;
            let vendor = mapper_vendor(&admitted.capture, &proof.scope);
            let build = mapper(vendor)?.build();
            if generation
                .manifest()
                .mapper_builds
                .get(vendor)
                .map(String::as_str)
                != Some(build)
            {
                return Err(WorkerError::MapperBuild(vendor.into()));
            }
            let key = source_key(&admitted, build)?;
            unit.scopes.push((
                admitted.capture.ingestion_authorization_epoch.clone(),
                admitted.admission_seq,
            ));
            // Only this source generation waits; raw admission and independent
            // attachment/inventory mappings continue under their existing bounds.
            if let Some(cursor) = unit.next.source_replays.get_mut(&key) {
                cursor.through = cursor.through.max(admitted.admission_seq);
                unit.batch.mapper_states.insert(
                    format!("source_replay:{key}"),
                    serde_json::to_value(cursor)?,
                );
            } else if let Some(gap) = unit.next.gaps.get(&key).cloned() {
                unit.gap(&key, &admitted, gap.code, gap.reason)?;
            } else {
                let original = unit
                    .next
                    .source_states
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let mapped = self
                    .map_record(&admitted, &original, &mut unit.next, through, vendor)
                    .await?;
                if let Some((code, reason)) = mapped.gap {
                    unit.gap(&key, &admitted, code, reason)?;
                } else {
                    if !mapped.output.blocks_source {
                        save_source_state(&mut unit, &key, &mapped.output.state)?;
                    }
                    self.apply_output(
                        OutputContext {
                            admitted: &admitted,
                            original_state: &original,
                        },
                        mapped.output,
                        &mut unit,
                    )
                    .await?;
                }
            }
            unit.batch
                .processed_occurrences
                .push(admitted.occurrence_id);
        }
        if unit.next.reducer.len() != state.reducer.len() {
            self.reduce_unit(&mut unit).await?;
        }
        let mut continuation_applied = false;
        let mut ordered_applied = 0usize;
        for pass in 0..self.limits.max_resolution_passes {
            let before = (
                unit.next.pending.keys().cloned().collect::<Vec<_>>(),
                unit.next.correlations.clone(),
                unit.next.correlation_evidence.clone(),
                unit.next.reducer.len(),
                unit.next.source_owners.clone(),
            );
            let mut pending_records = unit.next.pending.values().cloned().collect::<Vec<_>>();
            pending_records.sort_by_key(|pending| pending.admitted.admission_seq);
            for pending in pending_records {
                if let Some(key) = &pending.source_order {
                    if ordered_applied >= self.limits.max_unit_observations {
                        continue;
                    }
                    if !pending.requests.is_empty()
                        && !requests_may_resolve(&pending.requests, &unit.next)
                    {
                        continue;
                    }
                    if let Some(gap) = unit.next.gaps.get(key).cloned() {
                        unit.gap(key, &pending.admitted, gap.code, gap.reason)?;
                        let id = pending.admitted.canonical_observation.encode();
                        unit.next.pending.remove(&id);
                        unit.batch
                            .mapper_states
                            .insert(format!("pending:{id}"), serde_json::Value::Null);
                        continue;
                    }
                    ordered_applied += 1;
                }

                if pending.requests.contains(&ArchiveRequest::ContinueHistory) {
                    if continuation_applied
                        || captures.iter().any(|capture| {
                            capture.canonical_observation_ref
                                == pending.admitted.canonical_observation
                        })
                    {
                        continue;
                    }
                    continuation_applied = true;
                }
                if pending.source_order.is_none()
                    && !requests_may_resolve(&pending.requests, &unit.next)
                {
                    continue;
                }
                let proof = self
                    .catalogue
                    .scope_proof(&pending.admitted.capture.ingestion_authorization_epoch)
                    .await?;
                let vendor = mapper_vendor(&pending.admitted.capture, &proof.scope);
                // The ordered retry reads its committed predecessor. Replacing
                // source state after later records had run would lose that tail.
                let original = if let Some(key) = &pending.source_order {
                    unit.next
                        .source_states
                        .get(key)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    Arc::new(pending.state.clone())
                };
                let mapped = self
                    .map_record(
                        &pending.admitted,
                        &original,
                        &mut unit.next,
                        through,
                        vendor,
                    )
                    .await?;
                if let Some((code, reason)) = mapped.gap {
                    let key = source_key(&pending.admitted, mapper(vendor)?.build())?;
                    unit.gap(&key, &pending.admitted, code, reason)?;
                    let id = pending.admitted.canonical_observation.encode();
                    unit.next.pending.remove(&id);
                    unit.batch
                        .mapper_states
                        .insert(format!("pending:{id}"), serde_json::Value::Null);
                } else {
                    unit.scopes.push((
                        pending
                            .admitted
                            .capture
                            .ingestion_authorization_epoch
                            .clone(),
                        pending.admitted.admission_seq,
                    ));
                    if let Some(key) = &pending.source_order {
                        if !mapped.output.blocks_source {
                            save_source_state(&mut unit, key, &mapped.output.state)?;
                        }
                    }
                    self.apply_output(
                        OutputContext {
                            admitted: &pending.admitted,
                            original_state: &original,
                        },
                        mapped.output,
                        &mut unit,
                    )
                    .await?;
                }
            }
            let after = (
                unit.next.pending.keys().cloned().collect::<Vec<_>>(),
                unit.next.correlations.clone(),
                unit.next.correlation_evidence.clone(),
                unit.next.reducer.len(),
                unit.next.source_owners.clone(),
            );
            if before == after {
                break;
            }
            self.reduce_unit(&mut unit).await?;
            if pass + 1 == self.limits.max_resolution_passes
                && unit.next.pending.values().any(|pending| {
                    pending.source_order.is_none()
                        && !pending.requests.contains(&ArchiveRequest::ContinueHistory)
                        && requests_may_resolve(&pending.requests, &unit.next)
                })
            {
                return Err(WorkerError::Limit("pending dependency closure"));
            }
        }
        self.replay_source_tails(&mut unit, through).await?;
        if unit.next.reducer.len() > self.limits.max_reducer_claims {
            return Err(WorkerError::Limit("retained reducer claims"));
        }
        self.materialize(generation, state, &mut unit).await?;
        if serde_json::to_vec(&unit.batch.mapper_states)?.len() > self.limits.max_state_bytes {
            return Err(WorkerError::Limit("mapper checkpoint bytes"));
        }
        Ok(unit)
    }

    async fn reduce_unit(&self, unit: &mut ProjectionUnit) -> Result<()> {
        let resolved = self.reduce(&mut unit.next.reducer).await?;
        let claims = unit.next.reducer.claim_snapshot();
        let advanced = unit
            .next
            .archive_semantic
            .as_ref()
            .zip(resolved.append.as_ref())
            .filter(|(_, proof)| proof.matches(&unit.next.reduction, &resolved.reduction))
            .and_then(|(cache, proof)| cache.advance(proof, &claims, &mut unit.next.semantic_work))
            .or_else(|| {
                unit.next
                    .archive_semantic
                    .as_ref()
                    .zip(resolved.message_update.as_ref())
                    .filter(|(_, proof)| proof.matches(&unit.next.reduction, &resolved.reduction))
                    .and_then(|(cache, proof)| {
                        cache.advance_message(proof, &claims, &mut unit.next.semantic_work)
                    })
            });
        // Full reconciliation pays for its mapper view before publication. Leaving
        // this lazy would charge the first ordinary append a hidden archive scan.
        unit.next.archive_semantic = Some(advanced.unwrap_or_else(|| {
            semantic::SemanticCache::rebuild(
                resolved.reduction.clone(),
                &claims,
                &mut unit.next.semantic_work,
            )
        }));
        if unit.append_eligible && !Arc::ptr_eq(&unit.next.reduction, &resolved.reduction) {
            let (append, message_update) = match (
                unit.append.take(),
                unit.message_update.take(),
                resolved.append,
                resolved.message_update,
            ) {
                (Some(previous), None, Some(next), None) => (previous.compose(next), None),
                (None, Some(previous), None, Some(next)) => (None, previous.compose(next)),
                (None, None, Some(next), None)
                    if next.matches(&unit.next.reduction, &resolved.reduction) =>
                {
                    (Some(next), None)
                }
                (None, None, None, Some(next))
                    if next.matches(&unit.next.reduction, &resolved.reduction) =>
                {
                    (None, Some(next))
                }
                _ => (None, None),
            };
            unit.append = append;
            unit.message_update = message_update;
            // A later tail cannot erase an earlier unproved change in this unit.
            unit.append_eligible = unit.append.is_some() || unit.message_update.is_some();
        }
        unit.next.reduction = resolved.reduction;
        unit.reduction_changes.compose(resolved.changes);
        unit.reduction_work.accumulate(&resolved.work);
        Ok(())
    }

    async fn reduce(&self, reducer: &mut Reducer) -> Result<Resolution> {
        let mut owned = std::mem::take(reducer);
        #[cfg(test)]
        let max_dependencies = if self.full_reader_rebuilds {
            0
        } else {
            self.limits.max_incremental_dependencies
        };
        #[cfg(not(test))]
        let max_dependencies = self.limits.max_incremental_dependencies;
        let permit = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| WorkerError::Task(error.to_string()))?;
        let (restored, result) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = owned.resolve_incremental(max_dependencies);
            (owned, result)
        })
        .await
        .map_err(|error| WorkerError::Task(error.to_string()))?;
        *reducer = restored;
        Ok(result)
    }

    async fn commit_effects(
        &self,
        generation: &GenerationStore,
        unit: &ProjectionUnit,
    ) -> Result<()> {
        for (epoch, sequence) in &unit.scopes {
            let proof = self.catalogue.scope_proof(epoch).await?;
            self.catalogue
                .pin_generation_scope(&generation.manifest().generation, &proof, *sequence)
                .await?;
        }
        for (epoch, observation, agent) in &unit.bindings {
            let proof = self.catalogue.scope_proof(epoch).await?;
            if self
                .catalogue
                .bind_inventory_agent(&proof, agent, observation, crate::models::now_ms())
                .await?
                .is_some()
            {
                return Err(CatalogueError::DeletedScope.into());
            }
        }
        for (epoch, payload) in &unit.payloads {
            self.stage_derived(generation, epoch, payload).await?;
        }
        self.stage_composed_reading(generation, unit).await?;
        for (payload, epoch) in &unit.pins {
            self.catalogue
                .pin_payloads(
                    PinOwnerKind::Generation,
                    &generation.manifest().generation,
                    &[super::evidence::catalogue::PayloadPin {
                        payload: payload.clone(),
                        epoch: epoch.clone(),
                    }],
                )
                .await?;
        }
        Ok(())
    }

    async fn load_state(
        &self,
        generation: &GenerationStore,
        snapshot: &Snapshot,
    ) -> Result<(WorkerState, ReductionWork)> {
        let mut state = WorkerState {
            snapshot: Some(snapshot.clone()),
            ..WorkerState::default()
        };
        let mut after = None;
        loop {
            let page = generation
                .retained_claims(
                    snapshot,
                    after.as_ref(),
                    generation.limits().max_candidates,
                    generation.limits().max_batch_bytes,
                )
                .await?;
            state
                .available_evidence
                .extend(page.claims.iter().flat_map(|claim| claim.evidence.clone()));
            state.reducer.apply(
                &page.claims,
                &state.available_evidence,
                generation.limits().max_candidates,
            )?;
            if state.reducer.len() > self.limits.max_reducer_claims {
                return Err(WorkerError::Limit("retained reducer claims"));
            }
            if !page.has_more {
                break;
            }
            after = page.next;
        }
        // Claims are temporal, while mapper state is latest-only. Each page checks
        // this exact snapshot and reads its mapper rows in one transaction, so a
        // concurrent continuation rejects restoration instead of mixing revisions.
        //
        // The checkpoint is read in bounded pages, like the claims above. Reading it
        // whole and comparing it to `max_state_bytes` applied a per-batch transient
        // limit to accumulated history, which made the limit a ceiling on how long a
        // history could be before a restarted worker could no longer resume it at
        // all — while the same worker held the whole reduction in memory under
        // `max_reducer_claims`. The bound belongs on the page, which is the buffer
        // that must not grow.
        let mut cursor: Option<String> = None;
        loop {
            let page = generation
                .mapper_state_page(snapshot, cursor.as_deref(), self.limits.max_state_bytes)
                .await?;
            let resume = page.next;
            for (key, value) in page.states {
                self.restore_state_entry(&mut state, key, value)?;
            }
            match resume {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let resolved = self.reduce(&mut state.reducer).await?;
        state.reduction = resolved.reduction;
        self.restore_provenance_index(&mut state, snapshot.evidence_cut)
            .await?;
        Ok((state, resolved.work))
    }

    /// Apply one restored mapper-checkpoint entry to the state being rebuilt.
    ///
    /// Split out of `load_state` so the paged read has one body per entry rather
    /// than a loop whose shape depends on how the rows were batched.
    fn restore_state_entry(
        &self,
        state: &mut WorkerState,
        key: String,
        value: serde_json::Value,
    ) -> Result<()> {
        {
            if value.is_null() {
                return Ok(());
            }
            if let Some(key) = key.strip_prefix("source:") {
                state
                    .source_states
                    .insert(key.into(), Arc::new(serde_json::from_value(value)?));
            } else if let Some(key) = key.strip_prefix("correlation:") {
                let binding: CorrelationBinding = serde_json::from_value(value)?;
                state.correlations.insert(key.into(), binding.reference);
                state
                    .correlation_evidence
                    .insert(key.into(), binding.evidence);
            } else if let Some(key) = key.strip_prefix("source_replay:") {
                state
                    .source_replays
                    .insert(key.into(), serde_json::from_value(value)?);
            } else if let Some(key) = key.strip_prefix("pending:") {
                state
                    .pending
                    .insert(key.into(), Arc::new(serde_json::from_value(value)?));
            } else if let Some(key) = key.strip_prefix("gap:") {
                state
                    .gaps
                    .insert(key.into(), serde_json::from_value(value)?);
            } else if let Some(key) = key.strip_prefix("captured_payload:") {
                state
                    .captured_payloads
                    .insert(key.into(), serde_json::from_value(value)?);
            } else if key.starts_with("source_definition:") {
                let definition: mapping::codex::history::RecordedSource =
                    serde_json::from_value(value)?;
                state.source_definitions.insert(
                    (
                        definition.definition.source.clone(),
                        definition.definition.generation,
                    ),
                    definition,
                );
            } else if key.starts_with("history_record:") {
                index_history_record(&mut state.history_records, serde_json::from_value(value)?)?;
            } else if key.starts_with("owner:") {
                let ownership: mapping::SourceOwnership = serde_json::from_value(value)?;
                state
                    .available_evidence
                    .extend(ownership.owner.evidence.iter().cloned());
                state
                    .source_owners
                    .insert((ownership.source, ownership.generation), ownership.owner);
            } else if let Some(key) = key.strip_prefix("object_provenance:") {
                let reference = AnyObjectRef::decode(key)
                    .map_err(|error| WorkerError::Task(error.to_string()))?;
                if !value.is_null() {
                    state
                        .object_provenance
                        .insert(reference, Arc::new(serde_json::from_value(value)?));
                }
            } else if let Some(key) = key.strip_prefix("read_preview:") {
                state.previews.insert(
                    PayloadRef::parse(key).map_err(|error| WorkerError::Task(error.to_string()))?,
                    Arc::new(serde_json::from_value(value)?),
                );
            }
        }
        Ok(())
    }

    async fn map_record(
        &self,
        admitted: &AdmittedMapping,
        original: &Arc<MapperState>,
        state: &mut WorkerState,
        through_cut: u64,
        vendor: &str,
    ) -> Result<RecordMapping> {
        if admitted.capture.transport_kind == TransportKind::PayloadBytes {
            return Ok(RecordMapping {
                output: MapOutput {
                    state: original.as_ref().clone(),
                    ..MapOutput::default()
                },
                gap: None,
            });
        }
        let raw=match self.evidence.payload_bytes(&admitted.capture.payload_ref,self.limits.max_decode_bytes).await{
            Ok(raw)=>Arc::new(raw),
            Err(EvidenceError::Limit(_))=>return Ok(RecordMapping{output:MapOutput::default(),gap:Some((DiagnosticCode::ResourceLimit,"Retained observation exceeds semantic decode bytes; source requires reprojection".into()))}),
            Err(error)=>return Err(error.into()),
        };
        let mut captured_payloads = imbl::OrdMap::new();
        let mut payload_bytes = 0usize;
        for _ in 0..self.limits.max_resolution_passes {
            let mut archive = archive_context(state);
            archive.captured_payloads = captured_payloads.clone();
            let input = admitted.clone();
            let source_state = original.clone();
            let raw = raw.clone();
            let limits = self.limits.clone();
            let mapper = mapper(vendor)?;
            #[cfg(test)]
            let mapping_threads = self.mapping_threads.clone();
            let permit = self
                .cpu
                .clone()
                .acquire_owned()
                .await
                .map_err(|error| WorkerError::Task(error.to_string()))?;
            let mapped=tokio::task::spawn_blocking(move||{
                let _permit=permit;
                match budget::validate(&raw,&limits){
                    Err(budget::Failure::ResourceLimit)=>return RecordMapping{output:MapOutput::default(),gap:Some((DiagnosticCode::ResourceLimit,"JSON structure or identity exceeds semantic budget; source requires reprojection".into()))},
                    Err(budget::Failure::Malformed)=>return RecordMapping{output:MapOutput::default(),gap:Some((DiagnosticCode::DecodeFailure,"Malformed retained JSON creates an unresolved source gap; source requires reprojection".into()))},
                    Ok(())=>{}
                }
                #[cfg(test)]
                mapping_threads.lock().unwrap().push(std::thread::current().id());
                let output=mapper.map(MapInput{capture:&input.capture,canonical_observation:&input.canonical_observation,raw:&raw,state:&source_state,archive:&archive});
                if output.claims.len()>limits.max_claims_per_observation{return RecordMapping{output:MapOutput::default(),gap:Some((DiagnosticCode::ResourceLimit,"Semantic claim expansion exceeds its budget; source requires reprojection".into()))};}
                RecordMapping{output,gap:None}
            }).await.map_err(|error|WorkerError::Task(error.to_string()))?;
            let mut resolved = false;
            for request in &mapped.output.requests {
                if let ArchiveRequest::CapturedPayload { parent, slot } = request {
                    let key = mapping::captured_payload_key(parent, slot);
                    if !captured_payloads.contains_key(&key) {
                        if let Some(bindings) = state.captured_payloads.get(&key) {
                            let payloads: BTreeSet<_> = bindings
                                .iter()
                                .filter_map(|binding| binding.attachment.payload_ref.clone())
                                .collect();
                            let mut bytes = None;
                            let mut limited = false;
                            if payloads.len() == 1 {
                                let payload = payloads.first().unwrap();
                                match self
                                    .evidence
                                    .payload_bytes(
                                        payload,
                                        self.limits.max_decode_bytes.saturating_sub(payload_bytes),
                                    )
                                    .await
                                {
                                    Ok(value) => {
                                        payload_bytes += value.len();
                                        bytes = Some(value);
                                    }
                                    Err(EvidenceError::Limit(_)) => limited = true,
                                    Err(error) => return Err(error.into()),
                                }
                            }
                            captured_payloads.insert(
                                key,
                                mapping::CapturedPayload {
                                    bindings: bindings.iter().cloned().collect(),
                                    bytes,
                                    limited,
                                },
                            );
                            resolved = true;
                        }
                    }
                }
                if let ArchiveRequest::Observation { observation } = request {
                    let retained=self.evidence.receipt(observation).await?.is_some_and(|receipt|matches!(receipt.disposition,ReceiptDisposition::Retained{admission_seq}if admission_seq<=through_cut));
                    if retained {
                        resolved |= state
                            .available_evidence
                            .insert(observation.clone())
                            .is_none();
                        if !state.observation_sources.contains_key(observation) {
                            if let Some(position) =
                                self.evidence.observation_source(observation).await?
                            {
                                state
                                    .observation_sources
                                    .insert(observation.clone(), position);
                                resolved = true;
                            }
                        }
                    }
                }
            }
            if !resolved {
                return Ok(mapped);
            }
        }
        Err(WorkerError::Limit("archive dependency resolution"))
    }

    async fn apply_output(
        &self,
        context: OutputContext<'_>,
        output: MapOutput,
        unit: &mut ProjectionUnit,
    ) -> Result<()> {
        let admitted = context.admitted;
        let epoch = admitted.capture.ingestion_authorization_epoch.clone();
        for binding in &output.capture_payload_bindings {
            let key = mapping::captured_payload_key(&binding.parent, &binding.slot);
            let values = unit.next.captured_payloads.entry(key.clone()).or_default();
            if !values
                .iter()
                .any(|old| old.attachment == binding.attachment)
            {
                values.push_back(binding.clone());
            }
            unit.batch.mapper_states.insert(
                format!("captured_payload:{key}"),
                serde_json::to_value(values)?,
            );
        }

        for payload in &output.payloads {
            unit.batch.payload_derivations.push(PayloadDerivation {
                payload: payload.reference.clone(),
                evidence: payload.evidence.clone(),
                selector: payload.selector.clone(),
                rule: payload.rule.clone(),
                media_type: payload.media_type.clone(),
                encoding: payload.encoding,
            });
        }
        for agent in output.bound_agents {
            unit.bindings
                .push((epoch.clone(), admitted.canonical_observation.clone(), agent));
        }
        unit.pins.extend(
            output
                .claims
                .iter()
                .flat_map(|claim| claim.fact.payload_refs())
                .cloned()
                .map(|payload| (payload, epoch.clone())),
        );
        for claim in &output.claims {
            for evidence in &claim.evidence {
                if !unit.next.available_evidence.contains(evidence) {
                    if self.evidence.receipt(evidence).await?.is_some_and(|receipt|matches!(receipt.disposition,ReceiptDisposition::Retained{admission_seq}if admission_seq<=unit.batch.through_cut)){unit.next.available_evidence.insert(evidence.clone());}
                    else{return Err(WorkerError::Reduction(sylph_trace::reducer::ReductionError::MissingEvidence));}
                }
            }
        }
        unit.next.reducer.apply(
            &output.claims,
            &unit.next.available_evidence,
            self.limits.max_claims_per_observation,
        )?;
        unit.batch.claims.extend(output.claims);
        for definition in output.source_definitions {
            let key = (
                &definition.definition.source,
                definition.definition.generation,
            );
            let index_key = (
                definition.definition.source.clone(),
                definition.definition.generation,
            );
            if unit
                .next
                .source_definitions
                .get(&index_key)
                .is_some_and(|prior| prior.definition != definition.definition)
            {
                return Err(WorkerError::CorrelationConflict);
            }
            unit.batch.mapper_states.insert(
                format!("source_definition:{}", serde_json::to_string(&key)?),
                serde_json::to_value(&definition)?,
            );
            unit.next.source_definitions.insert(index_key, definition);
        }
        for record in output.history_records {
            let key = record.observation.encode();
            unit.batch.mapper_states.insert(
                format!("history_record:{key}"),
                serde_json::to_value(&record)?,
            );
            index_history_record(&mut unit.next.history_records, record)?;
        }
        for ownership in output.source_owners {
            let key = serde_json::to_string(&(&ownership.source, ownership.generation))?;
            let index_key = (ownership.source.clone(), ownership.generation);
            if let Some(prior) = unit.next.source_owners.get(&index_key) {
                if prior.agent != ownership.owner.agent {
                    return Err(WorkerError::CorrelationConflict);
                }
            }
            unit.next
                .available_evidence
                .extend(ownership.owner.evidence.iter().cloned());
            unit.batch
                .mapper_states
                .insert(format!("owner:{key}"), serde_json::to_value(&ownership)?);
            unit.next.source_owners.insert(index_key, ownership.owner);
        }
        for (key, reference) in output.correlations {
            if unit
                .next
                .correlations
                .get(&key)
                .is_some_and(|prior| prior != &reference)
            {
                return Err(WorkerError::CorrelationConflict);
            }
            let binding = CorrelationBinding {
                reference,
                evidence: output
                    .correlation_evidence
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| vec![admitted.canonical_observation.clone()]),
            };
            unit.batch.mapper_states.insert(
                format!("correlation:{key}"),
                serde_json::to_value(&binding)?,
            );
            unit.next
                .correlations
                .insert(key.clone(), binding.reference);
            unit.next.correlation_evidence.insert(key, binding.evidence);
        }
        let id = admitted.canonical_observation.encode();
        if output.requests.is_empty() {
            unit.next.pending.remove(&id);
            unit.batch
                .mapper_states
                .insert(format!("pending:{id}"), serde_json::Value::Null);
        } else {
            let pending = PendingMapping {
                admitted: admitted.clone(),
                state: output
                    .continuation_state
                    .unwrap_or_else(|| context.original_state.clone()),
                requests: output.requests,
                source_order: if output.blocks_source {
                    let proof = self
                        .catalogue
                        .scope_proof(&admitted.capture.ingestion_authorization_epoch)
                        .await?;
                    Some(source_key(
                        admitted,
                        mapper(mapper_vendor(&admitted.capture, &proof.scope))?.build(),
                    )?)
                } else {
                    None
                },
            };
            if let Some(key) = &pending.source_order {
                let cursor = unit
                    .next
                    .source_replays
                    .entry(key.clone())
                    .or_insert_with(|| SourceReplayCursor {
                        source: admitted.capture.source_position.source.clone(),
                        generation: admitted.capture.source_position.generation,
                        epoch: admitted.capture.ingestion_authorization_epoch.clone(),
                        after: admitted.admission_seq,
                        through: admitted.admission_seq,
                    });
                unit.batch.mapper_states.insert(
                    format!("source_replay:{key}"),
                    serde_json::to_value(cursor)?,
                );
            }
            self.save_pending(unit, pending)?;
        }
        if !output.diagnostics.is_empty() {
            unit.batch.mapper_states.insert(
                format!("diagnostic:{}", admitted.canonical_observation.encode()),
                serde_json::to_value(&output.diagnostics)?,
            );
            unit.diagnostics.extend(output.diagnostics);
        } else {
            // Pending ownership/correlation diagnostics describe the current
            // mapping attempt. A later proved resolution clears that status;
            // immutable source diagnostic claims remain in the reducer.
            unit.batch.mapper_states.insert(
                format!("diagnostic:{}", admitted.canonical_observation.encode()),
                serde_json::Value::Null,
            );
        }
        unit.payloads.extend(
            output
                .payloads
                .into_iter()
                .map(|payload| (epoch.clone(), payload)),
        );
        Ok(())
    }

    fn save_pending(&self, unit: &mut ProjectionUnit, pending: PendingMapping) -> Result<()> {
        let id = pending.admitted.canonical_observation.encode();
        unit.batch
            .mapper_states
            .insert(format!("pending:{id}"), serde_json::to_value(&pending)?);
        unit.next.pending.insert(id, Arc::new(pending));
        if unit.next.pending.len() > self.limits.max_pending {
            return Err(WorkerError::Limit("pending mappings"));
        }
        Ok(())
    }

    async fn replay_source_tails(&self, unit: &mut ProjectionUnit, through: u64) -> Result<()> {
        let keys: Vec<_> = unit.next.source_replays.keys().cloned().collect();
        let mut replayed = 0usize;
        let mut input_bytes = 0usize;
        for key in keys {
            if unit
                .next
                .pending
                .values()
                .any(|pending| pending.source_order.as_ref() == Some(&key))
            {
                continue;
            }
            if unit.next.gaps.contains_key(&key) {
                unit.next.source_replays.remove(&key);
                unit.batch
                    .mapper_states
                    .insert(format!("source_replay:{key}"), serde_json::Value::Null);
                continue;
            }
            loop {
                if replayed >= self.limits.max_unit_observations
                    || input_bytes >= self.limits.max_unit_input_bytes
                {
                    break;
                }
                let cursor = unit.next.source_replays[&key].clone();
                let record = self
                    .evidence
                    .source_record_after(
                        &cursor.source,
                        cursor.generation,
                        &cursor.epoch,
                        cursor.after,
                        cursor.through.min(through),
                    )
                    .await?;
                let Some(record) = record else {
                    unit.next.source_replays.remove(&key);
                    unit.batch
                        .mapper_states
                        .insert(format!("source_replay:{key}"), serde_json::Value::Null);
                    break;
                };
                let length = self
                    .evidence
                    .payload_length(&record.capture.payload_ref)
                    .await?
                    .unwrap_or(self.limits.max_decode_bytes as u64)
                    as usize;
                if replayed > 0
                    && input_bytes.saturating_add(length) > self.limits.max_unit_input_bytes
                {
                    break;
                }
                let admitted = AdmittedMapping::from(record);
                let proof = self
                    .catalogue
                    .scope_proof(&admitted.capture.ingestion_authorization_epoch)
                    .await?;
                let vendor = mapper_vendor(&admitted.capture, &proof.scope);
                if source_key(&admitted, mapper(vendor)?.build())? != key {
                    return Err(WorkerError::CorrelationConflict);
                }
                let original = unit
                    .next
                    .source_states
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let mapped = self
                    .map_record(&admitted, &original, &mut unit.next, through, vendor)
                    .await?;
                let blocked = mapped.output.blocks_source;
                if let Some((code, reason)) = mapped.gap {
                    unit.gap(&key, &admitted, code, reason)?;
                    unit.next.source_replays.remove(&key);
                    unit.batch
                        .mapper_states
                        .insert(format!("source_replay:{key}"), serde_json::Value::Null);
                    break;
                }
                unit.scopes
                    .push((cursor.epoch.clone(), admitted.admission_seq));
                if !blocked {
                    save_source_state(unit, &key, &mapped.output.state)?;
                }
                self.apply_output(
                    OutputContext {
                        admitted: &admitted,
                        original_state: &original,
                    },
                    mapped.output,
                    unit,
                )
                .await?;
                let cursor = unit.next.source_replays.get_mut(&key).unwrap();
                cursor.after = admitted.admission_seq;
                unit.batch.mapper_states.insert(
                    format!("source_replay:{key}"),
                    serde_json::to_value(cursor)?,
                );
                replayed += 1;
                input_bytes = input_bytes.saturating_add(length);
                if blocked {
                    break;
                }
            }
            if replayed >= self.limits.max_unit_observations
                || input_bytes >= self.limits.max_unit_input_bytes
            {
                break;
            }
        }
        if replayed > 0 {
            self.reduce_unit(unit).await?;
        }
        Ok(())
    }

    async fn stage_derived(
        &self,
        generation: &GenerationStore,
        epoch: &IngestionEpoch,
        payload: &DerivedPayload,
    ) -> Result<()> {
        let mut references = vec![payload.reference.clone()];
        if let Some(manifest) = &payload.manifest {
            references.extend(manifest.chunks.iter().map(|chunk| chunk.digest.clone()));
        }
        let mut tickets = Vec::new();
        for reference in references {
            tickets.push(
                self.catalogue
                    .begin_upload(
                        PinOwnerKind::Generation,
                        &generation.manifest().generation,
                        reference,
                        epoch.clone(),
                        crate::models::now_ms(),
                    )
                    .await?,
            );
        }
        if let Some(manifest) = &payload.manifest {
            for chunk in &manifest.chunks {
                let start = usize::try_from(chunk.payload_offset)
                    .map_err(|_| WorkerError::Limit("derived payload offset"))?;
                let end = start
                    .checked_add(chunk.length as usize)
                    .ok_or(WorkerError::Limit("derived payload range"))?;
                let bytes = payload
                    .bytes
                    .get(start..end)
                    .ok_or(WorkerError::Limit("derived payload range"))?;
                if self.evidence.stage_payload(bytes).await? != chunk.digest {
                    return Err(WorkerError::Task("derived chunk digest mismatch".into()));
                }
            }
            if self.evidence.stage_manifest(manifest).await? != payload.reference {
                return Err(WorkerError::Task("derived manifest mismatch".into()));
            }
        } else if self.evidence.stage_payload(&payload.bytes).await? != payload.reference {
            return Err(WorkerError::Task("derived bytes mismatch".into()));
        }
        for ticket in tickets {
            self.catalogue.finish_upload(&ticket).await?;
        }
        Ok(())
    }

    async fn materialize(
        &self,
        generation: &GenerationStore,
        before: &WorkerState,
        unit: &mut ProjectionUnit,
    ) -> Result<()> {
        let changes = std::mem::take(&mut unit.reduction_changes);
        self.refresh_provenance(unit, &changes).await?;
        let old = &before.reduction;
        let new = &unit.next.reduction;
        // Redelivery and acquisition-only metadata do not change a reader. Keep
        // the exact committed materialization; payload availability and new epoch
        // bindings remain independent inputs even when semantic values agree.
        if changes.is_empty()
            && unit.payloads.is_empty()
            && !unit
                .batch
                .mapper_states
                .keys()
                .any(|key| key.starts_with("object_provenance:"))
            && unit.next.reading.is_some()
        {
            return Ok(());
        }
        let batch = &mut unit.batch;
        for (reference, change) in &changes.objects.changed {
            let resolved = &change.after;
            if let Some(object) = resolved.declaration.known() {
                let (preview, preview_truncated) = object_preview(
                    &self.evidence,
                    object,
                    generation.limits().max_preview_bytes,
                    &unit.payloads,
                )
                .await?;
                batch.objects.push(ObjectUpdate::Put {
                    object: Box::new(object.clone()),
                    preview,
                    preview_truncated,
                });
            } else {
                batch.objects.push(ObjectUpdate::Withdraw {
                    object_ref: reference.clone(),
                    reason: "Conflicting declaration evidence".into(),
                });
            }
        }
        for reference in changes.objects.removed.keys() {
            batch.objects.push(ObjectUpdate::Withdraw {
                object_ref: reference.clone(),
                reason: "Resolved to another issued reference".into(),
            });
        }
        let previous_count = batch.memberships.len();
        let tails = unit
            .append
            .as_ref()
            .filter(|proof| proof.matches(old, new))
            .map(IndependentAppend::agents)
            .or_else(|| {
                unit.message_update
                    .as_ref()
                    .filter(|proof| proof.matches(old, new))
                    .map(MessageUpdate::agents)
            });
        if let Some(tails) = tails {
            for (agent, tail) in tails {
                for (offset, entry) in tail.added_entries().iter().enumerate() {
                    unit.membership_work.entries_examined += 1;
                    let ordinal = tail
                        .previous_archive_len()
                        .checked_add(offset)
                        .ok_or(WorkerError::Limit("archive membership ordinal"))?;
                    batch.memberships.push(MembershipChange {
                        scope: QueryScope::archive(agent.clone()),
                        object_ref: AnyObjectRef::HistoryEntry(entry.clone()),
                        order_key: (ordinal as u64).to_be_bytes().to_vec(),
                        present: true,
                    });
                }
            }
        // These are the only declaration families consumed by memberships().
        // Epoch, payload and lifecycle-only changes leave every collection intact.
        } else if !changes.histories.is_empty()
            || changes
                .objects
                .changed
                .keys()
                .chain(changes.objects.removed.keys())
                .any(|reference| {
                    matches!(
                        reference,
                        AnyObjectRef::Activation(_) | AnyObjectRef::Operation(_)
                    )
                })
        {
            unit.membership_work.full_reconciliations += 1;
            let old_members = memberships(old, &mut unit.membership_work)?;
            let new_members = memberships(new, &mut unit.membership_work)?;
            for (key, value) in &new_members {
                if old_members
                    .get(key)
                    .is_none_or(|old| old.order_key != value.order_key)
                {
                    batch.memberships.push(value.clone());
                }
            }
            for (key, value) in old_members {
                if !new_members.contains_key(&key) {
                    batch.memberships.push(MembershipChange {
                        present: false,
                        ..value
                    });
                }
            }
        }
        unit.membership_work.changes_emitted += batch.memberships.len() - previous_count;
        self.materialize_reading(generation, before, &changes, unit)
            .await?;
        Ok(())
    }
}

fn mapper_vendor<'a>(capture: &Capture, scope: &'a IngestionScope) -> &'a str {
    match capture.transport_kind {
        TransportKind::Control => "server",
        TransportKind::SourceInventory => "inventory",
        TransportKind::SourceAttachment => "attachment",
        TransportKind::SourceState | TransportKind::Supervisor => "observation",
        _ => scope.vendor().as_str(),
    }
}

fn mapper(vendor: &str) -> Result<Box<dyn SemanticMapper>> {
    match vendor {
        "inventory" => Ok(Box::new(mapping::inventory::InventoryMapper)),
        "attachment" => Ok(Box::new(mapping::attachment::AttachmentMapper)),
        "observation" => Ok(Box::new(mapping::observation::ObservationMapper)),
        "server" => Ok(Box::new(mapping::server::ServerMapper)),
        "codex" => Ok(Box::new(mapping::codex::CodexMapper)),
        "claude" => Ok(Box::new(mapping::claude::ClaudeMapper)),
        "pi" => Ok(Box::new(mapping::pi::PiMapper)),
        _ => Err(WorkerError::MapperBuild(vendor.into())),
    }
}

fn source_key(admitted: &AdmittedMapping, build: &str) -> Result<String> {
    Ok(serde_json::to_string(&(
        &admitted.capture.source_position.source,
        admitted.capture.source_position.generation,
        &admitted.capture.ingestion_authorization_epoch,
        build,
    ))?)
}

fn archive_context(state: &mut WorkerState) -> ArchiveContext {
    let mut archive = ArchiveContext {
        observations: state.available_evidence.clone(),
        observation_sources: state.observation_sources.clone(),
        source_owners: state.source_owners.clone(),
        source_definitions: state.source_definitions.clone(),
        history_records: state.history_records.clone(),
        correlations: state.correlations.clone(),
        correlation_evidence: state.correlation_evidence.clone(),
        claims: state.reducer.claim_snapshot(),
        ..ArchiveContext::default()
    };
    // Reduction advances this cache before publication. Initial/restored state
    // builds it once on demand; each mapper input retains immutable shared roots.
    let cached = state
        .archive_semantic
        .as_ref()
        .and_then(|cache| cache.get(&state.reduction));
    archive.semantic = if let Some(semantic) = cached {
        semantic
    } else {
        let cache = semantic::SemanticCache::rebuild(
            state.reduction.clone(),
            &state.reducer.claim_snapshot(),
            &mut state.semantic_work,
        );
        let semantic = cache
            .get(&state.reduction)
            .expect("rebuilt cache binds this reduction");
        state.archive_semantic = Some(cache);
        semantic
    };
    archive
}

fn index_history_record(
    records: &mut mapping::codex::history::HistoryRecords,
    record: mapping::codex::history::HistoryRecord,
) -> Result<()> {
    let SourceLocator::Jsonl { record_ordinal, .. } = record.position.locator else {
        return Err(WorkerError::Task(
            "Native history witness lacks a JSONL coordinate".into(),
        ));
    };
    let source = records
        .entry((record.position.source.clone(), record.position.generation))
        .or_default();
    if source.get(&record_ordinal).is_some_and(|prior| {
        prior.position != record.position
            || prior.vendor_ordinal != record.vendor_ordinal
            || prior.header != record.header
    }) {
        return Err(WorkerError::Task(
            "Conflicting retained witnesses for a native history coordinate".into(),
        ));
    }
    // Only agreeing lexical witnesses may share a coordinate. Proof selection
    // remains deterministic across admission and restart without choosing one
    // plausible byte range or header from contradictory source evidence.
    if source
        .get(&record_ordinal)
        .is_none_or(|prior| prior.observation.encode() <= record.observation.encode())
    {
        source.insert(record_ordinal, record);
    }
    Ok(())
}

fn save_source_state(unit: &mut ProjectionUnit, key: &str, state: &MapperState) -> Result<()> {
    unit.next
        .source_states
        .insert(key.into(), Arc::new(state.clone()));
    unit.batch
        .mapper_states
        .insert(format!("source:{key}"), serde_json::to_value(state)?);
    Ok(())
}
fn has_ready_continuations(
    all: &imbl::OrdMap<String, Arc<PendingMapping>>,
    replays: &imbl::OrdMap<String, SourceReplayCursor>,
) -> bool {
    all.values()
        .any(|pending| pending.requests.contains(&ArchiveRequest::ContinueHistory))
        || replays.keys().any(|key| {
            !all.values()
                .any(|pending| pending.source_order.as_ref() == Some(key))
        })
}

fn requests_may_resolve(requests: &[ArchiveRequest], state: &WorkerState) -> bool {
    requests.iter().any(|request|match request {
        ArchiveRequest::CapturedPayload {parent,slot}=>state.captured_payloads.contains_key(&mapping::captured_payload_key(parent,slot)),
        ArchiveRequest::ContinueHistory=>true,
        ArchiveRequest::HistoryLocator {issuer,physical_id}=>state.source_definitions.values().any(|source|&source.definition.source.issuer==issuer&&mapping::codex::history::physical_id(&source.definition.locator)==Some(physical_id.as_str())),
        ArchiveRequest::HistorySource {source,generation}=>state.history_records.contains_key(&(source.clone(), *generation)),
        ArchiveRequest::AttachmentTarget {parent,slot,locator}=>state.correlations.contains_key(&mapping::attachment_key(parent,slot,locator)),
        ArchiveRequest::Object {object_ref}=>state.reduction.object(object_ref).is_some(),
        ArchiveRequest::HistoryContent { entry, position, mapping_rule } => state.reduction.history_contents.get(entry)
            .is_some_and(|observations| observations.values().any(|observation| &observation.position == position && &observation.mapping_rule == mapping_rule)),
        ArchiveRequest::Observation {observation}=>state.available_evidence.contains(observation),
        ArchiveRequest::CommandIdentity {issuer,command_id}=>state.correlations.contains_key(&mapping::command_identity_key(issuer,command_id)),
        ArchiveRequest::Correlation {agent,namespace,key}=>state.correlations.contains_key(&mapping::correlation_key(agent,namespace,key)),
        ArchiveRequest::Agent {agent}=>state.reduction.object(&AnyObjectRef::Agent(agent.clone())).is_some(),
        ArchiveRequest::Source {source,generation}=>state.source_owners.contains_key(&(source.clone(), *generation))||state.reducer.claims().any(|claim|matches!(&claim.fact,Fact::HistoryMappingComplete {extent} if &extent.source==source && extent.generation==*generation)),
    })
}

fn memberships(
    reduction: &Reduction,
    work: &mut MembershipWork,
) -> Result<BTreeMap<(String, AnyObjectRef), MembershipChange>> {
    let mut result = BTreeMap::new();
    let mut add = |scope: QueryScope, reference: AnyObjectRef, index: usize| -> Result<()> {
        let key = serde_json::to_string(&scope)?;
        result.insert(
            (key, reference.clone()),
            MembershipChange {
                scope,
                object_ref: reference,
                order_key: (index as u64).to_be_bytes().to_vec(),
                present: true,
            },
        );
        Ok(())
    };
    for (agent, history) in &reduction.histories {
        for (index, entry) in history.archive_order.iter().enumerate() {
            work.entries_examined += 1;
            add(
                QueryScope::archive(agent.clone()),
                AnyObjectRef::HistoryEntry(entry.clone()),
                index,
            )?;
        }
        if let Some((selection, entries)) = history
            .selection
            .known()
            .zip(history.selected_entries.as_ref())
        {
            for (index, entry) in entries.iter().enumerate() {
                work.entries_examined += 1;
                add(
                    QueryScope::history(agent.clone(), selection.clone()),
                    AnyObjectRef::HistoryEntry(entry.clone()),
                    index,
                )?;
            }
        }
    }
    let mut activation_counts = BTreeMap::<AgentRef, usize>::new();
    work.objects_examined += reduction.objects.len();
    for object in reduction
        .objects
        .values()
        .filter_map(|object| object.declaration.known())
    {
        match object {
            TraceObject::Activation(activation) => {
                if let OwnerRef::Agent { agent } = &activation.owner {
                    let count = activation_counts.entry(agent.clone()).or_default();
                    add(
                        QueryScope {
                            collection: Collection::Activations {
                                agent: agent.clone(),
                            },
                            history_mode: HistoryMode::Authored,
                            representation: None,
                            filter: MembershipFilter::All,
                        },
                        AnyObjectRef::Activation(activation.activation_ref.clone()),
                        *count,
                    )?;
                    *count += 1;
                }
            }
            TraceObject::Operation(operation) => {
                if let Some(activation) = &operation.activation {
                    add(
                        QueryScope {
                            collection: Collection::ObjectRelations {
                                object: AnyObjectRef::Activation(activation.clone()),
                                relation: RelationFilter::Operation,
                            },
                            history_mode: HistoryMode::RetainedArchive,
                            representation: None,
                            filter: MembershipFilter::All,
                        },
                        AnyObjectRef::Operation(operation.operation_ref.clone()),
                        0,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

fn utf8_prefix(bytes: &[u8], limit: usize) -> (String, bool) {
    let end = bytes.len().min(limit);
    let bounded = &bytes[..end];
    match std::str::from_utf8(bounded) {
        Ok(text) => (text.into(), bytes.len() > end),
        Err(error) => {
            // Keep exact valid prefix bytes. A cut through a multibyte character
            // is not a replacement character observed in the source.
            let valid = error.valid_up_to();
            (
                std::str::from_utf8(&bounded[..valid])
                    .expect("validated UTF-8 prefix")
                    .into(),
                true,
            )
        }
    }
}

async fn object_preview(
    evidence: &EvidenceStore,
    object: &TraceObject,
    limit: usize,
    staged: &[(IngestionEpoch, DerivedPayload)],
) -> Result<(String, bool)> {
    match object {
        TraceObject::Agent(agent) => Ok(utf8_prefix(
            agent.name.as_deref().unwrap_or("").as_bytes(),
            limit,
        )),
        TraceObject::Operation(operation) => Ok(utf8_prefix(
            match &operation.category {
                OperationCategory::Tool { name, .. }
                | OperationCategory::Extension { name, .. } => name.as_bytes(),
                _ => &[],
            },
            limit,
        )),
        TraceObject::MessageRevision(revision) => {
            let mut preview = String::new();
            let mut truncated = false;
            for part in revision
                .parts
                .iter()
                .filter(|part| part.kind == ContentKind::Text)
            {
                let remaining = limit.saturating_sub(preview.len());
                let (text, cut) = if let Some((_, payload)) = staged
                    .iter()
                    .find(|(_, payload)| payload.reference == part.payload)
                {
                    utf8_prefix(&payload.bytes, remaining)
                } else {
                    let length = evidence.payload_length(&part.payload).await?;
                    let requested =
                        length.unwrap_or(remaining as u64).min(remaining as u64) as usize;
                    if requested == 0 {
                        (String::new(), length != Some(0))
                    } else {
                        let slice = evidence.payload_range(&part.payload, 0, requested).await?;
                        let (text, cut) = utf8_prefix(&slice.bytes, remaining);
                        (
                            text,
                            cut || length.is_none_or(|length| length > requested as u64),
                        )
                    }
                };
                preview.push_str(&text);
                truncated |= cut;
                // A partial part must not be followed by later parts, which would
                // concatenate around an omitted interval and present altered text.
                if cut {
                    break;
                }
            }
            Ok((preview, truncated))
        }
        _ => Ok((String::new(), false)),
    }
}

#[async_trait::async_trait]
impl super::publication::ProjectionCatchUp for ProjectionWorker {
    async fn catch_up(
        &self,
        generation: &GenerationStore,
        writer_token: u64,
        through_cut: u64,
    ) -> anyhow::Result<()> {
        ProjectionWorker::catch_up(self, generation, writer_token, through_cut).await?;
        Ok(())
    }

    async fn validate_generation(
        &self,
        generation: &GenerationStore,
        snapshot: &Snapshot,
    ) -> anyhow::Result<()> {
        let mut after = None::<(PayloadRef, IngestionEpoch)>;
        let mut pinned = BTreeSet::new();
        loop {
            let pins = self
                .catalogue
                .payload_pins(
                    PinOwnerKind::Generation,
                    &generation.manifest().generation,
                    after.as_ref().map(|(payload, epoch)| (payload, epoch)),
                    1,
                )
                .await?;
            if pins.is_empty() {
                break;
            }
            for pin in pins {
                self.catalogue.verify_payload(&pin).await?;
                pinned.insert(pin.payload.clone());
                after = Some((pin.payload, pin.epoch));
            }
        }
        let mut cursor = None;
        let mut checked = BTreeSet::new();
        loop {
            let page = generation
                .retained_claims(
                    snapshot,
                    cursor.as_ref(),
                    generation.limits().max_candidates,
                    generation.limits().max_batch_bytes,
                )
                .await?;
            for claim in page.claims {
                for payload in claim.fact.payload_refs() {
                    if !pinned.contains(payload) {
                        return Err(WorkerError::Task(
                            "projection references an unpinned payload".into(),
                        )
                        .into());
                    }
                }
                for observation in claim.evidence {
                    if checked.insert(observation.clone()) && !self.evidence.receipt(&observation).await?.is_some_and(|receipt|matches!(receipt.disposition,ReceiptDisposition::Retained { admission_seq } if admission_seq<=snapshot.evidence_cut)) {return Err(WorkerError::Task("claim evidence is outside the publication cut".into()).into());}
                }
            }
            if !page.has_more {
                break;
            }
            cursor = page.next;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
