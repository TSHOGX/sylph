//! Native routing follows explicit root identity and root-scoped path certificates.
use super::*;

use crate::mapping::owner_resolution::Plan;

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
        if own_path && root.is_none() {
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
    fn preceding_communication(&self) -> Option<&CommunicationMetadata> {
        self.state
            .communication_metadata
            .as_ref()
            .filter(|metadata| {
                input::adjacent_native(&metadata.position, &self.input.capture.source_position)
            })
    }
    pub(super) fn describe_communication(&mut self, first: usize, author: Author) {
        let metadata = self
            .preceding_communication()
            .filter(|metadata| metadata.trigger)
            .cloned();
        for claim in &mut self.output.claims[first..] {
            if let Fact::Declare {
                object: TraceObject::Message(message),
            } = &mut claim.fact
            {
                message.author = author.clone();
                message.protocol_role = None;
                message.production = None;
                message.purpose = if let Some(metadata) = &metadata {
                    if !claim.evidence.contains(&metadata.observation) {
                        claim.evidence.push(metadata.observation.clone());
                    }
                    MessagePurpose::TaskInput
                } else {
                    MessagePurpose::Unclassified
                };
            }
        }
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
        self.describe_communication(first_claim, sender.clone());
        // A fresh delivery creates local history. Its sender belongs to Message
        // and Delivery; original-owner evidence is reserved for copied entries.
        self.history(
            containing,
            vec![HistoryContent::Message(revision.clone())],
            None,
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
        if let Some(metadata) = self
            .preceding_communication()
            .filter(|metadata| metadata.trigger)
            .cloned()
        {
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
                if !claim.evidence.contains(&metadata.observation) {
                    claim.evidence.push(metadata.observation);
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
                    object: TraceObject::Message(_),
                } => (key(author.as_ref()), None),
                Fact::Declare {
                    object: TraceObject::Delivery(_),
                } => (key(author.as_ref()), key(recipient.as_ref())),
                _ => continue,
            };
            if author.is_some() || recipient.is_some() {
                self.output.owner_resolution_plans.push(Plan {
                    base: base.claim_ref.clone(),
                    owner: author,
                    recipient,
                });
            }
        }
    }
}
