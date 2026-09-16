//! Deterministic field resolution over a fixed set of retained, evidenced claims.
//!
//! Claims remain indexed by issued identity. Corrections and equivalences are derived
//! views, so withdrawing an interpretation can split a previously joined class without
//! rewriting evidence or the addresses that a reader saved.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::{algo::kosaraju_scc, graph::DiGraph};
use serde::{Deserialize, Serialize};
use sylph_protocol::trace::*;
mod content;
pub use content::ResolvedEntryContents;

macro_rules! resolved_ref {
    ($registry:expr,$kind:ident,$reference:expr) => {{
        let original = AnyObjectRef::$kind($reference.clone());
        match $registry.get(&original) {
            Some(AnyObjectRef::$kind(value)) => value.clone(),
            _ => $reference.clone(),
        }
    }};
}

fn canonical(
    registry: &BTreeMap<AnyObjectRef, AnyObjectRef>,
    reference: &AnyObjectRef,
) -> AnyObjectRef {
    registry
        .get(reference)
        .cloned()
        .unwrap_or_else(|| reference.clone())
}

mod references;
use references::normalized_claim;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReductionError {
    #[error("claim identity was reused for incompatible facts")]
    ClaimCollision,
    #[error("a projection batch exceeds its declared claim bound")]
    ClaimLimit,
    #[error("a claim cites evidence outside the supplied archive cut")]
    MissingEvidence,
    #[error("invalid claim: {0}")]
    InvalidClaim(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Resolved<T> {
    Known {
        value: T,
        claims: Vec<ClaimRef>,
    },
    Unknown,
    Conflicted {
        alternatives: Vec<(T, Vec<ClaimRef>)>,
    },
}

impl<T> Resolved<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObject {
    pub reference: AnyObjectRef,
    pub declaration: Resolved<TraceObject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionState {
    pub started: Vec<ClaimRef>,
    pub terminated: Vec<ClaimRef>,
    pub outcome: Resolved<TerminalOutcome>,
    pub interrupt_requests: Vec<ClaimRef>,
    pub boundaries: Vec<(ExecutionBoundary, ClaimRef)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReductionDiagnostic {
    pub code: DiagnosticCode,
    pub claims: Vec<ClaimRef>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRelation {
    pub claim: ClaimRef,
    pub relation: Relation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingOrderBasis {
    NativePredecessor,
    ExplicitOrder,
    PresentationTiebreak,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryView {
    pub agent: AgentRef,
    pub selection: Resolved<SelectionRef>,
    pub candidate_heads: Vec<EntryRef>,
    pub archive_order: Vec<EntryRef>,
    pub order_basis: BTreeMap<EntryRef, ReadingOrderBasis>,
    /// None means there is no proven selected head/membership, not an empty history.
    pub selected_entries: Option<Vec<EntryRef>>,
    pub unresolved_entries: Vec<EntryRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHistory {
    pub revisions: Vec<MessageRevisionRef>,
    pub selected: Resolved<MessageRevisionRef>,
    pub sealed: bool,
    pub late_segments: Vec<MessageRevisionRef>,
    pub conflicted_revisions: Vec<MessageRevisionRef>,
    candidates: Vec<RevisionCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RevisionCandidate {
    reference: MessageRevisionRef,
    form: RevisionForm,
    position: Option<SourcePosition>,
    claim: ClaimRef,
}
impl MessageHistory {
    /// Selection applies the same sealing and ordering rules to exactly its
    /// included revisions. A final elsewhere cannot choose this history's body.
    pub fn select_in(
        &self,
        included: &BTreeSet<MessageRevisionRef>,
    ) -> Resolved<MessageRevisionRef> {
        if self
            .conflicted_revisions
            .iter()
            .any(|reference| included.contains(reference))
        {
            return Resolved::Unknown;
        }
        select_revision(
            &self
                .candidates
                .iter()
                .filter(|value| included.contains(&value.reference))
                .collect::<Vec<_>>(),
        )
    }
}
fn select_revision(values: &[&RevisionCandidate]) -> Resolved<MessageRevisionRef> {
    let replaced: BTreeSet<_> = values
        .iter()
        .filter_map(|value| match &value.form {
            RevisionForm::Correction { replaces } => Some(replaces),
            _ => None,
        })
        .collect();
    let finals: Vec<_> = values
        .iter()
        .copied()
        .filter(|value| {
            !replaced.contains(&value.reference)
                && matches!(
                    value.form,
                    RevisionForm::Complete
                        | RevisionForm::Final { .. }
                        | RevisionForm::Correction { .. }
                )
        })
        .collect();
    let selected = if finals.is_empty() {
        values
            .iter()
            .copied()
            .filter(|a| {
                !values.iter().any(|b| {
                    a.position
                        .as_ref()
                        .zip(b.position.as_ref())
                        .is_some_and(|(a, b)| source_order(a, b) == Some(std::cmp::Ordering::Less))
                })
            })
            .collect()
    } else {
        finals
    };
    resolve_values(
        selected
            .into_iter()
            .map(|value| (value.reference.clone(), value.claim.clone())),
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reduction {
    pub objects: BTreeMap<AnyObjectRef, ResolvedObject>,
    pub representatives: BTreeMap<AnyObjectRef, AnyObjectRef>,
    pub executions: BTreeMap<AnyObjectRef, ExecutionState>,
    pub delivery_stages: BTreeMap<DeliveryRef, Vec<(DeliveryStage, ClaimRef)>>,
    pub delivery_revisions: BTreeMap<DeliveryRef, BTreeMap<ClaimRef, MessageRevisionRef>>,
    pub resource_states: BTreeMap<ResourceRef, Vec<(ResourceState, ClaimRef)>>,
    pub agent_states: BTreeMap<AgentRef, Vec<(AgentState, ClaimRef)>>,
    pub relations: Vec<ResolvedRelation>,
    pub histories: BTreeMap<AgentRef, HistoryView>,
    pub selection_memberships: BTreeMap<SelectionRef, Option<Vec<EntryRef>>>,
    pub history_contents: BTreeMap<EntryRef, BTreeMap<ClaimRef, HistoryContentObservation>>,
    pub selection_content_observations: BTreeMap<SelectionRef, Option<BTreeSet<ClaimRef>>>,
    pub messages: BTreeMap<MessageRef, MessageHistory>,
    pub coverage: Vec<(Coverage, ClaimRef)>,
    pub diagnostics: Vec<ReductionDiagnostic>,
    pub withdrawn_claims: Vec<ClaimRef>,
}

impl Reduction {
    pub fn object(&self, reference: &AnyObjectRef) -> Option<&ResolvedObject> {
        self.objects
            .get(&canonical(&self.representatives, reference))
    }
    pub fn execution(&self, reference: &AnyObjectRef) -> Option<&ExecutionState> {
        self.executions
            .get(&canonical(&self.representatives, reference))
    }
    pub fn message(&self, reference: &MessageRef) -> Option<&MessageHistory> {
        self.messages
            .get(&resolved_ref!(&self.representatives, Message, reference))
    }
    pub fn history(&self, reference: &AgentRef) -> Option<&HistoryView> {
        self.histories
            .get(&resolved_ref!(&self.representatives, Agent, reference))
    }
}

/// Checkpointable claim state. Batching and redelivery never affect resolved value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reducer {
    claims: imbl::OrdMap<ClaimRef, Claim>,
}

impl Reducer {
    pub fn len(&self) -> usize {
        self.claims.len()
    }
    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }
    /// Validate a whole bounded unit before mutating state. The worker advances its
    /// evidence cut only in the transaction that persists this state and its projection.
    pub fn apply(
        &mut self,
        claims: &[Claim],
        available_evidence: &imbl::OrdSet<ObservationRef>,
        max_claims: usize,
    ) -> Result<(), ReductionError> {
        if claims.len() > max_claims {
            return Err(ReductionError::ClaimLimit);
        }
        let mut incoming = BTreeMap::new();
        for claim in claims {
            claim
                .validate()
                .map_err(|error| ReductionError::InvalidClaim(error.to_string()))?;
            if claim
                .evidence
                .iter()
                .any(|evidence| !available_evidence.contains(evidence))
            {
                return Err(ReductionError::MissingEvidence);
            }
            if self
                .claims
                .get(&claim.claim_ref)
                .or_else(|| incoming.get(&claim.claim_ref))
                .is_some_and(|prior| prior != claim)
            {
                return Err(ReductionError::ClaimCollision);
            }
            incoming.insert(claim.claim_ref.clone(), claim.clone());
        }
        self.claims.extend(incoming);
        Ok(())
    }

    pub fn claims(&self) -> impl Iterator<Item = &Claim> {
        self.claims.values()
    }

    /// An immutable input snapshot shares retained claims with the next candidate.
    pub fn claim_snapshot(&self) -> imbl::OrdMap<ClaimRef, Claim> {
        self.claims.clone()
    }

    /// Rebuilding is pure. Incremental workers may resolve bounded affected scopes;
    /// this full reduction is also the reference path used to verify their checkpoints.
    pub fn resolve(&self) -> Reduction {
        let mut result = Reduction::default();
        let active = self.active_claims(&mut result);
        let original_declarations = declarations(&active, &mut result);
        resolve_equivalences(&active, &original_declarations, &mut result);
        let normalized: Vec<_> = active
            .iter()
            .map(|claim| normalized_claim(&result.representatives, claim))
            .collect();
        let active: Vec<_> = normalized.iter().collect();
        let declarations = declarations(&active, &mut result);
        resolve_objects(&declarations, &mut result);
        for claim in &active {
            if let Fact::HistoryContentObserved { observation } = &claim.fact {
                result.history_contents.entry(observation.entry.clone()).or_default()
                    .insert(claim.claim_ref.clone(), observation.clone());
            }
        }
        resolve_messages(&active, &mut result);
        resolve_execution(&active, &mut result);
        resolve_relations(&active, &mut result);
        resolve_histories(&active, &mut result);
        for claim in active {
            match &claim.fact {
                Fact::AgentState { agent, state } => result
                    .agent_states
                    .entry(agent.clone())
                    .or_default()
                    .push((state.clone(), claim.claim_ref.clone())),
                Fact::DeliveryStage { delivery, stage } => result
                    .delivery_stages
                    .entry(delivery.clone())
                    .or_default()
                    .push((stage.clone(), claim.claim_ref.clone())),
                Fact::DeliveryRevisionObserved { delivery, revision } => {
                    result.delivery_revisions.entry(delivery.clone()).or_default()
                        .insert(claim.claim_ref.clone(), revision.clone());
                }
                Fact::ResourceState { resource, state } => result
                    .resource_states
                    .entry(resource.clone())
                    .or_default()
                    .push((state.clone(), claim.claim_ref.clone())),
                Fact::Coverage { coverage } => result
                    .coverage
                    .push((coverage.clone(), claim.claim_ref.clone())),
                Fact::Diagnostic { diagnostic } => result.diagnostics.push(ReductionDiagnostic {
                    code: diagnostic.code,
                    claims: vec![claim.claim_ref.clone()],
                    message: diagnostic.message.clone(),
                }),
                _ => {}
            }
        }
        result
            .diagnostics
            .sort_by(|a, b| (&a.claims, &a.message).cmp(&(&b.claims, &b.message)));
        result
    }

    fn active_claims<'a>(&'a self, result: &mut Reduction) -> Vec<&'a Claim> {
        let mut correction_edges = Vec::new();
        for claim in self.claims.values() {
            for prior in &claim.supersedes {
                let Some(previous) = self.claims.get(prior) else {
                    diagnostic(
                        result,
                        DiagnosticCode::MissingSource,
                        vec![claim.claim_ref.clone(), prior.clone()],
                        "Correction awaits its referenced claim",
                    );
                    continue;
                };
                if field_key(&claim.fact) != field_key(&previous.fact) {
                    diagnostic(
                        result,
                        DiagnosticCode::InvalidRelation,
                        vec![claim.claim_ref.clone(), prior.clone()],
                        "Correction cannot withdraw another object's field",
                    );
                    continue;
                }
                correction_edges.push((
                    claim.claim_ref.clone(),
                    prior.clone(),
                    claim.claim_ref.clone(),
                ));
            }
        }
        let cyclic = cyclic_edge_claims(&correction_edges);
        let mut withdrawn = BTreeSet::new();
        for (new, old, _) in correction_edges {
            if cyclic.contains(&new) {
                diagnostic(
                    result,
                    DiagnosticCode::InvalidRelation,
                    vec![new, old],
                    "Cyclic correction remains conflicted",
                );
            } else {
                withdrawn.insert(old);
            }
        }
        result.withdrawn_claims = withdrawn.iter().cloned().collect();
        self.claims
            .values()
            .filter(|claim| !withdrawn.contains(&claim.claim_ref))
            .collect()
    }
}

fn field_key(fact: &Fact) -> String {
    // This key only groups typed facts for correction validation, never domain identity.
    // Source-specific field rules can supersede only their own family and subject.
    let value = match fact {
        Fact::Declare { object } => serde_json::json!(["declaration", object.object_ref()]),
        Fact::AgentState { agent, state } => {
            serde_json::json!(["agent_state", agent, variant_name(state)])
        }
        Fact::DeliveryStage { delivery, stage } => {
            serde_json::json!(["delivery", delivery, variant_name(stage)])
        }
        Fact::DeliveryRevisionObserved { delivery, .. } => serde_json::json!(["delivery_revision", delivery]),
        Fact::Execution {
            execution,
            boundary,
        } => serde_json::json!(["execution", execution, variant_name(boundary)]),
        Fact::ResourceState { resource, .. } => serde_json::json!(["resource", resource]),
        Fact::Relation { relation } => serde_json::json!([
            "relation",
            relation.subject_refs()[0],
            variant_name(relation)
        ]),
        Fact::SelectHistory {
            agent, selected_at, ..
        } => serde_json::json!(["selection", agent, selected_at.source]),
        Fact::HistoryPosition {
            entry,
            position,
            coordinate,
            ..
        } => serde_json::json!(["history_position", entry, position.source, coordinate]),
        Fact::HistoryMappingComplete { extent } => {
            serde_json::json!(["history_mapping_complete", extent])
        }
        Fact::HistoryContentObserved { observation } => serde_json::json!([
            "history_content", observation.entry, observation.position, observation.mapping_rule
        ]),
        Fact::Equivalence { left, right, .. } => {
            let (a, b) = if left <= right {
                (left, right)
            } else {
                (right, left)
            };
            serde_json::json!(["equivalence", a, b])
        }
        Fact::Coverage { coverage } => serde_json::json!([
            "coverage",
            coverage.subject,
            coverage.source,
            coverage.dimension,
            coverage.interval
        ]),
        Fact::Diagnostic { diagnostic } => {
            serde_json::json!(["diagnostic", diagnostic.subject, diagnostic.code])
        }
    };
    value.to_string()
}

fn variant_name(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("closed fact serialization")["kind"].clone()
}

fn diagnostic(
    result: &mut Reduction,
    code: DiagnosticCode,
    mut claims: Vec<ClaimRef>,
    message: &str,
) {
    claims.sort();
    claims.dedup();
    result.diagnostics.push(ReductionDiagnostic {
        code,
        claims,
        message: message.into(),
    });
}

fn declarations<'a>(
    active: &[&'a Claim],
    result: &mut Reduction,
) -> BTreeMap<AnyObjectRef, Vec<(&'a TraceObject, &'a Claim)>> {
    let mut declarations: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for claim in active {
        if let Fact::Declare { object } = &claim.fact {
            let reference = object.object_ref();
            if let Some(OwnerRef::Agent { agent }) = object_owner(object) {
                if agent.environment() != reference.scope().environment() {
                    diagnostic(
                        result,
                        DiagnosticCode::ConflictingOwner,
                        vec![claim.claim_ref.clone()],
                        "Owner lies outside the object's environment",
                    );
                    continue;
                }
            }
            declarations
                .entry(reference)
                .or_default()
                .push((object, *claim));
        }
    }
    declarations
}

fn object_owner(object: &TraceObject) -> Option<&OwnerRef> {
    match object {
        TraceObject::Activation(v) => Some(&v.owner),
        TraceObject::Operation(v) => Some(&v.owner),
        TraceObject::ContextCheckpoint(v) => Some(&v.owner),
        TraceObject::Message(v) => v.production.as_ref().map(|p| &p.owner),
        TraceObject::Resource(v) => v.host.as_ref(),
        _ => None,
    }
}

fn resolve_equivalences(
    active: &[&Claim],
    declarations: &BTreeMap<AnyObjectRef, Vec<(&TraceObject, &Claim)>>,
    result: &mut Reduction,
) {
    let mut edges = Vec::new();
    for claim in active {
        if let Fact::Equivalence { left, right, rule } = &claim.fact {
            let same_owner = declarations
                .get(left)
                .zip(declarations.get(right))
                .is_some_and(|(a, b)| {
                    let owners: Vec<_> = a
                        .iter()
                        .chain(b.iter())
                        .filter_map(|(object, _)| object_owner(object))
                        .collect();
                    (if owners.is_empty() {
                        left.scope() == right.scope()
                    } else {
                        true
                    }) && owners.windows(2).all(|pair| pair[0] == pair[1])
                        && owners
                            .iter()
                            .all(|owner| matches!(owner, OwnerRef::Agent { .. }))
                });
            // Equal scoped namespace plus an explicit claim is necessary; equal text
            // is never inspected. Missing declarations defer this join until evidence arrives.
            if !left.same_kind(right)
                || left.scope().environment() != right.scope().environment()
                || left.scope().anchor_session().agent != right.scope().anchor_session().agent
                || !same_owner
                || rule.trim().is_empty()
            {
                diagnostic(
                    result,
                    DiagnosticCode::ConflictingIdentity,
                    vec![claim.claim_ref.clone()],
                    "Equivalence lacks compatible declared identity and ownership",
                );
                continue;
            }
            edges.push((left.clone(), right.clone()));
        }
    }
    let mut adjacency: BTreeMap<AnyObjectRef, BTreeSet<AnyObjectRef>> = BTreeMap::new();
    for reference in declarations.keys() {
        adjacency.entry(reference.clone()).or_default();
    }
    for (left, right) in edges {
        adjacency
            .entry(left.clone())
            .or_default()
            .insert(right.clone());
        adjacency.entry(right).or_default().insert(left);
    }
    let mut visited = BTreeSet::new();
    for reference in adjacency.keys() {
        if visited.contains(reference) {
            continue;
        }
        let mut component = BTreeSet::new();
        let mut pending = vec![reference.clone()];
        while let Some(next) = pending.pop() {
            if !visited.insert(next.clone()) {
                continue;
            }
            component.insert(next.clone());
            pending.extend(adjacency[&next].iter().cloned());
        }
        let representative = component
            .first()
            .expect("component contains its seed")
            .clone();
        for member in component {
            result
                .representatives
                .insert(member, representative.clone());
        }
    }
}

fn resolve_objects(
    declarations: &BTreeMap<AnyObjectRef, Vec<(&TraceObject, &Claim)>>,
    result: &mut Reduction,
) {
    for (reference, values) in declarations {
        // Resolve fields across the canonical class. Original addresses and owner
        // claims remain in Reducer evidence; lookup through the registry preserves
        // every issued reference and a rebuild can split the class again.
        let declaration = resolve_declarations(values);
        if matches!(declaration, Resolved::Conflicted { .. }) {
            diagnostic(
                result,
                DiagnosticCode::ConflictingIdentity,
                values
                    .iter()
                    .map(|(_, claim)| claim.claim_ref.clone())
                    .collect(),
                "Object declaration has incompatible unsuperseded alternatives",
            );
        }
        result.objects.insert(
            reference.clone(),
            ResolvedObject {
                reference: reference.clone(),
                declaration,
            },
        );
    }
}

fn merge_optional<T: Clone + PartialEq>(a: &Option<T>, b: &Option<T>) -> Option<Option<T>> {
    match (a, b) {
        (Some(a), Some(b)) if a != b => None,
        (Some(a), _) => Some(Some(a.clone())),
        (_, Some(b)) => Some(Some(b.clone())),
        _ => Some(None),
    }
}

fn merge_author(a: &Author, b: &Author) -> Option<Author> {
    if a == b {
        return Some(a.clone());
    }
    match (a, b) {
        (Author::Unknown, b) => Some(b.clone()),
        (a, Author::Unknown) => Some(a.clone()),
        (Author::Human { subject: a }, Author::Human { subject: b }) => Some(Author::Human {
            subject: merge_optional(a, b)?,
        }),
        (Author::Harness { identity: a }, Author::Harness { identity: b }) => {
            Some(Author::Harness {
                identity: merge_optional(a, b)?,
            })
        }
        (Author::External { identity: a }, Author::External { identity: b }) => {
            Some(Author::External {
                identity: merge_optional(a, b)?,
            })
        }
        (Author::Tool { operation: a }, Author::Tool { operation: b }) => Some(Author::Tool {
            operation: merge_optional(a, b)?,
        }),
        _ => None,
    }
}

fn merge_production(
    a: &Option<ProductionScope>,
    b: &Option<ProductionScope>,
) -> Option<Option<ProductionScope>> {
    match (a, b) {
        (Some(a), Some(b)) if a.owner == b.owner => Some(Some(ProductionScope {
            owner: a.owner.clone(),
            activation: merge_optional(&a.activation, &b.activation)?,
        })),
        _ => merge_optional(a, b),
    }
}

/// These are individual unknown-to-known field rules, not a global evidence rank.
/// Immutable revision content and disagreeing known owners never enter this merge.
fn refine_declaration(a: &TraceObject, b: &TraceObject) -> Option<TraceObject> {
    if a == b {
        return Some(a.clone());
    }
    match (a, b) {
        (TraceObject::Agent(a), TraceObject::Agent(b))
            if a.agent_ref == b.agent_ref && a.vendor == b.vendor =>
        {
            Some(TraceObject::Agent(AgentThread {
                agent_ref: a.agent_ref.clone(),
                vendor: a.vendor.clone(),
                name: merge_optional(&a.name, &b.name)?,
                model: merge_optional(&a.model, &b.model)?,
            }))
        }
        (TraceObject::Message(a), TraceObject::Message(b)) if a.message_ref == b.message_ref => {
            let purpose = match (a.purpose, b.purpose) {
                (a, b) if a == b => a,
                (MessagePurpose::Unclassified, b) => b,
                (a, MessagePurpose::Unclassified) => a,
                _ => return None,
            };
            Some(TraceObject::Message(Message {
                message_ref: a.message_ref.clone(),
                author: merge_author(&a.author, &b.author)?,
                protocol_role: merge_optional(&a.protocol_role, &b.protocol_role)?,
                purpose,
                production: merge_production(&a.production, &b.production)?,
            }))
        }
        (TraceObject::Operation(a), TraceObject::Operation(b))
            if a.operation_ref == b.operation_ref
                && a.owner == b.owner
                && a.category == b.category =>
        {
            Some(TraceObject::Operation(Operation {
                operation_ref: a.operation_ref.clone(),
                owner: a.owner.clone(),
                activation: merge_optional(&a.activation, &b.activation)?,
                requester: merge_optional(&a.requester, &b.requester)?,
                category: a.category.clone(),
                input_revision: merge_optional(&a.input_revision, &b.input_revision)?,
                argument_state: if a.argument_state == ArgumentState::Sealed
                    || b.argument_state == ArgumentState::Sealed
                {
                    ArgumentState::Sealed
                } else {
                    ArgumentState::Collecting
                },
            }))
        }
        (TraceObject::Resource(a), TraceObject::Resource(b))
            if a.resource_ref == b.resource_ref && a.kind == b.kind =>
        {
            Some(TraceObject::Resource(Resource {
                resource_ref: a.resource_ref.clone(),
                creator: merge_optional(&a.creator, &b.creator)?,
                host: merge_optional(&a.host, &b.host)?,
                kind: a.kind.clone(),
            }))
        }
        (TraceObject::HistoryEntry(a), TraceObject::HistoryEntry(b))
            if a.entry_ref == b.entry_ref
                && a.containing_agent == b.containing_agent =>
        {
            Some(TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: a.entry_ref.clone(),
                containing_agent: a.containing_agent.clone(),
                predecessor: merge_optional(&a.predecessor, &b.predecessor)?,
                original_owner: merge_optional(&a.original_owner, &b.original_owner)?,
            }))
        }
        _ => None,
    }
}

fn resolve_declarations(values: &[(&TraceObject, &Claim)]) -> Resolved<TraceObject> {
    let Some((first, _)) = values.first() else {
        return Resolved::Unknown;
    };
    let mut merged = (*first).clone();
    for (next, _) in values.iter().skip(1) {
        let Some(refined) = refine_declaration(&merged, next) else {
            return resolve_values(
                values
                    .iter()
                    .map(|(object, claim)| ((*object).clone(), claim.claim_ref.clone())),
            );
        };
        merged = refined;
    }
    Resolved::Known {
        value: merged,
        claims: values
            .iter()
            .map(|(_, claim)| claim.claim_ref.clone())
            .collect(),
    }
}

fn resolve_values<T: Clone + PartialEq>(
    values: impl Iterator<Item = (T, ClaimRef)>,
) -> Resolved<T> {
    let mut alternatives: Vec<(T, Vec<ClaimRef>)> = Vec::new();
    for (value, claim) in values {
        if let Some((_, claims)) = alternatives.iter_mut().find(|(prior, _)| prior == &value) {
            claims.push(claim);
        } else {
            alternatives.push((value, vec![claim]));
        }
    }
    match alternatives.len() {
        0 => Resolved::Unknown,
        1 => {
            let (value, claims) = alternatives.pop().unwrap();
            Resolved::Known { value, claims }
        }
        _ => Resolved::Conflicted { alternatives },
    }
}

fn claim_position(claim: &Claim) -> Option<&SourcePosition> {
    match claim.claim_ref.address() {
        ObjectAddress::Occurrence { position, .. } => Some(position),
        _ => None,
    }
}

fn source_order(a: &SourcePosition, b: &SourcePosition) -> Option<std::cmp::Ordering> {
    if a.source != b.source || a.generation != b.generation {
        return None;
    }
    match (&a.locator, &b.locator) {
        (
            SourceLocator::Jsonl { byte_start: a, .. },
            SourceLocator::Jsonl { byte_start: b, .. },
        ) => Some(a.cmp(b)),
        (SourceLocator::Frame { ordinal: a }, SourceLocator::Frame { ordinal: b }) => {
            Some(a.cmp(b))
        }
        (a, b) if a == b => Some(std::cmp::Ordering::Equal),
        _ => None,
    }
}

fn resolve_messages(active: &[&Claim], result: &mut Reduction) {
    let mut revisions: BTreeMap<MessageRef, Vec<(&MessageRevision, &Claim)>> = BTreeMap::new();
    for claim in active {
        if let Fact::Declare {
            object: TraceObject::MessageRevision(revision),
        } = &claim.fact
        {
            revisions
                .entry(revision.message.clone())
                .or_default()
                .push((revision, *claim));
        }
    }
    for (message, values) in revisions {
        let conflicted_revisions: Vec<_> = values
            .iter()
            .filter(|(revision, _)| {
                result
                    .objects
                    .get(&AnyObjectRef::MessageRevision(
                        revision.revision_ref.clone(),
                    ))
                    .is_some_and(|object| matches!(object.declaration, Resolved::Conflicted { .. }))
            })
            .map(|(revision, _)| revision.revision_ref.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let replaced: BTreeSet<_> = values
            .iter()
            .filter_map(|(revision, _)| match &revision.form {
                RevisionForm::Correction { replaces } => Some(replaces.clone()),
                _ => None,
            })
            .collect();
        let finals: Vec<_> = values
            .iter()
            .filter(|(revision, _)| {
                !replaced.contains(&revision.revision_ref)
                    && matches!(
                        revision.form,
                        RevisionForm::Final { .. }
                            | RevisionForm::Correction { .. }
                            | RevisionForm::Complete
                    )
            })
            .collect();
        let mut late_segments = Vec::new();
        for (revision, claim) in &values {
            if !matches!(revision.form, RevisionForm::Append { .. }) {
                continue;
            }
            let later_than_seal = finals.iter().any(|(_, sealed)| {
                claim_position(claim)
                    .zip(claim_position(sealed))
                    .is_some_and(|(a, b)| source_order(a, b) == Some(std::cmp::Ordering::Greater))
            });
            if later_than_seal {
                late_segments.push(revision.revision_ref.clone());
                diagnostic(
                    result,
                    DiagnosticCode::LateSegment,
                    vec![claim.claim_ref.clone()],
                    "Late ordinary segment is retained without reopening sealed content",
                );
            }
        }
        let candidates: Vec<_> = values
            .iter()
            .map(|(revision, claim)| RevisionCandidate {
                reference: revision.revision_ref.clone(),
                form: revision.form.clone(),
                position: claim_position(claim).cloned(),
                claim: claim.claim_ref.clone(),
            })
            .collect();
        let selected = if conflicted_revisions.is_empty() {
            select_revision(&candidates.iter().collect::<Vec<_>>())
        } else {
            Resolved::Unknown
        };
        result.messages.insert(
            message,
            MessageHistory {
                revisions: values
                    .iter()
                    .map(|(revision, _)| revision.revision_ref.clone())
                    .collect(),
                selected,
                sealed: !finals.is_empty(),
                late_segments,
                conflicted_revisions,
                candidates,
            },
        );
    }
}

/// Cumulative usage can be differenced only within the same evidenced counter domain.
/// A missing or reset baseline remains unavailable; no zero baseline is fabricated.
pub fn cumulative_delta(
    previous: &Measurement,
    current: &Measurement,
) -> Option<serde_json::Number> {
    let (
        MeasurementMode::Cumulative {
            counter_domain: a,
            reset_epoch: ar,
            sample_position: ap,
        },
        MeasurementMode::Cumulative {
            counter_domain: b,
            reset_epoch: br,
            sample_position: bp,
        },
    ) = (&previous.mode, &current.mode)
    else {
        return None;
    };
    if a.is_none()
        || ar.is_none()
        || a != b
        || ar != br
        || previous.subject != current.subject
        || previous.metric != current.metric
        || previous.unit != current.unit
        || previous.aggregation_scope != current.aggregation_scope
        || source_order(ap, bp) != Some(std::cmp::Ordering::Less)
    {
        return None;
    }
    if let (Some(a), Some(b)) = (previous.value.as_u64(), current.value.as_u64()) {
        return b.checked_sub(a).map(Into::into);
    }
    let delta = current.value.as_f64()? - previous.value.as_f64()?;
    if delta < 0.0 {
        None
    } else {
        serde_json::Number::from_f64(delta)
    }
}

fn resolve_execution(active: &[&Claim], result: &mut Reduction) {
    let mut outcomes: BTreeMap<AnyObjectRef, Vec<(TerminalOutcome, ClaimRef)>> = BTreeMap::new();
    for claim in active {
        if let Fact::Execution {
            execution,
            boundary,
        } = &claim.fact
        {
            let reference = execution.object_ref();
            let state = result
                .executions
                .entry(reference.clone())
                .or_insert_with(|| ExecutionState {
                    started: vec![],
                    terminated: vec![],
                    outcome: Resolved::Unknown,
                    interrupt_requests: vec![],
                    boundaries: vec![],
                });
            state
                .boundaries
                .push((boundary.clone(), claim.claim_ref.clone()));
            match boundary {
                ExecutionBoundary::Started { .. } => state.started.push(claim.claim_ref.clone()),
                ExecutionBoundary::Terminated { .. } => {
                    state.terminated.push(claim.claim_ref.clone())
                }
                ExecutionBoundary::Outcome { outcome, .. } => {
                    state.terminated.push(claim.claim_ref.clone());
                    outcomes
                        .entry(reference)
                        .or_default()
                        .push((*outcome, claim.claim_ref.clone()));
                }
                ExecutionBoundary::InterruptRequested { .. } => {
                    state.interrupt_requests.push(claim.claim_ref.clone())
                }
            }
        }
    }
    for (reference, state) in &mut result.executions {
        state.outcome = resolve_values(outcomes.remove(reference).unwrap_or_default().into_iter());
        if let Resolved::Conflicted { alternatives } = &state.outcome {
            result.diagnostics.push(ReductionDiagnostic {
                code: DiagnosticCode::ConflictingOutcome,
                claims: alternatives
                    .iter()
                    .flat_map(|(_, claims)| claims.clone())
                    .collect(),
                message: "Independent terminal outcomes disagree".into(),
            });
        }
    }
}

/// Strongly connected components identify all cyclic edges without depending on
/// insertion order. A greedy edge insertion would preserve a different parent on replay.
fn cyclic_edge_claims<T: Clone + Ord>(edges: &[(T, T, ClaimRef)]) -> BTreeSet<ClaimRef> {
    let mut graph = DiGraph::<T, ()>::new();
    let mut indices = BTreeMap::new();
    for (from, to, _) in edges {
        for value in [from, to] {
            indices
                .entry(value.clone())
                .or_insert_with(|| graph.add_node(value.clone()));
        }
        graph.add_edge(indices[from], indices[to], ());
    }
    let mut components = BTreeMap::new();
    for (index, component) in kosaraju_scc(&graph).into_iter().enumerate() {
        for node in component {
            components.insert(graph[node].clone(), index);
        }
    }
    edges
        .iter()
        .filter(|(a, b, _)| components[a] == components[b])
        .map(|(_, _, claim)| claim.clone())
        .collect()
}

fn resolve_relations(active: &[&Claim], result: &mut Reduction) {
    let mut spawn_edges = Vec::new();
    let mut history_edges = Vec::new();
    let mut nesting_edges = Vec::new();
    let mut parents: BTreeMap<AgentRef, BTreeMap<AgentRef, Vec<ClaimRef>>> = BTreeMap::new();
    let mut relation_claims = Vec::new();
    for claim in active {
        if let Fact::Relation { relation } = &claim.fact {
            match relation {
                Relation::Spawn { parent, child, .. } => {
                    spawn_edges.push((parent.clone(), child.clone(), claim.claim_ref.clone()));
                    parents
                        .entry(child.clone())
                        .or_default()
                        .entry(parent.clone())
                        .or_default()
                        .push(claim.claim_ref.clone());
                }
                Relation::HistoryPredecessor { predecessor, entry }
                | Relation::ReadingOrder {
                    before: predecessor,
                    after: entry,
                } => history_edges.push((
                    predecessor.clone(),
                    entry.clone(),
                    claim.claim_ref.clone(),
                )),
                Relation::Nesting { enclosing, nested } => {
                    nesting_edges.push((enclosing.clone(), nested.clone(), claim.claim_ref.clone()))
                }
                _ => {}
            }
            relation_claims.push((*claim, relation));
        }
        if let Fact::Declare {
            object: TraceObject::HistoryEntry(entry),
        } = &claim.fact
        {
            if let Some(predecessor) = &entry.predecessor {
                history_edges.push((
                    predecessor.clone(),
                    entry.entry_ref.clone(),
                    claim.claim_ref.clone(),
                ));
            }
        }
    }
    let spawn_cycles = cyclic_edge_claims(&spawn_edges);
    let history_cycles = cyclic_edge_claims(&history_edges);
    let nesting_cycles = cyclic_edge_claims(&nesting_edges);
    let mut invalid = BTreeSet::new();
    for (code, set, message) in [
        (
            DiagnosticCode::CyclicSpawn,
            spawn_cycles,
            "Spawn graph contains a cycle",
        ),
        (
            DiagnosticCode::CyclicHistory,
            history_cycles,
            "History order contains a cycle",
        ),
        (
            DiagnosticCode::InvalidRelation,
            nesting_cycles,
            "Operation nesting contains a cycle",
        ),
    ] {
        if !set.is_empty() {
            diagnostic(result, code, set.iter().cloned().collect(), message);
            invalid.extend(set);
        }
    }
    for candidates in parents.values().filter(|parents| parents.len() > 1) {
        let claims: Vec<_> = candidates.values().flatten().cloned().collect();
        diagnostic(
            result,
            DiagnosticCode::ConflictingOwner,
            claims.clone(),
            "Agent has more than one proven spawn parent",
        );
        invalid.extend(claims);
    }
    for (claim, relation) in relation_claims {
        if invalid.contains(&claim.claim_ref) {
            continue;
        }
        if let Relation::Nesting { enclosing, nested } = relation {
            let a = result
                .objects
                .get(&AnyObjectRef::Operation(enclosing.clone()))
                .and_then(|v| v.declaration.known())
                .and_then(object_owner);
            let b = result
                .objects
                .get(&AnyObjectRef::Operation(nested.clone()))
                .and_then(|v| v.declaration.known())
                .and_then(object_owner);
            if a.zip(b).is_some_and(|(a, b)| a != b) {
                diagnostic(
                    result,
                    DiagnosticCode::ConflictingOwner,
                    vec![claim.claim_ref.clone()],
                    "Nesting cannot transfer operation ownership",
                );
                continue;
            }
        }
        result.relations.push(ResolvedRelation {
            claim: claim.claim_ref.clone(),
            relation: relation.clone(),
        });
    }
}

mod history;
use history::resolve_histories;

mod changes;
pub use changes::{ChangedValue, MapChanges, ReductionChanges, SetChanges};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod content_tests;

#[cfg(test)]
#[path = "reducer/history_tests.rs"]
mod history_tests;
