//! Returned IDs describe children only through the retained built-in invocation.
use super::*;

pub(super) enum ResultInterpretation {
    AwaitingInvocation,
    Unclassified,
    Outcome(TerminalOutcome, Vec<ObservationRef>),
}
fn interpreted(
    outcome: Option<TerminalOutcome>,
    evidence: Vec<ObservationRef>,
) -> ResultInterpretation {
    match outcome {
        Some(outcome) => ResultInterpretation::Outcome(outcome, evidence),
        None => ResultInterpretation::Unclassified,
    }
}

impl Context<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn delegation_result(
        &mut self,
        owner: &AgentRef,
        root: &AgentRef,
        operation: &OperationRef,
        part: &Value,
        value: &Value,
        slot: &str,
    ) -> ResultInterpretation {
        if part.get("is_error").and_then(Value::as_bool) == Some(true) {
            return ResultInterpretation::Unclassified;
        }
        let Some(invocation) = self.input.archive.semantic.operations.get(operation) else {
            if value.get("agentId").is_some()
                || value.get("resumedAgentId").is_some()
                || value.get("success").is_some()
            {
                self.output.requests.push(ArchiveRequest::Object {
                    object_ref: AnyObjectRef::Operation(operation.clone()),
                });
                return ResultInterpretation::AwaitingInvocation;
            }
            return ResultInterpretation::Unclassified;
        };
        let resumes = matches!(&invocation.category, OperationCategory::Tool { namespace: Some(namespace), name } if namespace == "claude" && name == "SendMessage");
        let delegates = invocation.category == OperationCategory::Delegation;
        if !resumes && !delegates {
            return ResultInterpretation::Unclassified;
        }
        // A complete local pair keeps its own invocation proof. Later native/SDK
        // confirmations of the same operation must not rewrite an earlier claim's
        // evidence merely by growing the archive's observation set.
        let mut proof =
            if let Some(observation) = self.state.invocation_evidence.get(&operation.encode()) {
                vec![observation.clone()]
            } else {
                self.input
                    .archive
                    .semantic
                    .object_evidence
                    .get(&AnyObjectRef::Operation(operation.clone()))
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
            };
        proof.push(self.input.canonical_observation.clone());
        proof.sort();
        proof.dedup();
        let outcome = if resumes {
            value
                .get("success")
                .and_then(Value::as_bool)
                .map(|success| {
                    if success {
                        TerminalOutcome::Succeeded
                    } else {
                        TerminalOutcome::Failed
                    }
                })
        } else {
            match text(value, "status") {
                Some("completed" | "async_launched") => Some(TerminalOutcome::Succeeded),
                _ => None,
            }
        };
        if resumes && outcome != Some(TerminalOutcome::Succeeded) {
            return interpreted(outcome, proof);
        }
        let Some(id) = text(value, if resumes { "resumedAgentId" } else { "agentId" })
            .filter(|id| !id.is_empty())
        else {
            return interpreted(outcome, proof);
        };
        let child = Self::child(root, id);
        let first = self.output.claims.len();
        self.declare_agent(
            &child,
            None,
            text(value, "resolvedModel").map(str::to_owned),
        );
        let ObjectAddress::Native { key, .. } = operation.address() else {
            return interpreted(outcome, proof);
        };
        let call_id = key
            .first()
            .expect("native Claude invocation has its call ID");
        let parent_binding = correlation_key(
            root,
            "claude.parent-tool-use",
            std::slice::from_ref(call_id),
        );
        self.output
            .correlations
            .insert(parent_binding.clone(), AnyObjectRef::Agent(child.clone()));
        let mut bindings = vec![parent_binding];
        let creates = correlation_key(owner, "claude.delegation-create", &[operation.encode()]);
        if delegates && self.input.archive.correlations.contains_key(&creates) {
            self.add(
                owner,
                &format!("{slot}/spawn"),
                Fact::Relation {
                    relation: Relation::Spawn {
                        parent: owner.clone(),
                        child: child.clone(),
                        operation: Some(operation.clone()),
                    },
                },
            );
            let spawn_binding = correlation_key(
                &child,
                "claude.spawn-operation",
                std::slice::from_ref(call_id),
            );
            self.output.correlations.insert(
                spawn_binding.clone(),
                AnyObjectRef::Operation(operation.clone()),
            );
            bindings.push(spawn_binding);
        }
        self.add(
            owner,
            &format!("{slot}/interaction"),
            Fact::Relation {
                relation: Relation::Interaction {
                    sender: owner.clone(),
                    recipient: child,
                    delivery: None,
                    operation: Some(operation.clone()),
                },
            },
        );
        for claim in &mut self.output.claims[first..] {
            claim.evidence.extend(proof.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
        // Forwarding can continue to name the original call after resume. Each
        // explicit returned child binding has its own proof and never changes the
        // original tool-to-child association to the currently active parent call.
        for key in bindings {
            if self
                .input
                .archive
                .correlations
                .get(&key)
                .is_none_or(|old| self.output.correlations.get(&key) == Some(old))
            {
                self.output.corroborating_correlations.insert(key.clone());
            }
            self.output.correlation_evidence.insert(key, proof.clone());
        }
        interpreted(outcome, proof)
    }
}
