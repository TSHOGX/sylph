//! Native routing follows explicit root identity and root-scoped path certificates.
use super::*;

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
        let key = correlation_key(&root, "codex.agent-path", &[path.into()]);
        if !self.input.archive.correlations.contains_key(&key) {
            self.output.requests.push(ArchiveRequest::Correlation {
                agent: root.clone(),
                namespace: "codex.agent-path".into(),
                key: vec![path.into()],
            });
        }
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
        self.refine_communication(first_claim, author.as_ref(), recipient.as_ref());
    }

    fn routing_resolution(
        &self,
        owner: Option<&OwnerRef>,
    ) -> Option<(OwnerRef, Vec<ObservationRef>)> {
        let OwnerRef::Unresolved {
            scope,
            namespace,
            native_key,
            ..
        } = owner?
        else {
            return None;
        };
        if native_key.first().is_some_and(|path| path == "/root")
            || self.state.accounting_root.as_ref() != Some(scope)
        {
            return None;
        }
        let key = correlation_key(scope, namespace, native_key);
        let AnyObjectRef::Agent(agent) = self.input.archive.correlations.get(&key)? else {
            return None;
        };
        let evidence = self.input.archive.correlation_evidence.get(&key)?;
        if evidence.is_empty() {
            return None;
        }
        Some((
            OwnerRef::Agent {
                agent: agent.clone(),
            },
            evidence.clone(),
        ))
    }

    fn refine_communication(
        &mut self,
        first: usize,
        author: Option<&OwnerRef>,
        recipient: Option<&OwnerRef>,
    ) {
        let author = self.routing_resolution(author);
        let recipient = self.routing_resolution(recipient);
        let known = u8::from(author.is_some()) | (u8::from(recipient.is_some()) << 1);
        let originals = self.output.claims[first..].to_vec();
        for base in originals {
            let relevant = match &base.fact {
                Fact::Declare {
                    object: TraceObject::Message(_) | TraceObject::HistoryEntry(_),
                } => 1,
                Fact::Declare {
                    object: TraceObject::Delivery(_),
                } => 3,
                _ => continue,
            };
            // Emit every evidenced subset, even on cold replay. A two-sided join
            // supersedes each one-sided refinement, so source arrival order cannot
            // change either the retained claims or the resolved record.
            for mask in 1..=3u8 {
                if mask & known != mask || mask & relevant != mask {
                    continue;
                }
                let mut refined = base.clone();
                refined.claim_ref = route_claim(&base.claim_ref, mask);
                refined.supersedes.push(base.claim_ref.clone());
                for prior in 1..mask {
                    if prior & mask == prior {
                        refined.supersedes.push(route_claim(&base.claim_ref, prior));
                    }
                }
                let sender = author.as_ref().filter(|_| mask & 1 != 0);
                let target = recipient.as_ref().filter(|_| mask & 2 != 0);
                for (_, evidence) in sender.into_iter().chain(target) {
                    refined.evidence.extend(evidence.iter().cloned());
                }
                refined.evidence.sort();
                refined.evidence.dedup();
                match &mut refined.fact {
                    Fact::Declare {
                        object: TraceObject::Message(value),
                    } => {
                        value.author = Author::Agent {
                            agent: sender.unwrap().0.clone(),
                        };
                    }
                    Fact::Declare {
                        object: TraceObject::HistoryEntry(value),
                    } => {
                        value.original_owner = Some(sender.unwrap().0.clone());
                    }
                    Fact::Declare {
                        object: TraceObject::Delivery(value),
                    } => {
                        if let Some((owner, _)) = sender {
                            value.sender = Author::Agent {
                                agent: owner.clone(),
                            };
                        }
                        if let Some((owner, _)) = target {
                            value.recipient = Recipient::Agent {
                                agent: owner.clone(),
                            };
                        }
                    }
                    _ => unreachable!(),
                }
                self.output.claims.push(refined);
            }
        }
    }
}

fn route_claim(base: &ClaimRef, mask: u8) -> ClaimRef {
    let ObjectAddress::Occurrence {
        scope,
        position,
        local_slot,
    } = base.address()
    else {
        unreachable!("mapper claims identify a retained occurrence")
    };
    let mut slot = local_slot.clone();
    slot.push(format!("route-resolution-{mask}"));
    ClaimRef::occurrence(scope.clone(), position.clone(), slot).unwrap()
}
