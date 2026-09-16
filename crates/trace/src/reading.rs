//! Reader projections depend only on normalized facts and immutable payload metadata.
//!
//! This module never sees vendor JSON. Presentation grouping cannot create execution,
//! change authorship or promote an operation into an agent.

use crate::reducer::{ReadingOrderBasis, Reduction, Resolved};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use sylph_protocol::trace::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PayloadPreview {
    pub preview: String,
    pub preview_truncated: bool,
    pub total_bytes: Option<u64>,
    pub ranges: Vec<PayloadRange>,
    pub availability: Option<ReadAvailability>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReadingScope {
    pub agent: AgentRef,
    pub selection: Option<SelectionRef>,
}

#[derive(Debug, Clone, Default)]
pub struct ReadingProjection {
    pub headers: BTreeMap<AgentRef, AgentHeader>,
    pub windows: BTreeMap<ReadingScope, Vec<ReadingUnit>>,
    pub directories: BTreeMap<AgentRef, Vec<AgentDirectoryRow>>,
    pub histories: BTreeMap<AgentRef, Vec<HistoryChoice>>,
    pub operations: BTreeMap<AnyObjectRef, Vec<AnyObjectRef>>,
    pub objects: BTreeMap<AnyObjectRef, ReadObjectDetail>,
    pub object_relations: BTreeMap<AnyObjectRef, Vec<ReadRelation>>,
    pub parts: BTreeMap<String, Vec<PartSummary>>,
    pub selected_objects: BTreeMap<ReadingScope, BTreeSet<AnyObjectRef>>,
}

struct Index<'a> {
    reduction: &'a Reduction,
    payloads: &'a BTreeMap<PayloadRef, PayloadPreview>,
    max_parts: usize,
    agents: BTreeMap<AgentRef, AgentThread>,
    activation_agents: BTreeMap<AgentRef, Vec<ActivationRef>>,
    operations: BTreeMap<ActivationRef, Vec<OperationRef>>,
    deliveries: BTreeMap<EntryRef, Vec<DeliveryRef>>,
    results: BTreeMap<OperationRef, Vec<ResultRef>>,
    resources: BTreeMap<OperationRef, BTreeSet<ResourceRef>>,
    children: BTreeMap<AgentRef, BTreeSet<AgentRef>>,
    relations: BTreeMap<AnyObjectRef, Vec<ReadRelation>>,
    nested: BTreeMap<OperationRef, Vec<OperationRef>>,
    coverage_by_agent: BTreeMap<AgentRef, Vec<&'a Coverage>>,
    coverage_by_subject: BTreeMap<AnyObjectRef, Vec<&'a Coverage>>,
    submitted: BTreeMap<AgentRef, Vec<&'a Delivery>>,
    parents: BTreeMap<AgentRef, AgentRef>,
}

pub fn project(
    reduction: &Reduction,
    payloads: &BTreeMap<PayloadRef, PayloadPreview>,
    max_parts: usize,
) -> ReadingProjection {
    let mut index = Index {
        reduction,
        payloads,
        max_parts: max_parts.max(1),
        agents: BTreeMap::new(),
        activation_agents: BTreeMap::new(),
        operations: BTreeMap::new(),
        deliveries: BTreeMap::new(),
        results: BTreeMap::new(),
        resources: BTreeMap::new(),
        children: BTreeMap::new(),
        relations: BTreeMap::new(),
        nested: BTreeMap::new(),
        coverage_by_agent: BTreeMap::new(),
        coverage_by_subject: BTreeMap::new(),
        submitted: BTreeMap::new(),
        parents: BTreeMap::new(),
    };
    for object in reduction
        .objects
        .values()
        .filter_map(|object| object.declaration.known())
    {
        match object {
            TraceObject::Agent(agent) => {
                index.agents.insert(agent.agent_ref.clone(), agent.clone());
            }
            TraceObject::Activation(activation) => {
                if let OwnerRef::Agent { agent } = &activation.owner {
                    index
                        .activation_agents
                        .entry(agent.clone())
                        .or_default()
                        .push(activation.activation_ref.clone());
                }
            }
            TraceObject::Operation(operation) => {
                if let Some(activation) = &operation.activation {
                    index
                        .operations
                        .entry(activation.clone())
                        .or_default()
                        .push(operation.operation_ref.clone());
                }
            }
            TraceObject::Result(result) => {
                index
                    .results
                    .entry(result.operation.clone())
                    .or_default()
                    .push(result.result_ref.clone());
            }
            TraceObject::Delivery(delivery) => {
                if matches!(delivery.route, DeliveryRoute::ServerCommand { .. }) {
                    if let Recipient::Agent {
                        agent: OwnerRef::Agent { agent },
                    } = &delivery.recipient
                    {
                        index
                            .submitted
                            .entry(agent.clone())
                            .or_default()
                            .push(delivery);
                    }
                }
            }
            TraceObject::Resource(resource) => {
                if let Some(operation) = &resource.creator {
                    index
                        .resources
                        .entry(operation.clone())
                        .or_default()
                        .insert(resource.resource_ref.clone());
                }
            }
            _ => {}
        }
    }
    for (delivery, stages) in &reduction.delivery_stages {
        for (stage, _) in stages {
            if let DeliveryStage::Admitted { entry: Some(entry) } = stage {
                index
                    .deliveries
                    .entry(entry.clone())
                    .or_default()
                    .push(delivery.clone());
            }
        }
    }
    for (coverage, _) in &reduction.coverage {
        index
            .coverage_by_agent
            .entry(coverage.subject.scope().clone())
            .or_default()
            .push(coverage);
        index
            .coverage_by_subject
            .entry(coverage.subject.clone())
            .or_default()
            .push(coverage);
    }
    // Build adjacency once. Detail materialization must not rescan the whole graph
    // for every object, and repeated evidence must not inflate relation counts.
    let mut relation_keys = BTreeMap::<AnyObjectRef, BTreeSet<(String, AnyObjectRef)>>::new();
    for relation in &reduction.relations {
        let subjects = relation.relation.subject_refs();
        for subject in &subjects {
            for target in subjects.iter().filter(|target| *target != subject) {
                relation_keys
                    .entry(subject.clone())
                    .or_default()
                    .insert((relation_label(&relation.relation).into(), target.clone()));
            }
        }
        match &relation.relation {
            Relation::Nesting { enclosing, nested } => {
                index
                    .nested
                    .entry(enclosing.clone())
                    .or_default()
                    .push(nested.clone());
            }
            Relation::Spawn { parent, child, .. } => {
                if index.agents.contains_key(child) {
                    index
                        .children
                        .entry(parent.clone())
                        .or_default()
                        .insert(child.clone());
                    index.parents.insert(child.clone(), parent.clone());
                }
            }
            Relation::UsesResource {
                operation,
                resource,
            } => {
                index
                    .resources
                    .entry(operation.clone())
                    .or_default()
                    .insert(resource.clone());
            }
            _ => {}
        }
    }
    for (subject, targets) in relation_keys {
        index.relations.insert(
            subject,
            targets
                .into_iter()
                .map(|(label, target)| ReadRelation {
                    agent: match &target {
                        AnyObjectRef::Agent(agent) => Some(index.agent_link(agent)),
                        _ => None,
                    },
                    target: TraceAddress::of(&target),
                    label,
                })
                .collect(),
        );
    }
    let mut result = ReadingProjection::default();
    for (agent, declaration) in &index.agents {
        result
            .headers
            .insert(agent.clone(), index.header(declaration));
        let directory = index
            .children
            .get(agent)
            .into_iter()
            .flatten()
            .map(|child| AgentDirectoryRow {
                agent: index.agent_link(child),
                relation: "spawn".into(),
                own_execution: index.agent_execution(child),
                activations: count(
                    index.activation_agents.get(child).map_or(0, Vec::len),
                    "own activations",
                ),
                required_input: unknown_count("required input"),
                coverage: index.coverage(child),
            })
            .collect();
        result.directories.insert(agent.clone(), directory);
        let scope = ReadingScope {
            agent: agent.clone(),
            selection: None,
        };
        if !reduction.histories.contains_key(agent) {
            result
                .windows
                .insert(scope.clone(), index.window(&scope, &[]));
        }
    }
    for (agent, history) in &reduction.histories {
        let archive = ReadingScope {
            agent: agent.clone(),
            selection: None,
        };
        result.windows.insert(
            archive.clone(),
            index.window(&archive, &history.archive_order),
        );
        if let Some((selection, entries)) = history
            .selection
            .known()
            .zip(history.selected_entries.as_ref())
        {
            let scope = ReadingScope {
                agent: agent.clone(),
                selection: Some(selection.clone()),
            };
            result
                .windows
                .insert(scope.clone(), index.window(&scope, entries));
        }
    }
    for object in reduction
        .objects
        .values()
        .filter_map(|object| object.declaration.known())
    {
        if let TraceObject::HistorySelection(selection) = object {
            let selected = reduction
                .histories
                .get(&selection.agent)
                .and_then(|history| history.selection.known())
                == Some(&selection.selection_ref);
            let entries = reduction
                .selection_memberships
                .get(&selection.selection_ref)
                .and_then(Option::as_ref);
            if let Some(entries) = entries {
                let scope = ReadingScope {
                    agent: selection.agent.clone(),
                    selection: Some(selection.selection_ref.clone()),
                };
                result
                    .windows
                    .entry(scope.clone())
                    .or_insert_with(|| index.window(&scope, entries));
                result
                    .selected_objects
                    .insert(scope.clone(), index.selected_objects(&scope, entries));
            }
            result
                .histories
                .entry(selection.agent.clone())
                .or_default()
                .push(HistoryChoice {
                    selection: selection.selection_ref.clone(),
                    label: "Recorded history selection".into(),
                    is_current: selected,
                    availability: if entries.is_some() {
                        ReadAvailability::Available
                    } else {
                        ReadAvailability::Partial {
                            reason: "Membership has not been resolved for this selection".into(),
                        }
                    },
                    entry_count: entries.map_or_else(
                        || unknown_count("selection members"),
                        |entries| count(entries.len(), "selection members"),
                    ),
                });
        }
    }
    for (activation, operations) in &index.operations {
        result.operations.insert(
            AnyObjectRef::Activation(activation.clone()),
            operations
                .iter()
                .cloned()
                .map(AnyObjectRef::Operation)
                .collect(),
        );
    }
    for resolved in reduction.objects.values() {
        let Some(object) = resolved.declaration.known() else {
            continue;
        };
        let detail = index.object_detail(object);
        result
            .object_relations
            .insert(object.object_ref(), detail.relations.clone());
        result.objects.insert(object.object_ref(), detail);
    }
    for (scope, units) in &result.windows {
        for unit in units {
            if let ReadingContent::Message { revision_ref, .. } = &unit.content {
                if let Some(TraceObject::MessageRevision(revision)) =
                    index.object(&AnyObjectRef::MessageRevision(revision_ref.clone()))
                {
                    let key = parts_key(
                        &scope.agent,
                        scope.selection.as_ref(),
                        &unit.anchor.object(),
                        revision_ref,
                    );
                    result.parts.insert(
                        key,
                        revision
                            .parts
                            .iter()
                            .map(|part| index.part(scope, &unit.anchor.object(), revision, part))
                            .collect(),
                    );
                }
            }
        }
    }
    result
}

pub fn parts_key(
    agent: &AgentRef,
    selection: Option<&SelectionRef>,
    anchor: &AnyObjectRef,
    revision: &MessageRevisionRef,
) -> String {
    serde_json::to_string(&("parts", agent, selection, anchor, revision))
        .expect("typed parts scope")
}

fn count(value: usize, scope: &str) -> ReadCount {
    ReadCount {
        value: Some(value as u64),
        scope: scope.into(),
        complete: true,
    }
}
fn unknown_count(scope: &str) -> ReadCount {
    ReadCount {
        value: None,
        scope: scope.into(),
        complete: false,
    }
}

impl Index<'_> {
    fn selected_objects(
        &self,
        scope: &ReadingScope,
        entries: &[EntryRef],
    ) -> BTreeSet<AnyObjectRef> {
        let mut pending: Vec<_> = entries
            .iter()
            .cloned()
            .map(AnyObjectRef::HistoryEntry)
            .collect();
        pending.push(AnyObjectRef::Agent(scope.agent.clone()));
        if let Some(selection) = &scope.selection {
            pending.push(AnyObjectRef::HistorySelection(selection.clone()));
        }
        let mut included = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            if !included.insert(reference.clone()) {
                continue;
            }
            match self.object(&reference) {
                Some(TraceObject::HistoryEntry(entry)) => {
                    for content in &entry.content {
                        match content {
                            HistoryContent::Message(revision) => {
                                pending.push(AnyObjectRef::MessageRevision(revision.clone()))
                            }
                            HistoryContent::Operation(operation) => {
                                pending.push(AnyObjectRef::Operation(operation.clone()))
                            }
                            HistoryContent::Checkpoint(checkpoint) => {
                                pending.push(AnyObjectRef::ContextCheckpoint(checkpoint.clone()))
                            }
                            HistoryContent::Opaque(_) => {}
                        }
                    }
                    for delivery in self.deliveries.get(&entry.entry_ref).into_iter().flatten() {
                        pending.push(AnyObjectRef::Delivery(delivery.clone()));
                    }
                }
                Some(TraceObject::MessageRevision(revision)) => {
                    pending.push(AnyObjectRef::Message(revision.message.clone()))
                }
                Some(TraceObject::Message(message)) => {
                    if let Some(production) = &message.production {
                        if matches!(&production.owner,OwnerRef::Agent {agent} if agent==&scope.agent)
                        {
                            if let Some(activation) = &production.activation {
                                pending.push(AnyObjectRef::Activation(activation.clone()));
                            }
                        }
                    }
                }
                Some(TraceObject::Operation(operation)) => {
                    if let Some(activation) = &operation.activation {
                        pending.push(AnyObjectRef::Activation(activation.clone()));
                    }
                    pending.extend(
                        self.results
                            .get(&operation.operation_ref)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(AnyObjectRef::Result),
                    );
                    pending.extend(
                        self.resources
                            .get(&operation.operation_ref)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(AnyObjectRef::Resource),
                    );
                    pending.extend(
                        self.nested
                            .get(&operation.operation_ref)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(AnyObjectRef::Operation),
                    );
                }
                _ => {}
            }
        }
        included
    }

    fn object_detail(&self, object: &TraceObject) -> ReadObjectDetail {
        let reference = object.object_ref();
        let owning_agent = match object {
            TraceObject::Operation(operation) => match &operation.owner {
                OwnerRef::Agent { agent } => Some(agent),
                _ => None,
            },
            TraceObject::Activation(activation) => match &activation.owner {
                OwnerRef::Agent { agent } => Some(agent),
                _ => None,
            },
            TraceObject::Resource(resource) => match &resource.host {
                Some(OwnerRef::Agent { agent }) => Some(agent),
                _ => None,
            },
            _ => Some(reference.scope()),
        };
        let scope = ReadingScope {
            agent: owning_agent.unwrap_or(reference.scope()).clone(),
            selection: None,
        };
        let mut relations = self.relations.get(&reference).cloned().unwrap_or_default();
        let content = match object {
            TraceObject::Operation(operation) => {
                let results = self
                    .results
                    .get(&operation.operation_ref)
                    .into_iter()
                    .flatten()
                    .filter_map(|reference| {
                        match self.object(&AnyObjectRef::Result(reference.clone())) {
                            Some(TraceObject::Result(result)) => Some(result),
                            _ => None,
                        }
                    })
                    .map(|result| ReadResultOccurrence {
                        reference: TraceAddress::of(&AnyObjectRef::Result(
                            result.result_ref.clone(),
                        )),
                        disposition: result.disposition,
                        representation: result.representation,
                        label: match result.disposition {
                            ResultDisposition::FinalReturn => "Returned",
                            ResultDisposition::Yield => "Yielded",
                            ResultDisposition::Progress => "Progress",
                            ResultDisposition::Refusal => "Refused",
                            ResultDisposition::Unknown => "Result",
                        }
                        .into(),
                        payloads: result
                            .payload
                            .as_ref()
                            .map(|payload| {
                                self.access(
                                    &scope,
                                    &reference,
                                    &result.result_ref.encode(),
                                    payload,
                                    result.representation,
                                    None,
                                    PayloadEncoding::Binary,
                                )
                            })
                            .into_iter()
                            .collect(),
                        availability: if result.payload.is_some() {
                            ReadAvailability::Available
                        } else {
                            ReadAvailability::Unavailable {
                                reason: "Result payload unavailable".into(),
                            }
                        },
                    })
                    .collect();
                let resources = self
                    .resources
                    .get(&operation.operation_ref)
                    .into_iter()
                    .flatten()
                    .map(|resource| ReadRelation {
                        target: TraceAddress::of(&AnyObjectRef::Resource(resource.clone())),
                        label: "Independent resource".into(),
                        agent: None,
                    })
                    .collect();
                ReadObjectContent::Operation {
                    category: operation.category.clone(),
                    input: operation
                        .input_revision
                        .as_ref()
                        .map(|payload| {
                            self.access(
                                &scope,
                                &reference,
                                &payload.to_string(),
                                payload,
                                Representation::Runtime,
                                Some("application/json".into()),
                                PayloadEncoding::Utf8,
                            )
                        })
                        .into_iter()
                        .collect(),
                    results,
                    results_cursor: None,
                    resources,
                    resources_cursor: None,
                }
            }
            TraceObject::Resource(resource) => ReadObjectContent::Resource {
                resource_kind: resource.kind.clone(),
                state: ReadAvailability::Available,
                payloads: vec![],
                operations_cursor: None,
            },
            TraceObject::ContextCheckpoint(checkpoint) => ReadObjectContent::Context {
                basis: checkpoint.basis,
                coverage: checkpoint.coverage.clone(),
                memberships: relations
                    .iter()
                    .filter(|relation| relation.label == "Context membership")
                    .cloned()
                    .collect(),
                memberships_cursor: None,
                payloads: vec![],
            },
            TraceObject::Measurement(measurement) => ReadObjectContent::Measurement {
                metric: measurement.metric.clone(),
                unit: measurement.unit.clone(),
                value: measurement.value.clone(),
                scope: format!("{:?}", measurement.aggregation_scope),
                mode: measurement.mode.clone(),
            },
            TraceObject::Activation(activation) => {
                for operation in self
                    .operations
                    .get(&activation.activation_ref)
                    .into_iter()
                    .flatten()
                {
                    relations.push(ReadRelation {
                        target: TraceAddress::of(&AnyObjectRef::Operation(operation.clone())),
                        label: "Operation".into(),
                        agent: None,
                    });
                }
                ReadObjectContent::Generic {
                    label: "Activation".into(),
                    payloads: vec![],
                }
            }
            _ => ReadObjectContent::Generic {
                label: object_label(object).into(),
                payloads: vec![],
            },
        };
        ReadObjectDetail {
            view: ReadView {
                token: String::new(),
                generation: String::new(),
                evidence_cut: 0,
                view_revision: 0,
                expires_at_ms: 0,
            },
            reference: TraceAddress::of(&reference),
            title: object_label(object).into(),
            owner: owning_agent.map(|agent| self.agent_link(agent)),
            execution: match object {
                TraceObject::Activation(_)
                | TraceObject::Operation(_)
                | TraceObject::Resource(_) => Some(self.execution(&reference)),
                _ => None,
            },
            availability: ReadAvailability::Available,
            content,
            relations,
            relations_cursor: None,
            coverage: self.coverage(reference.scope()),
        }
    }
    fn object(&self, reference: &AnyObjectRef) -> Option<&TraceObject> {
        self.reduction.object(reference)?.declaration.known()
    }

    fn agent_link(&self, agent: &AgentRef) -> AgentLink {
        let native = match agent {
            AgentRef::Session(session) => session.native_id.as_str(),
            AgentRef::Embedded {
                native_child_id, ..
            } => native_child_id,
        };
        let suffix = native
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        AgentLink {
            address: AgentAddress::of(agent),
            name: self.agents.get(agent).and_then(|agent| agent.name.clone()),
            identity_suffix: suffix,
            vendor: agent.anchor_session().agent.as_str().into(),
        }
    }

    fn execution(&self, reference: &AnyObjectRef) -> ExecutionSummary {
        let Some(execution) = self.reduction.execution(reference) else {
            return ExecutionSummary {
                reference: Some(TraceAddress::of(reference)),
                state: ReadExecutionState::Unavailable,
                durations: vec![],
            };
        };
        let mut durations = Vec::new();
        let mut started = None;
        let mut stopped = None;
        let mut last = None;
        for (boundary, _) in &execution.boundaries {
            match boundary {
                ExecutionBoundary::Started { time } => {
                    if let Some(time) = time {
                        started = Some(time);
                        last = Some(time);
                    }
                }
                ExecutionBoundary::Terminated { time, .. } => {
                    if let Some(time) = time {
                        stopped = Some(time);
                        last = Some(time);
                    }
                }
                _ => {}
            }
        }
        if let Some((start, end)) = started.zip(stopped) {
            if let Ok(value) = start.elapsed_until(end) {
                durations.push(ReadDuration {
                    value: value.into(),
                    unit: start.unit,
                    basis: match start.basis {
                        TimeBasis::SourceMonotonic => ReadDurationBasis::SourceMonotonic,
                        TimeBasis::SourceWall => ReadDurationBasis::SourceWall,
                        _ => ReadDurationBasis::ServerObserved,
                    },
                    clock_domain: Some(start.domain.clone()),
                });
            }
        }
        let reason = execution
            .boundaries
            .iter()
            .find_map(|(boundary, _)| match boundary {
                ExecutionBoundary::Outcome {
                    reason: Some(reason),
                    ..
                }
                | ExecutionBoundary::Terminated {
                    reason: Some(reason),
                    ..
                } => Some(close_reason(reason)),
                _ => None,
            });
        let state = match &execution.outcome {
            Resolved::Known { value, .. } => ReadExecutionState::Terminated {
                outcome: *value,
                reason,
            },
            Resolved::Conflicted { .. } => ReadExecutionState::Conflicted {
                reason: "Independent terminal outcomes disagree".into(),
            },
            Resolved::Unknown if !execution.terminated.is_empty() => {
                ReadExecutionState::Terminated {
                    outcome: TerminalOutcome::Unknown,
                    reason,
                }
            }
            Resolved::Unknown if execution.started.is_empty() => ReadExecutionState::Unavailable,
            Resolved::Unknown => {
                let following = self
                    .coverage_by_subject
                    .get(reference)
                    .into_iter()
                    .flatten()
                    .any(|coverage| {
                        coverage.subject == *reference
                            && coverage.dimension == CoverageDimension::ExecutionBoundaries
                            && coverage.state == CoverageState::Following
                    });
                let last_observed_at_ms = last
                    .filter(|time| {
                        time.unit == TimeUnit::Milliseconds && time.basis == TimeBasis::SourceWall
                    })
                    .map(|time| time.value);
                if following {
                    ReadExecutionState::ObservedActive {
                        last_observed_at_ms,
                    }
                } else {
                    ReadExecutionState::ObservationEnded {
                        last_observed_at_ms,
                    }
                }
            }
        };
        ExecutionSummary {
            reference: Some(TraceAddress::of(reference)),
            state,
            durations,
        }
    }

    fn agent_execution(&self, agent: &AgentRef) -> ExecutionSummary {
        let references = self.activation_agents.get(agent);
        let known = references
            .into_iter()
            .flatten()
            .map(|reference| self.execution(&AnyObjectRef::Activation(reference.clone())))
            .collect::<Vec<_>>();
        if known.len() == 1 {
            return known[0].clone();
        }
        let active: Vec<_> = known
            .iter()
            .filter(|summary| matches!(summary.state, ReadExecutionState::ObservedActive { .. }))
            .collect();
        if active.len() == 1 {
            return active[0].clone();
        }
        if let Some(history) = self.reduction.histories.get(agent).filter(|history| {
            history.selected_entries.is_some() || history.candidate_heads.len() == 1
        }) {
            for entry_ref in history
                .selected_entries
                .as_ref()
                .unwrap_or(&history.archive_order)
                .iter()
                .rev()
            {
                if history.order_basis.get(entry_ref)
                    == Some(&ReadingOrderBasis::PresentationTiebreak)
                {
                    continue;
                }
                let Some(TraceObject::HistoryEntry(entry)) =
                    self.object(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
                else {
                    continue;
                };
                for content in entry.content.iter().rev() {
                    let activation=match content {
                        HistoryContent::Message(revision)=>match self.object(&AnyObjectRef::MessageRevision(revision.clone())) {Some(TraceObject::MessageRevision(revision))=>match self.object(&AnyObjectRef::Message(revision.message.clone())) {Some(TraceObject::Message(message))=>message.production.as_ref().filter(|scope|matches!(&scope.owner,OwnerRef::Agent {agent:owner} if owner==agent)).and_then(|scope|scope.activation.as_ref()),_=>None},_=>None},
                        HistoryContent::Operation(operation)=>match self.object(&AnyObjectRef::Operation(operation.clone())) {Some(TraceObject::Operation(operation)) if matches!(&operation.owner,OwnerRef::Agent {agent:owner} if owner==agent)=>operation.activation.as_ref(),_=>None},
                        _=>None,
                    };
                    if let Some(activation) = activation {
                        return self.execution(&AnyObjectRef::Activation(activation.clone()));
                    }
                }
            }
        }
        // Native IDs and receipt order do not establish which independent activation
        // is the vendor's current one. The activation catalogue remains inspectable.
        ExecutionSummary {
            reference: None,
            state: ReadExecutionState::Unavailable,
            durations: vec![],
        }
    }

    fn coverage(&self, agent: &AgentRef) -> Vec<ReadCoverage> {
        let mut dimensions = BTreeMap::new();
        for coverage in self.coverage_by_agent.get(agent).into_iter().flatten() {
            let availability = match &coverage.state {
                CoverageState::Complete | CoverageState::Following => ReadAvailability::Available,
                CoverageState::Pending { reason } => ReadAvailability::Pending {
                    reason: reason.clone(),
                },
                CoverageState::Gap { reason } => ReadAvailability::Partial {
                    reason: reason.clone(),
                },
                CoverageState::Unavailable { reason } => ReadAvailability::Unavailable {
                    reason: reason.clone(),
                },
                CoverageState::Conflicted { .. } => ReadAvailability::Conflicted {
                    reason: "Coverage claims disagree".into(),
                },
            };
            dimensions
                .entry(coverage.dimension)
                .or_insert_with(Vec::new)
                .push(ReadCoverage {
                    dimension: coverage.dimension,
                    state: availability,
                    evidence: Some(TraceAddress::of(&coverage.subject)),
                });
        }
        dimensions.into_iter().map(|(dimension,mut values)|{
            let first=values.remove(0);
            if values.iter().all(|value|value.state==first.state) {first}
            else {ReadCoverage {dimension,state:ReadAvailability::Partial {reason:"Sources report different coverage for this dimension; inspect their intervals".into()},evidence:first.evidence}}
        }).collect()
    }

    fn header(&self, agent: &AgentThread) -> AgentHeader {
        let history = self.reduction.histories.get(&agent.agent_ref);
        let selection = history
            .and_then(|history| history.selection.known())
            .cloned();
        let known = selection.is_some();
        let complete = history.is_some_and(|history| history.selected_entries.is_some());
        AgentHeader {
            agent: self.agent_link(&agent.agent_ref),
            environment: agent.agent_ref.environment().as_str().into(),
            environment_name: None,
            model: agent.model.clone(),
            own_execution: self.agent_execution(&agent.agent_ref),
            history: HistoryLocation {
                selection,
                label: if known {
                    "Selected history"
                } else {
                    "Retained history · selection unknown"
                }
                .into(),
                is_current: known,
                selection_known: known,
                availability: if complete {
                    ReadAvailability::Available
                } else {
                    ReadAvailability::Partial {
                        reason: "The source has not established complete selected membership"
                            .into(),
                    }
                },
            },
            parent: self
                .parents
                .get(&agent.agent_ref)
                .map(|parent| self.agent_link(parent)),
            origin: None,
            coverage: self.coverage(&agent.agent_ref),
            descendant_count: count(
                self.children.get(&agent.agent_ref).map_or(0, BTreeSet::len),
                "direct children",
            ),
            continuing_resources: unknown_count("independent resources"),
            required_input: unknown_count("required input"),
            file_context: None,
        }
    }

    fn author(&self, author: &Author) -> ReadAuthor {
        match author {
            Author::Human { subject } => ReadAuthor::Human {
                label: subject.clone().unwrap_or_else(|| "Human".into()),
            },
            Author::Agent {
                agent: OwnerRef::Agent { agent },
            } => ReadAuthor::Agent {
                agent: Some(self.agent_link(agent)),
                label: self
                    .agents
                    .get(agent)
                    .and_then(|agent| agent.name.clone())
                    .unwrap_or_else(|| "Agent".into()),
            },
            Author::Agent { .. } => ReadAuthor::Agent {
                agent: None,
                label: "Agent · identity unresolved".into(),
            },
            Author::Harness { identity } => ReadAuthor::Harness {
                label: identity.clone().unwrap_or_else(|| "Harness".into()),
            },
            Author::Tool { operation } => ReadAuthor::Tool {
                operation: operation
                    .as_ref()
                    .map(|operation| TraceAddress::of(&AnyObjectRef::Operation(operation.clone()))),
                label: "Tool".into(),
            },
            Author::External { identity } => ReadAuthor::External {
                label: identity.clone().unwrap_or_else(|| "External".into()),
            },
            Author::Unknown => ReadAuthor::Unknown,
        }
    }

    fn access(
        &self,
        scope: &ReadingScope,
        anchor: &AnyObjectRef,
        revision: &str,
        payload: &PayloadRef,
        representation: Representation,
        media_type: Option<String>,
        encoding: PayloadEncoding,
    ) -> PayloadAccess {
        let preview = self.payloads.get(payload);
        PayloadAccess {
            payload_ref: payload.clone(),
            body_revision: revision.into(),
            representation,
            via_ref: TraceAddress::of(anchor),
            containing_agent: AgentAddress::of(&scope.agent),
            selection: scope.selection.clone(),
            media_type,
            encoding,
            total_bytes: preview.and_then(|preview| preview.total_bytes),
            available_ranges: preview.map_or_else(Vec::new, |preview| preview.ranges.clone()),
            availability: preview
                .and_then(|preview| preview.availability.clone())
                .unwrap_or_else(|| ReadAvailability::Unavailable {
                    reason: "Payload metadata is unavailable".into(),
                }),
        }
    }

    fn part(
        &self,
        scope: &ReadingScope,
        anchor: &AnyObjectRef,
        revision: &MessageRevision,
        part: &ContentPart,
    ) -> PartSummary {
        let preview = self.payloads.get(&part.payload);
        PartSummary {
            part_ref: part.part_ref.clone(),
            kind: part.kind,
            preview: preview.map_or_else(String::new, |preview| preview.preview.clone()),
            preview_truncated: preview.is_some_and(|preview| preview.preview_truncated),
            payload: self.access(
                scope,
                anchor,
                &revision.revision_ref.encode(),
                &part.payload,
                revision.representation,
                part.media_type.clone(),
                part.encoding,
            ),
        }
    }

    fn window(&self, scope: &ReadingScope, entries: &[EntryRef]) -> Vec<ReadingUnit> {
        let mut units = Vec::new();
        let mut seen_deliveries = BTreeSet::new();
        let mut represented_messages = BTreeSet::new();
        let included_revisions: BTreeSet<_> = entries
            .iter()
            .filter_map(|entry| self.object(&AnyObjectRef::HistoryEntry(entry.clone())))
            .filter_map(|object| match object {
                TraceObject::HistoryEntry(entry) => Some(entry),
                _ => None,
            })
            .flat_map(|entry| entry.content.iter())
            .filter_map(|content| match content {
                HistoryContent::Message(revision) => Some(revision.clone()),
                _ => None,
            })
            .collect();
        for entry_ref in entries {
            let Some(TraceObject::HistoryEntry(entry)) =
                self.object(&AnyObjectRef::HistoryEntry(entry_ref.clone()))
            else {
                continue;
            };
            let anchor = AnyObjectRef::HistoryEntry(entry_ref.clone());
            let order_basis = match self
                .reduction
                .histories
                .get(&entry.containing_agent)
                .and_then(|history| history.order_basis.get(entry_ref))
            {
                Some(ReadingOrderBasis::NativePredecessor) => ReadOrderBasis::NativePredecessor,
                Some(ReadingOrderBasis::ExplicitOrder) => ReadOrderBasis::ExplicitOrder,
                _ => ReadOrderBasis::PresentationTiebreak,
            };
            for (slot, content) in entry.content.iter().enumerate() {
                let address = format!("{}:{slot}", TraceAddress::of(&anchor).as_str());
                match content {
                    HistoryContent::Message(revision_ref) => {
                        let Some(TraceObject::MessageRevision(revision)) =
                            self.object(&AnyObjectRef::MessageRevision(revision_ref.clone()))
                        else {
                            units.push(self.unknown(
                                address,
                                anchor.clone(),
                                order_basis,
                                "Message revision unavailable",
                            ));
                            continue;
                        };
                        let revision = if self.deliveries.get(entry_ref).is_none_or(Vec::is_empty) {
                            if !represented_messages.insert(revision.message.clone()) {
                                continue;
                            }
                            self.reduction
                                .message(&revision.message)
                                .and_then(|history| history.selected.known())
                                .filter(|selected| included_revisions.contains(*selected))
                                .and_then(|selected| {
                                    self.object(&AnyObjectRef::MessageRevision(selected.clone()))
                                })
                                .and_then(|object| match object {
                                    TraceObject::MessageRevision(revision) => Some(revision),
                                    _ => None,
                                })
                                .unwrap_or(revision)
                        } else {
                            revision
                        };
                        let Some(TraceObject::Message(message)) =
                            self.object(&AnyObjectRef::Message(revision.message.clone()))
                        else {
                            units.push(self.unknown(
                                address,
                                anchor.clone(),
                                order_basis,
                                "Message author evidence unavailable",
                            ));
                            continue;
                        };
                        let deliveries =
                            self.deliveries.get(entry_ref).cloned().unwrap_or_default();
                        let associated = deliveries
                            .into_iter()
                            .filter_map(|delivery| {
                                match self.object(&AnyObjectRef::Delivery(delivery.clone())) {
                                    Some(TraceObject::Delivery(value))
                                        if value.message_revision == *revision_ref =>
                                    {
                                        Some((delivery, value))
                                    }
                                    _ => None,
                                }
                            })
                            .collect::<Vec<_>>();
                        if associated.is_empty() {
                            units.push(self.message_unit(
                                scope,
                                address,
                                anchor.clone(),
                                order_basis,
                                message,
                                revision,
                                None,
                                Some(entry),
                            ));
                        } else {
                            for (delivery, value) in associated {
                                seen_deliveries.insert(delivery.clone());
                                units.push(self.message_unit(
                                    scope,
                                    format!("{address}:{}", delivery.encode()),
                                    anchor.clone(),
                                    order_basis,
                                    message,
                                    revision,
                                    Some(value),
                                    Some(entry),
                                ));
                            }
                        }
                    }
                    HistoryContent::Operation(operation) => {
                        let Some(TraceObject::Operation(operation)) =
                            self.object(&AnyObjectRef::Operation(operation.clone()))
                        else {
                            units.push(self.unknown(
                                address,
                                anchor.clone(),
                                order_basis,
                                "Invocation details unavailable",
                            ));
                            continue;
                        };
                        let OwnerRef::Agent { agent: owner } = &operation.owner else {
                            units.push(self.unknown(
                                address,
                                anchor.clone(),
                                order_basis,
                                "Operation owner is unresolved",
                            ));
                            continue;
                        };
                        units.push(ReadingUnit {
                            address,
                            anchor: TraceAddress::of(&anchor),
                            order_basis,
                            source_surface: source_label(entry_ref),
                            activation: operation
                                .activation
                                .as_ref()
                                .filter(|_| owner == &scope.agent)
                                .map(|activation| {
                                    self.execution(&AnyObjectRef::Activation(activation.clone()))
                                }),
                            content: ReadingContent::Work {
                                owner: self.agent_link(owner),
                                operations: count(1, "this invocation"),
                                agents: unknown_count("agents created by this invocation"),
                                assignments: unknown_count("task deliveries"),
                                failed_operations: count(
                                    usize::from(matches!(
                                        self.execution(&AnyObjectRef::Operation(
                                            operation.operation_ref.clone()
                                        ))
                                        .state,
                                        ReadExecutionState::Terminated {
                                            outcome: TerminalOutcome::Failed,
                                            ..
                                        }
                                    )),
                                    "own operation outcomes",
                                ),
                                active_resources: unknown_count("independent resource states"),
                                required_input: unknown_count("required input"),
                                operations_anchor: TraceAddress::of(&AnyObjectRef::Operation(
                                    operation.operation_ref.clone(),
                                )),
                            },
                        });
                    }
                    HistoryContent::Checkpoint(checkpoint) => units.push(ReadingUnit {
                        address,
                        anchor: TraceAddress::of(&anchor),
                        order_basis,
                        source_surface: source_label(entry_ref),
                        activation: None,
                        content: ReadingContent::ContextBoundary {
                            checkpoint: TraceAddress::of(&AnyObjectRef::ContextCheckpoint(
                                checkpoint.clone(),
                            )),
                            label: "Context checkpoint".into(),
                            summary: None,
                        },
                    }),
                    HistoryContent::Opaque(payload) => units.push(ReadingUnit {
                        address,
                        anchor: TraceAddress::of(&anchor),
                        order_basis,
                        source_surface: source_label(entry_ref),
                        activation: None,
                        content: ReadingContent::Unknown {
                            label: "Retained content".into(),
                            reason: "The source supplied opaque content".into(),
                            payloads: vec![self.access(
                                scope,
                                &anchor,
                                &payload.to_string(),
                                payload,
                                Representation::Conversation,
                                None,
                                PayloadEncoding::Binary,
                            )],
                        },
                    }),
                }
            }
        }
        // A server-accepted input is visible before vendor admission, with exactly
        // its delivery stage. It never creates a synthetic history entry/activation.
        for delivery in self.submitted.get(&scope.agent).into_iter().flatten() {
            if scope.selection.is_some() || seen_deliveries.contains(&delivery.delivery_ref) {
                continue;
            }
            let Some(TraceObject::MessageRevision(revision)) = self.object(
                &AnyObjectRef::MessageRevision(delivery.message_revision.clone()),
            ) else {
                continue;
            };
            let Some(TraceObject::Message(message)) =
                self.object(&AnyObjectRef::Message(revision.message.clone()))
            else {
                continue;
            };
            let anchor = AnyObjectRef::Delivery(delivery.delivery_ref.clone());
            units.push(self.message_unit(
                scope,
                TraceAddress::of(&anchor).as_str().into(),
                anchor,
                ReadOrderBasis::PresentationTiebreak,
                message,
                revision,
                Some(delivery),
                None,
            ));
        }
        let mut grouped: Vec<ReadingUnit> = Vec::new();
        for unit in units {
            if let (
                Some(previous),
                ReadingContent::Work {
                    owner,
                    operations,
                    failed_operations,
                    ..
                },
            ) = (grouped.last_mut(), &unit.content)
            {
                if previous.activation == unit.activation && unit.activation.is_some() {
                    if let ReadingContent::Work {
                        owner: prior_owner,
                        operations: prior_operations,
                        failed_operations: prior_failed,
                        ..
                    } = &mut previous.content
                    {
                        if prior_owner.address == owner.address {
                            prior_operations.value = prior_operations
                                .value
                                .zip(operations.value)
                                .map(|(a, b)| a + b);
                            prior_failed.value = prior_failed
                                .value
                                .zip(failed_operations.value)
                                .map(|(a, b)| a + b);
                            continue;
                        }
                    }
                }
            }
            let mut unit = unit;
            if let ReadingContent::Work {
                operations_anchor, ..
            } = &mut unit.content
            {
                if let Some(reference) = unit
                    .activation
                    .as_ref()
                    .and_then(|activation| activation.reference.clone())
                {
                    *operations_anchor = reference;
                }
            }
            grouped.push(unit);
        }
        grouped
    }

    #[allow(clippy::too_many_arguments)]
    fn message_unit(
        &self,
        scope: &ReadingScope,
        address: String,
        anchor: AnyObjectRef,
        order_basis: ReadOrderBasis,
        message: &Message,
        revision: &MessageRevision,
        delivery: Option<&Delivery>,
        entry: Option<&HistoryEntry>,
    ) -> ReadingUnit {
        ReadingUnit {
            address,
            anchor: TraceAddress::of(&anchor),
            order_basis,
            source_surface: entry.map_or_else(
                || "server control".into(),
                |entry| source_label(&entry.entry_ref),
            ),
            activation: message
                .production
                .as_ref()
                .filter(|production| matches!(&production.owner,OwnerRef::Agent {agent} if agent==&scope.agent))
                .and_then(|production| production.activation.as_ref())
                .map(|activation| self.execution(&AnyObjectRef::Activation(activation.clone()))),
            content: ReadingContent::Message {
                message_ref: message.message_ref.clone(),
                revision_ref: revision.revision_ref.clone(),
                author: self.author(&message.author),
                purpose: message.purpose,
                delivery: delivery.map(|delivery| ReadDelivery {
                    reference: TraceAddress::of(&AnyObjectRef::Delivery(
                        delivery.delivery_ref.clone(),
                    )),
                    sender: self.author(&delivery.sender),
                    stages: self
                        .reduction
                        .delivery_stages
                        .get(&delivery.delivery_ref)
                        .into_iter()
                        .flatten()
                        .map(|(stage, _)| stage.clone())
                        .collect(),
                    availability: ReadAvailability::Available,
                }),
                parts: revision
                    .parts
                    .iter()
                    .take(self.max_parts)
                    .map(|part| self.part(scope, &anchor, revision, part))
                    .collect(),
                parts_total: revision.parts.len(),
                parts_complete: revision.parts.len() <= self.max_parts,
                parts_cursor: None,
                original_owner: entry
                    .and_then(|entry| entry.original_owner.as_ref())
                    .and_then(|owner| match owner {
                        OwnerRef::Agent { agent } => Some(self.agent_link(agent)),
                        _ => None,
                    }),
            },
        }
    }

    fn unknown(
        &self,
        address: String,
        anchor: AnyObjectRef,
        order_basis: ReadOrderBasis,
        reason: &str,
    ) -> ReadingUnit {
        ReadingUnit {
            address,
            anchor: TraceAddress::of(&anchor),
            order_basis,
            source_surface: "retained".into(),
            activation: None,
            content: ReadingContent::Unknown {
                label: "Unresolved content".into(),
                reason: reason.into(),
                payloads: vec![],
            },
        }
    }
}

fn source_label(entry: &EntryRef) -> String {
    match entry.address() {
        ObjectAddress::Native { namespace, .. } => namespace.clone(),
        ObjectAddress::Occurrence { position, .. } => position.source.instance.clone(),
    }
}

fn close_reason(reason: &ExecutionCloseReason) -> String {
    match reason {
        ExecutionCloseReason::VendorStop => "Vendor stopped execution",
        ExecutionCloseReason::BudgetExhausted => "Budget exhausted",
        ExecutionCloseReason::ServerInterrupt => "Interrupted by request",
        ExecutionCloseReason::ProcessExit { .. } => "Process exited",
        ExecutionCloseReason::ObservationLoss => "Observation ended",
        ExecutionCloseReason::GrantRevoked => "Control grant revoked",
        ExecutionCloseReason::Extension { code, .. } => return code.clone(),
    }
    .into()
}

fn relation_label(relation: &Relation) -> &'static str {
    match relation {
        Relation::Spawn { .. } => "Spawn",
        Relation::Interaction { .. } => "Interaction",
        Relation::Trigger { .. } => "Trigger",
        Relation::Nesting { .. } => "Nested operation",
        Relation::UsesResource { .. } => "Resource",
        Relation::HistoryPredecessor { .. } => "History predecessor",
        Relation::ContextLineage { .. } => "Context origin",
        Relation::ContextMembership { .. } => "Context membership",
        Relation::Representation { .. } => "Representation",
        Relation::ReadingOrder { .. } => "Reading order",
    }
}
fn object_label(object: &TraceObject) -> &'static str {
    match object {
        TraceObject::Agent(_) => "Agent",
        TraceObject::Activation(_) => "Activation",
        TraceObject::Message(_) => "Message",
        TraceObject::MessageRevision(_) => "Message revision",
        TraceObject::Delivery(_) => "Delivery",
        TraceObject::Operation(_) => "Operation",
        TraceObject::Result(_) => "Result occurrence",
        TraceObject::Resource(_) => "Resource",
        TraceObject::HistoryEntry(_) => "History entry",
        TraceObject::HistorySelection(_) => "History selection",
        TraceObject::ContextCheckpoint(_) => "Context checkpoint",
        TraceObject::Measurement(_) => "Measurement",
    }
}

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
