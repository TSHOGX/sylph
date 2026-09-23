//! Native copy witnesses join exact source incarnations without owning copied bytes.
use super::*;
use crate::mapping::RecordedSource;
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::sync::Arc;

pub const COPY_PAGE_WORK: usize = 64;
const CONTINUATION: &str = "pi.copy-continuation";
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntryKey {
    id: String,
    fingerprint: String,
}
impl EntryKey {
    pub(super) fn of(id: &str, value: &Value) -> Self {
        let mut content = value.clone();
        let object = content
            .as_object_mut()
            .expect("an entry with an ID is an object");
        // Pi's branch writer re-chains around removed labels and may move a
        // compaction's kept boundary. These are position, not content, equality.
        object.remove("id");
        object.remove("parentId");
        object.remove("parent_id");
        if object.get("type").and_then(Value::as_str) == Some("compaction") {
            object.remove("firstKeptEntryId");
        }
        content.sort_all_objects();
        let mut hasher = blake3::Hasher::new();
        serde_json::to_writer(&mut hasher, &content)
            .expect("canonical JSON hashes without buffering");
        Self {
            id: id.into(),
            fingerprint: hasher.finalize().to_hex().to_string(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct LocatorKey {
    issuer: Issuer,
    locator: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub base: ClaimRef,
    pub key: EntryKey,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub base: ClaimRef,
    pub key: EntryKey,
    pub locator: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    sources: imbl::OrdMap<ObservationRef, Arc<RecordedSource>>,
    by_locator: imbl::OrdMap<LocatorKey, imbl::OrdSet<ObservationRef>>,
    by_source: imbl::OrdMap<(SourceRef, u64), imbl::OrdSet<ObservationRef>>,
    witnesses: imbl::OrdMap<ClaimRef, Arc<Witness>>,
    by_entry: imbl::OrdMap<(SourceRef, u64, EntryKey), imbl::OrdSet<ClaimRef>>,
    plans: imbl::OrdMap<ClaimRef, Arc<Plan>>,
    plans_by_locator: imbl::OrdMap<LocatorKey, imbl::OrdSet<ClaimRef>>,
    plans_by_entry: imbl::OrdMap<(LocatorKey, EntryKey), imbl::OrdSet<ClaimRef>>,
}
fn position(base: &ClaimRef) -> &SourcePosition {
    let ObjectAddress::Occurrence { position, .. } = base.address() else {
        unreachable!("native witnesses name captured claims")
    };
    position
}
impl Index {
    pub fn retain_source(&mut self, source: RecordedSource) {
        if source.definition.owner.anchor_session().agent.as_str() != "pi"
            || !matches!(
                source.definition.basis,
                SourceOwnerBasis::NativeIdentity { .. }
            )
        {
            return;
        }
        let key = LocatorKey {
            issuer: source.definition.source.issuer.clone(),
            locator: source.definition.locator.clone(),
        };
        self.by_locator
            .entry(key)
            .or_default()
            .insert(source.observation.clone());
        self.by_source
            .entry((
                source.definition.source.clone(),
                source.definition.generation,
            ))
            .or_default()
            .insert(source.observation.clone());
        self.sources
            .insert(source.observation.clone(), Arc::new(source));
    }
    pub fn retain_witness(&mut self, witness: Witness) {
        let p = position(&witness.base);
        self.by_entry
            .entry((p.source.clone(), p.generation, witness.key.clone()))
            .or_default()
            .insert(witness.base.clone());
        self.witnesses
            .insert(witness.base.clone(), Arc::new(witness));
    }
    pub fn retain_plan(&mut self, plan: Plan) {
        let key = LocatorKey {
            issuer: position(&plan.base).source.issuer.clone(),
            locator: plan.locator.clone(),
        };
        self.plans_by_locator
            .entry(key.clone())
            .or_default()
            .insert(plan.base.clone());
        self.plans_by_entry
            .entry((key, plan.key.clone()))
            .or_default()
            .insert(plan.base.clone());
        self.plans.insert(plan.base.clone(), Arc::new(plan));
    }
    pub fn shares_roots(&self, other: &Self) -> bool {
        self.sources.ptr_eq(&other.sources)
            && self.by_locator.ptr_eq(&other.by_locator)
            && self.by_source.ptr_eq(&other.by_source)
            && self.witnesses.ptr_eq(&other.witnesses)
            && self.by_entry.ptr_eq(&other.by_entry)
            && self.plans.ptr_eq(&other.plans)
            && self.plans_by_locator.ptr_eq(&other.plans_by_locator)
            && self.plans_by_entry.ptr_eq(&other.plans_by_entry)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum Trigger {
    Plan(ClaimRef),
    Source(ObservationRef),
    Witness(ClaimRef),
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Position {
    source: ObservationRef,
    plan: Option<ClaimRef>,
    witness: Option<ClaimRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    triggers: Vec<Trigger>,
    trigger: usize,
    after: Position,
}
pub(crate) fn has_continuation(input: &MapInput<'_>) -> bool {
    input.state.values.contains_key(CONTINUATION)
}

pub(crate) fn refine(input: &MapInput<'_>, output: &mut MapOutput) {
    let cursor: Option<Cursor> = match input
        .state
        .values
        .get(CONTINUATION)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
    {
        Ok(value) => value,
        Err(_) => {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::DecodeFailure,
                message: "The Pi copy continuation is invalid".into(),
            });
            return;
        }
    };
    output.state.values.remove(CONTINUATION);
    if let Some(state) = &mut output.continuation_state {
        state.values.remove(CONTINUATION);
    }
    if cursor.is_none()
        && output.pi_copy_witnesses.is_empty()
        && output.pi_copy_plans.is_empty()
        && !output.source_definitions.iter().any(|source| {
            source.definition.owner.anchor_session().agent.as_str() == "pi"
                && matches!(
                    source.definition.basis,
                    SourceOwnerBasis::NativeIdentity { .. }
                )
        })
    {
        return;
    }
    let mut index = input.archive.pi_copies.clone();
    let mut triggers = cursor
        .as_ref()
        .map_or_else(Vec::new, |cursor| cursor.triggers.clone());
    for source in &output.source_definitions {
        index.retain_source(source.clone());
        if index.sources.contains_key(&source.observation) {
            triggers.push(Trigger::Source(source.observation.clone()));
        }
    }
    for witness in &output.pi_copy_witnesses {
        index.retain_witness(witness.clone());
        triggers.push(Trigger::Witness(witness.base.clone()));
    }
    for plan in &output.pi_copy_plans {
        index.retain_plan(plan.clone());
        triggers.push(Trigger::Plan(plan.base.clone()));
    }
    if cursor.is_none() {
        triggers.sort();
        triggers.dedup();
    }
    if index.sources.is_empty() || index.plans.is_empty() || index.witnesses.is_empty() {
        return;
    }
    let local: BTreeMap<_, _> = output
        .claims
        .iter()
        .map(|claim| (claim.claim_ref.clone(), claim.clone()))
        .collect();
    let mut budget = COPY_PAGE_WORK;
    let mut seen = BTreeSet::new();
    for (at, trigger) in triggers
        .iter()
        .enumerate()
        .skip(cursor.as_ref().map_or(0, |cursor| cursor.trigger))
    {
        let after = cursor
            .as_ref()
            .filter(|cursor| cursor.trigger == at)
            .map(|cursor| &cursor.after);
        if let Some(last) = scan(
            &index,
            input,
            &local,
            trigger,
            after,
            &mut budget,
            &mut output.claims,
            &mut seen,
        ) {
            let state = output
                .continuation_state
                .get_or_insert_with(|| input.state.clone());
            state.values.insert(
                CONTINUATION.into(),
                serde_json::to_value(Cursor {
                    triggers: triggers.clone(),
                    trigger: at,
                    after: last,
                })
                .unwrap(),
            );
            output.requests.push(ArchiveRequest::ContinueCopies);
            return;
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn scan(
    index: &Index,
    input: &MapInput<'_>,
    local: &BTreeMap<ClaimRef, Claim>,
    trigger: &Trigger,
    after: Option<&Position>,
    budget: &mut usize,
    output: &mut Vec<Claim>,
    seen: &mut BTreeSet<ClaimRef>,
) -> Option<Position> {
    let empty_sources = imbl::OrdSet::new();
    let singleton_source;
    let sources = match trigger {
        Trigger::Plan(base) => {
            let plan = index.plans.get(base)?;
            index
                .by_locator
                .get(&LocatorKey {
                    issuer: position(base).source.issuer.clone(),
                    locator: plan.locator.clone(),
                })
                .unwrap_or(&empty_sources)
        }
        Trigger::Source(source) => {
            singleton_source = std::iter::once(source.clone()).collect();
            &singleton_source
        }
        Trigger::Witness(base) => {
            let p = position(base);
            index
                .by_source
                .get(&(p.source.clone(), p.generation))
                .unwrap_or(&empty_sources)
        }
    };
    for source_id in sources.range((
        after.map_or(Unbounded, |after| Included(after.source.clone())),
        Unbounded,
    )) {
        let source = index.sources.get(source_id)?;
        let marker = Position {
            source: source_id.clone(),
            plan: None,
            witness: None,
        };
        if after.is_none_or(|after| &marker > after) && charge(budget) {
            return Some(marker);
        }
        let locator = LocatorKey {
            issuer: source.definition.source.issuer.clone(),
            locator: source.definition.locator.clone(),
        };
        let empty_plans = imbl::OrdSet::new();
        let singleton_plan;
        let plans = match trigger {
            Trigger::Plan(base) => {
                singleton_plan = std::iter::once(base.clone()).collect();
                &singleton_plan
            }
            Trigger::Source(_) => index.plans_by_locator.get(&locator).unwrap_or(&empty_plans),
            Trigger::Witness(base) => index
                .plans_by_entry
                .get(&(locator, index.witnesses.get(base)?.key.clone()))
                .unwrap_or(&empty_plans),
        };
        let same_source = after.filter(|after| &after.source == source_id);
        let lower = same_source
            .and_then(|after| after.plan.as_ref())
            .map_or(Unbounded, |plan| Included(plan.clone()));
        for plan_id in plans.range((lower, Unbounded)) {
            let plan = index.plans.get(plan_id)?;
            let marker = Position {
                source: source_id.clone(),
                plan: Some(plan_id.clone()),
                witness: None,
            };
            if after.is_none_or(|after| &marker > after) && charge(budget) {
                return Some(marker);
            }
            let empty_witnesses = imbl::OrdSet::new();
            let singleton_witness;
            let witnesses = match trigger {
                Trigger::Witness(base) => {
                    singleton_witness = std::iter::once(base.clone()).collect();
                    &singleton_witness
                }
                _ => index
                    .by_entry
                    .get(&(
                        source.definition.source.clone(),
                        source.definition.generation,
                        plan.key.clone(),
                    ))
                    .unwrap_or(&empty_witnesses),
            };
            let same_plan = same_source.filter(|after| after.plan.as_ref() == Some(plan_id));
            let lower = same_plan
                .and_then(|after| after.witness.as_ref())
                .map_or(Unbounded, |witness| Excluded(witness.clone()));
            for witness in witnesses.range((lower, Unbounded)) {
                let marker = Position {
                    source: source_id.clone(),
                    plan: Some(plan_id.clone()),
                    witness: Some(witness.clone()),
                };
                if let Some(claim) = matched_claim(input, local, source, plan, witness) {
                    if seen.insert(claim.claim_ref.clone()) {
                        output.push(claim);
                    }
                }
                if charge(budget) {
                    return Some(marker);
                }
            }
        }
    }
    None
}
fn charge(budget: &mut usize) -> bool {
    *budget -= 1;
    *budget == 0
}
fn matched_claim(
    input: &MapInput<'_>,
    local: &BTreeMap<ClaimRef, Claim>,
    source: &RecordedSource,
    plan: &Plan,
    witness: &ClaimRef,
) -> Option<Claim> {
    let lookup = |reference: &ClaimRef| {
        local
            .get(reference)
            .or_else(|| input.archive.claims.get(reference))
    };
    let copied = lookup(&plan.base)?;
    let original = lookup(witness)?;
    let Fact::Declare {
        object: TraceObject::HistoryEntry(copy),
    } = &copied.fact
    else {
        return None;
    };
    let Fact::Declare {
        object: TraceObject::HistoryEntry(origin),
    } = &original.fact
    else {
        return None;
    };
    if copy.entry_ref == origin.entry_ref || origin.containing_agent != source.definition.owner {
        return None;
    }
    let SourceOwnerBasis::NativeIdentity {
        observation: header,
    } = &source.definition.basis
    else {
        return None;
    };
    let original_position = position(witness).clone();
    let copied_position = position(&plan.base).clone();
    let mut claim = copied.clone();
    let ObjectAddress::Occurrence {
        scope,
        position,
        local_slot,
    } = claim.claim_ref.address()
    else {
        return None;
    };
    let digest = blake3::hash(&serde_json::to_vec(&(&source.observation, witness)).unwrap());
    let mut slot = local_slot.clone();
    slot.push(format!("pi-copy-{digest}"));
    claim.claim_ref = ClaimRef::occurrence(scope.clone(), position.clone(), slot).unwrap();
    claim.fact = Fact::Relation {
        relation: Relation::ContextCopy {
            original: origin.entry_ref.clone(),
            copied: copy.entry_ref.clone(),
            original_position,
            copied_position,
        },
    };
    claim.rule_id = "pi/native-copy-entry".into();
    claim.evidence.extend(original.evidence.iter().cloned());
    claim
        .evidence
        .extend([header.clone(), source.observation.clone()]);
    claim.evidence.sort();
    claim.evidence.dedup();
    claim.supersedes.clear();
    Some(claim)
}
