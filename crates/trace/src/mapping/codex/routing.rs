//! Native routing follows explicit root identity and root-scoped path certificates.
use super::*;

mod refinement;
pub use refinement::{Plan, PlanIndex, retain_plan, ROUTE_PAGE_WORK};
pub(super) use refinement::{refine, resume};

impl Context<'_, '_> {
    fn communication_owner(&mut self, containing: &AgentRef, path: &str) -> OwnerRef {
        let root = self.state.accounting_root.clone();
        let own_path = self.state.agent_path.as_deref() == Some(path);
        let unresolved = |scope: AgentRef, reason: &str| OwnerRef::Unresolved {
            scope,
            namespace: "codex.agent-path".into(),
            native_key: vec![path.into()],
            reason: reason.into(),
        };
        // SessionMeta.session_id is the root thread ID, and the vendor registry
        // registers it at /root even when the root header omits agent_path.
        // The containing thread is never a substitute for that root identity.
        if path == "/root" {
            if let Some(root) = &root {
                if own_path && root != containing {
                    let reason = "Native agent_path contradicts the explicit root thread identity";
                    self.issue(
                        containing,
                        "codex/root-path-conflict",
                        DiagnosticCode::ConflictingOwner,
                        reason,
                    );
                    return unresolved(root.clone(), reason);
                }
                return OwnerRef::Agent {
                    agent: root.clone(),
                };
            }
        }
        if own_path {
            return OwnerRef::Agent {
                agent: containing.clone(),
            };
        }
        let Some(root) = root else {
            let reason = "A cross-agent path requires the native root thread identity";
            self.issue(
                containing,
                &format!("codex/route-root-missing/{path}"),
                DiagnosticCode::MissingOwner,
                reason,
            );
            return unresolved(containing.clone(), reason);
        };
        unresolved(
            root,
            "Agent path requires a retained root-scoped identity binding",
        )
    }
    pub(super) fn communication(&mut self, containing: &AgentRef, value: &Value) {
        let author = string(value, "author").map(|path| self.communication_owner(containing, path));
        let recipient =
            string(value, "recipient").map(|path| self.communication_owner(containing, path));
        // The routed occurrence is identified in its containing native history.
        // Authorship may resolve later; neither its address nor an earlier claim
        // can change when a peer header arrives.
        let first_claim = self.output.claims.len();
        let Some(revision) =
            self.message(containing, value, "/payload", None, RevisionForm::Complete)
        else {
            return;
        };
        let sender = author
            .clone()
            .map_or(Author::Unknown, |agent| Author::Agent { agent });
        // The authored message and its containing history can have different owners.
        if let Some(Claim {
            fact:
                Fact::Declare {
                    object: TraceObject::Message(message),
                },
            ..
        }) = self.output.claims.iter_mut().rev().find(|claim| {
            matches!(
                &claim.fact,
                Fact::Declare {
                    object: TraceObject::Message(_)
                }
            )
        }) {
            message.author = sender.clone();
            message.purpose = if self
                .state
                .communication_metadata
                .as_ref()
                .is_some_and(|(trigger, _)| *trigger)
            {
                MessagePurpose::TaskInput
            } else {
                MessagePurpose::Unclassified
            };
        }
        self.history(
            containing,
            vec![HistoryContent::Message(revision.clone())],
            author.clone(),
        );
        let entry = self.output.state.previous_entry.clone();
        let delivery = DeliveryRef::occurrence(
            containing.clone(),
            self.input.capture.source_position.clone(),
            vec!["agent-message".into()],
        )
        .unwrap();
        self.declare(
            containing,
            "codex/communication-delivery",
            TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender,
                recipient: recipient
                    .clone()
                    .map_or(Recipient::Unknown, |agent| Recipient::Agent { agent }),
                route: DeliveryRoute::VendorMailbox {
                    namespace: "codex.agent-message".into(),
                    occurrence: vec![string(value, "id").unwrap_or("").into()],
                },
            }),
        );
        self.add(
            containing,
            "codex/communication-delivery-revision",
            Fact::DeliveryRevisionObserved {
                delivery: delivery.clone(),
                revision,
            },
        );
        self.add(
            containing,
            "codex/communication-admitted",
            Fact::DeliveryStage {
                delivery: delivery.clone(),
                stage: DeliveryStage::Admitted { entry },
            },
        );
        if let Some((true, metadata)) = self.state.communication_metadata.clone() {
            if let Some(activation) = self
                .state
                .active_by_agent
                .get(&containing.encode())
                .cloned()
            {
                self.add(
                    containing,
                    "codex/communication-trigger",
                    Fact::Relation {
                        relation: Relation::Trigger {
                            delivery,
                            activation,
                        },
                    },
                );
                let claim = self.output.claims.last_mut().unwrap();
                if !claim.evidence.contains(&metadata) {
                    claim.evidence.push(metadata);
                }
            }
        }
        for base in &self.output.claims[first_claim..] {
            let key = |owner: Option<&OwnerRef>| {
                let OwnerRef::Unresolved {
                    scope,
                    namespace,
                    native_key,
                    ..
                } = owner?
                else {
                    return None;
                };
                (self.state.accounting_root.as_ref() == Some(scope)
                    && native_key.first().is_some_and(|path| path != "/root"))
                .then(|| correlation_key(scope, namespace, native_key))
            };
            let (author, recipient) = match &base.fact {
                Fact::Declare {
                    object: TraceObject::Message(_) | TraceObject::HistoryEntry(_),
                } => (key(author.as_ref()), None),
                Fact::Declare {
                    object: TraceObject::Delivery(_),
                } => (key(author.as_ref()), key(recipient.as_ref())),
                _ => continue,
            };
            if author.is_some() || recipient.is_some() {
                self.output.routing_plans.push(Plan {
                    base: base.claim_ref.clone(),
                    author,
                    recipient,
                });
            }
        }
    }
}
