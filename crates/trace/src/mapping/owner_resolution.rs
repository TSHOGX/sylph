//! Indexed late certificates refine immutable declarations in bounded continuations.
use super::*;
use std::collections::BTreeMap;
use crate::mapping::{CorrelationProof, CorrelationProofs};
use std::ops::Bound::{Excluded, Included, Unbounded};

/// A plan stores no source checkpoint or body. Its base is an immutable claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub base: ClaimRef,
    pub owner: Option<String>,
    pub recipient: Option<String>,
}
pub type PlanIndex = imbl::OrdMap<String, imbl::OrdMap<ClaimRef, Plan>>;
pub const OWNER_PAGE_WORK: usize = 64;
const CONTINUATION: &str = "owner-resolution-continuation";

pub fn retain_plan(index: &mut PlanIndex, plan: Plan) {
    for key in plan.owner.iter().chain(&plan.recipient) {
        index
            .entry(key.clone())
            .or_default()
            .insert(plan.base.clone(), plan.clone());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Variant {
    mask: u8,
    owner: Option<CorrelationProof>,
    recipient: Option<CorrelationProof>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    /// None is the current record's own plans; named indexes follow in key order.
    source: Option<String>,
    plan: ClaimRef,
    /// None means the plan is finished. Otherwise resume after this certificate pair.
    after: Option<Variant>,
    local: Vec<Plan>,
    sources: Vec<String>,
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
        Ok(cursor) => cursor,
        Err(_) => {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::DecodeFailure,
                message: "The owner-resolution continuation checkpoint is invalid".into(),
            });
            return;
        }
    };
    output.state.values.remove(CONTINUATION);
    if let Some(state) = &mut output.continuation_state {
        state.values.remove(CONTINUATION);
    }
    if cursor.is_none()
        && output.owner_resolution_plans.is_empty()
        && !output
            .correlations
            .keys()
            .any(|key| input.archive.owner_resolution_plans.contains_key(key))
    {
        return;
    }
    let local_plans = cursor.as_ref().map_or_else(
        || output.owner_resolution_plans.clone(),
        |cursor| cursor.local.clone(),
    );
    let local: imbl::OrdMap<_, _> = local_plans
        .iter()
        .cloned()
        .map(|plan| (plan.base.clone(), plan))
        .collect();
    let local_claims: BTreeMap<_, _> = output
        .claims
        .iter()
        .cloned()
        .map(|claim| (claim.claim_ref.clone(), claim))
        .collect();
    let mut proofs = input.archive.correlation_proofs.clone();
    let mut keys = cursor
        .as_ref()
        .map_or_else(Vec::new, |cursor| cursor.sources.clone());
    for (key, reference) in &output.correlations {
        crate::mapping::retain_correlation_proof(
            &mut proofs,
            key.clone(),
            reference.clone(),
            output
                .correlation_evidence
                .get(key)
                .cloned()
                .unwrap_or_else(|| vec![input.canonical_observation.clone()]),
        );
        if input.archive.owner_resolution_plans.contains_key(key) {
            keys.push(key.clone());
        }
    }
    keys.sort();
    keys.dedup();
    let mut budget = OWNER_PAGE_WORK;
    for source in std::iter::once(None).chain(keys.iter().cloned().map(Some)) {
        if cursor.as_ref().is_some_and(|cursor| source < cursor.source) {
            continue;
        }
        let plans = match &source {
            None => &local,
            Some(key) => &input.archive.owner_resolution_plans[key],
        };
        let checkpoint = cursor.as_ref().filter(|cursor| cursor.source == source);
        let lower = match checkpoint {
            Some(cursor) if cursor.after.is_some() => Included(cursor.plan.clone()),
            Some(cursor) => Excluded(cursor.plan.clone()),
            None => Unbounded,
        };
        // Seek the index rather than repeatedly skipping an already visited prefix.
        // Headers only visit declarations that actually depend on their route key.
        for (reference, plan) in plans.range((lower, Unbounded)) {
            budget -= 1;
            let after = checkpoint
                .filter(|cursor| &cursor.plan == reference)
                .and_then(|cursor| cursor.after.as_ref());
            let base = local_claims
                .get(reference)
                .or_else(|| input.archive.claims.get(reference));
            let (done, last) = if let Some(base) = base {
                variants(plan, base, &proofs, after, &mut budget, &mut output.claims)
            } else {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::MissingSource,
                    message: "A retained owner-resolution plan lacks its immutable base claim"
                        .into(),
                });
                (true, Variant::default())
            };
            if budget == 0 {
                let mut state = output
                    .continuation_state
                    .take()
                    .unwrap_or_else(|| input.state.clone());
                state.values.insert(
                    CONTINUATION.into(),
                    serde_json::to_value(Cursor {
                        source,
                        plan: reference.clone(),
                        after: (!done).then_some(last),
                        local: local_plans,
                        sources: keys.clone(),
                    })
                    .expect("typed owner-resolution cursor"),
                );
                output.continuation_state = Some(state);
                output
                    .requests
                    .push(ArchiveRequest::ContinueOwnerResolution);
                return;
            }
        }
    }
}

type Certificates = imbl::OrdSet<CorrelationProof>;
fn endpoint<'a>(key: Option<&String>, proofs: &'a CorrelationProofs) -> Option<&'a Certificates> {
    proofs.get(key?).filter(|proofs| !proofs.is_empty())
}
fn owner(proof: &CorrelationProof) -> OwnerRef {
    let AnyObjectRef::Agent(agent) = &proof.reference else {
        unreachable!("owner certificates name agents")
    };
    OwnerRef::Agent {
        agent: agent.clone(),
    }
}
fn alternatives<'a>(
    endpoint: Option<&'a Certificates>,
    required: bool,
    lower: Option<&CorrelationProof>,
) -> Box<dyn Iterator<Item = Option<&'a CorrelationProof>> + 'a> {
    if !required {
        return Box::new(std::iter::once(None));
    }
    let proofs = endpoint.expect("mask requires an evidenced endpoint");
    Box::new(
        proofs
            .range((lower.cloned().map_or(Unbounded, Included), Unbounded))
            .map(Some),
    )
}

fn variants(
    plan: &Plan,
    base: &Claim,
    proofs: &CorrelationProofs,
    after: Option<&Variant>,
    budget: &mut usize,
    output: &mut Vec<Claim>,
) -> (bool, Variant) {
    let owner_proofs = endpoint(plan.owner.as_ref(), proofs);
    let recipient = endpoint(plan.recipient.as_ref(), proofs);
    let known = u8::from(owner_proofs.is_some()) | (u8::from(recipient.is_some()) << 1);
    let mut last = after.cloned().unwrap_or_default();
    for mask in 1..=3u8 {
        if mask & known != mask || after.is_some_and(|after| mask < after.mask) {
            continue;
        }
        let same_mask = after.filter(|after| mask == after.mask);
        for sender in alternatives(
            owner_proofs,
            mask & 1 != 0,
            same_mask.and_then(|after| after.owner.as_ref()),
        ) {
            let same_sender = same_mask.filter(|after| after.owner.as_ref() == sender);
            for target in alternatives(
                recipient,
                mask & 2 != 0,
                same_sender.and_then(|after| after.recipient.as_ref()),
            ) {
                let variant = Variant {
                    mask,
                    owner: sender.cloned(),
                    recipient: target.cloned(),
                };
                if after.is_some_and(|after| variant <= *after) {
                    continue;
                }
                if *budget == 0 {
                    return (false, last);
                }
                *budget -= 1;
                let mut refined = base.clone();
                refined.claim_ref = resolved_claim(&base.claim_ref, &variant);
                refined.supersedes.push(base.claim_ref.clone());
                // Every two-sided certificate supersedes its one-sided subsets.
                // Other independent certificates remain alternative sufficient proofs.
                if mask == 3 {
                    refined.supersedes.push(resolved_claim(
                        &base.claim_ref,
                        &Variant {
                            mask: 1,
                            owner: variant.owner.clone(),
                            recipient: None,
                        },
                    ));
                    refined.supersedes.push(resolved_claim(
                        &base.claim_ref,
                        &Variant {
                            mask: 2,
                            owner: None,
                            recipient: variant.recipient.clone(),
                        },
                    ));
                }
                refined.evidence.extend(
                    sender
                        .into_iter()
                        .chain(target)
                        .flat_map(|proof| proof.evidence.iter().cloned()),
                );
                refined.evidence.sort();
                refined.evidence.dedup();
                match &mut refined.fact {
                    Fact::Declare {
                        object: TraceObject::Message(value),
                    } => {
                        value.author = Author::Agent {
                            agent: owner(sender.unwrap()),
                        };
                    }
                    Fact::Declare {
                        object: TraceObject::Delivery(value),
                    } => {
                        if let Some(sender) = sender {
                            value.sender = Author::Agent {
                                agent: owner(sender),
                            };
                        }
                        if let Some(target) = target {
                            value.recipient = Recipient::Agent {
                                agent: owner(target),
                            };
                        }
                    }
                    Fact::Relation {
                        relation: Relation::ContextLineage { original_owner, .. },
                    } => {
                        *original_owner = owner(sender.unwrap());
                    }
                    _ => unreachable!("owner-resolution plans only refine their declared families"),
                }
                output.push(refined);
                last = variant;
            }
        }
    }
    (true, last)
}
fn resolved_claim(base: &ClaimRef, variant: &Variant) -> ClaimRef {
    let ObjectAddress::Occurrence {
        scope,
        position,
        local_slot,
    } = base.address()
    else {
        unreachable!("mapper claims identify a retained occurrence")
    };
    let mut slot = local_slot.clone();
    let digest =
        blake3::hash(&serde_json::to_vec(variant).expect("typed owner-resolution certificate"));
    slot.push(format!("owner-resolution-{}-{digest}", variant.mask));
    ClaimRef::occurrence(scope.clone(), position.clone(), slot).unwrap()
}
