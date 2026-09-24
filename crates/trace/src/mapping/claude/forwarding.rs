//! Retained host certificates authorize forwarded facts without re-running stream state.
use super::*;
use crate::mapping::{CorrelationProof, CorrelationProofs};
use std::{
    collections::VecDeque,
    ops::Bound::{Excluded, Unbounded},
    sync::Arc,
};

pub const PAGE_WORK: usize = 64;
const CONTINUATION: &str = "claude.forwarding-continuation";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Claim(Box<Claim>),
    Binding {
        key: String,
        reference: AnyObjectRef,
        evidence: Vec<ObservationRef>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub route: String,
    pub target: AgentRef,
    pub action: Action,
}
pub type Plans = imbl::OrdMap<String, imbl::OrdMap<String, Arc<Plan>>>;
pub fn retain_plan(plans: &mut Plans, plan: Plan) {
    plans
        .entry(plan.route.clone())
        .or_default()
        .insert(plan.id.clone(), Arc::new(plan));
}
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Trigger {
    Plan {
        route: String,
        id: String,
        after: Option<CorrelationProof>,
    },
    Certificate {
        route: String,
        proof: CorrelationProof,
        after: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    queue: VecDeque<Trigger>,
}

pub(crate) fn has_continuation(input: &MapInput<'_>) -> bool {
    input.state.values.contains_key(CONTINUATION)
}
pub(super) fn prepare(
    input: &MapInput<'_>,
    forwarded: Option<&(String, AgentRef)>,
    output: &mut MapOutput,
) {
    let Some((route, target)) = forwarded else {
        return;
    };
    for claim in std::mem::take(&mut output.claims) {
        output.forwarding_plans.push(Plan {
            // This is a private index key, not a semantic occurrence identity.
            // Fixed size keeps a continuation independent of encoded address length.
            id: format!(
                "claim:{}",
                blake3::hash(claim.claim_ref.encode().as_bytes())
            ),
            route: route.clone(),
            target: target.clone(),
            action: Action::Claim(Box::new(claim)),
        });
    }
    for (key, reference) in std::mem::take(&mut output.correlations) {
        let evidence = output
            .correlation_evidence
            .remove(&key)
            .unwrap_or_else(|| vec![input.canonical_observation.clone()]);
        let id = format!(
            "binding:{}",
            blake3::hash(&serde_json::to_vec(&(input.canonical_observation, &key)).unwrap())
        );
        output.forwarding_plans.push(Plan {
            id,
            route: route.clone(),
            target: target.clone(),
            action: Action::Binding {
                key,
                reference,
                evidence,
            },
        });
    }
    output.corroborating_correlations.clear();
}

pub(crate) fn refine(input: &MapInput<'_>, output: &mut MapOutput) {
    let resumed = input.state.values.get(CONTINUATION);
    if resumed.is_none()
        && output.forwarding_plans.is_empty()
        && !output
            .correlations
            .keys()
            .any(|key| input.archive.forwarding_plans.contains_key(key))
    {
        return;
    }
    let mut cursor: Cursor = match resumed
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
    {
        Ok(value) => value.unwrap_or(Cursor {
            queue: VecDeque::new(),
        }),
        Err(_) => {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::DecodeFailure,
                message: "Invalid forwarded-claim continuation".into(),
            });
            return;
        }
    };
    output.state.values.remove(CONTINUATION);
    if let Some(state) = &mut output.continuation_state {
        state.values.remove(CONTINUATION);
    }
    let mut plans = input.archive.forwarding_plans.clone();
    for plan in &output.forwarding_plans {
        retain_plan(&mut plans, plan.clone());
        cursor.queue.push_back(Trigger::Plan {
            route: plan.route.clone(),
            id: plan.id.clone(),
            after: None,
        });
    }
    let mut proofs = input.archive.correlation_proofs.clone();
    for (key, reference) in &output.correlations {
        let mut evidence = output
            .correlation_evidence
            .get(key)
            .cloned()
            .unwrap_or_else(|| vec![input.canonical_observation.clone()]);
        evidence.sort();
        evidence.dedup();
        let proof = CorrelationProof {
            reference: reference.clone(),
            evidence,
        };
        crate::mapping::retain_correlation_proof(
            &mut proofs,
            key.clone(),
            proof.reference.clone(),
            proof.evidence.clone(),
        );
        if plans.contains_key(key) {
            cursor.queue.push_back(Trigger::Certificate {
                route: key.clone(),
                proof,
                after: None,
            });
        }
    }
    let mut budget = PAGE_WORK;
    while let Some(trigger) = cursor.queue.front().cloned() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        let next = match &trigger {
            Trigger::Plan { route, id, after } => plans
                .get(route)
                .and_then(|values| values.get(id))
                .and_then(|plan| {
                    proofs
                        .get(route)
                        .and_then(|values| {
                            values
                                .range((after.clone().map_or(Unbounded, Excluded), Unbounded))
                                .next()
                        })
                        .map(|proof| (plan.clone(), proof.clone()))
                }),
            Trigger::Certificate {
                route,
                proof,
                after,
            } => plans
                .get(route)
                .and_then(|values| {
                    values
                        .range((after.clone().map_or(Unbounded, Excluded), Unbounded))
                        .next()
                })
                .map(|(_, plan)| (plan.clone(), proof.clone())),
        };
        let Some((plan, proof)) = next else {
            cursor.queue.pop_front();
            continue;
        };
        if proof.reference == AnyObjectRef::Agent(plan.target.clone())
            && !proof.evidence.is_empty()
            && !emit(
                &plan,
                &proof,
                output,
                &mut proofs,
                &plans,
                &mut cursor.queue,
            )
        {
            break;
        }
        match cursor.queue.front_mut().unwrap() {
            Trigger::Plan { after, .. } => *after = Some(proof),
            Trigger::Certificate { after, .. } => *after = Some(plan.id.clone()),
        }
    }
    if !cursor.queue.is_empty() {
        output
            .continuation_state
            .get_or_insert_with(|| input.state.clone())
            .values
            .insert(CONTINUATION.into(), serde_json::to_value(cursor).unwrap());
        output.requests.push(ArchiveRequest::ContinueForwarding);
    }
}

fn emit(
    plan: &Plan,
    proof: &CorrelationProof,
    output: &mut MapOutput,
    proofs: &mut CorrelationProofs,
    plans: &Plans,
    queue: &mut VecDeque<Trigger>,
) -> bool {
    match &plan.action {
        Action::Claim(base) => {
            let mut claim = base.as_ref().clone();
            let ObjectAddress::Occurrence {
                scope,
                position,
                local_slot,
            } = base.claim_ref.address()
            else {
                unreachable!("forwarded claims have captured positions")
            };
            let mut slot = local_slot.clone();
            slot.push(format!(
                "forwarding:{}",
                blake3::hash(&serde_json::to_vec(proof).unwrap())
            ));
            claim.claim_ref = ClaimRef::occurrence(scope.clone(), position.clone(), slot).unwrap();
            claim.evidence.extend(proof.evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
            output.claims.push(claim);
        }
        Action::Binding {
            key,
            reference,
            evidence,
        } => {
            let mut evidence = evidence.clone();
            evidence.extend(proof.evidence.iter().cloned());
            evidence.sort();
            evidence.dedup();
            // MapOutput has one certificate per key per pass. Yield before a
            // second alternative rather than conjoining or overwriting its proof.
            if output.correlations.contains_key(key)
                && output.correlation_evidence.get(key) != Some(&evidence)
            {
                return false;
            }
            let next = CorrelationProof {
                reference: reference.clone(),
                evidence: evidence.clone(),
            };
            let new = !proofs.get(key).is_some_and(|values| values.contains(&next));
            output.correlations.insert(key.clone(), reference.clone());
            output.correlation_evidence.insert(key.clone(), evidence);
            // Preserve the existing hard conflict for a route key naming a
            // different target; corroboration adds proofs only to one binding.
            if proofs.get(key).is_none_or(|values| {
                crate::mapping::unique_correlation_reference(values) == Some(reference)
            }) {
                output.corroborating_correlations.insert(key.clone());
            }
            crate::mapping::retain_correlation_proof(
                proofs,
                key.clone(),
                next.reference.clone(),
                next.evidence.clone(),
            );
            if new && plans.contains_key(key) {
                queue.push_back(Trigger::Certificate {
                    route: key.clone(),
                    proof: next,
                    after: None,
                });
            }
        }
    }
    true
}
