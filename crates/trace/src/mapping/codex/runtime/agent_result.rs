//! The raw result observation records a successful harness submission to the parent.
//! It is neither the child's authored answer nor the parent's model-visible history.
use super::*;

impl Context<'_, '_> {
    pub(super) fn agent_result(&mut self, owner: &AgentRef, envelope: &Value, event: &Value) {
        let identifiers = (
            string(event, "child_thread_id").filter(|value| !value.is_empty()),
            string(event, "child_codex_turn_id").filter(|value| !value.is_empty()),
            string(event, "parent_thread_id").filter(|value| !value.is_empty()),
            string(event, "edge_id").filter(|value| !value.is_empty()),
            string(envelope, "rollout_id").filter(|value| !value.is_empty()),
        );
        let (Some(child), Some(turn), Some(parent), Some(edge), Some(rollout)) = identifiers else {
            self.issue(
                owner,
                "codex/agent-result-identity",
                DiagnosticCode::MissingOwner,
                "Agent result observation lacks its explicit child, turn, parent or edge identity",
            );
            return;
        };
        let (Some(child), Some(parent)) = (self.agent(child), self.agent(parent)) else {
            return;
        };
        if &child != owner
            || child == parent
            || string(envelope, "codex_turn_id").is_some_and(|value| value != turn)
        {
            self.issue(owner,"codex/agent-result-scope",DiagnosticCode::ConflictingOwner,
                "Agent result observation differs from its captured child scope or names itself as parent");
            return;
        }
        let Some(message) = string(event, "message") else {
            self.issue(
                owner,
                "codex/agent-result-message",
                DiagnosticCode::DecodeFailure,
                "Agent result observation has no exact carried message string",
            );
            return;
        };
        let descriptor = &event["carried_payload"];
        let carried = if descriptor.is_null() {
            None
        } else {
            let Some(carried) = self.raw_payload_value("carried_payload") else {
                self.issue(
                    owner,
                    "codex/agent-result-carried",
                    DiagnosticCode::DecodeFailure,
                    "Agent result carried metadata is not a retained JSON object",
                );
                return;
            };
            if string(&carried, "message") != Some(message) {
                self.issue(
                    owner,
                    "codex/agent-result-carried-mismatch",
                    DiagnosticCode::ConflictingOwner,
                    "Agent result message differs from its separately retained carried payload",
                );
                return;
            }
            if string(&carried, "child_agent_path").is_none() || carried.get("status").is_none() {
                self.issue(
                    owner,
                    "codex/agent-result-carried-shape",
                    DiagnosticCode::DecodeFailure,
                    "Agent result carried metadata omits its native path or status",
                );
                return;
            }
            Some(carried)
        };
        let message_ref = MessageRef::occurrence(
            child.clone(),
            self.input.capture.source_position.clone(),
            vec!["agent-result-notice".into()],
        )
        .unwrap();
        let revision = MessageRevisionRef::occurrence(
            child.clone(),
            self.input.capture.source_position.clone(),
            vec!["agent-result-notice".into()],
        )
        .unwrap();
        let body = self.content(message.as_bytes().to_vec(), "/payload/message");
        let mut parts = vec![ContentPart {
            part_ref: MessagePartRef::occurrence(
                child.clone(),
                self.input.capture.source_position.clone(),
                vec!["agent-result-message".into()],
            )
            .unwrap(),
            kind: ContentKind::Text,
            payload: body.reference,
            media_type: Some("text/plain".into()),
            encoding: PayloadEncoding::Utf8,
        }];
        if carried.is_some() {
            if let Some(payload) = self.raw_payload(
                &child,
                descriptor,
                "carried_payload",
                AnyObjectRef::Message(message_ref.clone()),
            ) {
                parts.push(ContentPart {
                    part_ref: MessagePartRef::occurrence(
                        child.clone(),
                        self.input.capture.source_position.clone(),
                        vec!["agent-result-metadata".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Structured,
                    payload: payload.reference,
                    media_type: Some("application/json".into()),
                    encoding: PayloadEncoding::Utf8,
                });
            }
        }
        self.declare(
            &child,
            "codex/agent-result-notice",
            TraceObject::Message(Message {
                message_ref: message_ref.clone(),
                author: Author::Harness {
                    identity: Some("codex.agent-result".into()),
                },
                protocol_role: None,
                purpose: MessagePurpose::Notice,
                production: None,
            }),
        );
        self.declare(
            &child,
            "codex/agent-result-revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: message_ref,
                representation: Representation::Runtime,
                form: RevisionForm::Complete,
                parts,
            }),
        );
        // edge_id describes a native route. Repeated notification observations do
        // not become one occurrence merely because that route and text are equal.
        let delivery = DeliveryRef::occurrence(
            parent.clone(),
            self.input.capture.source_position.clone(),
            vec!["agent-result-delivery".into()],
        )
        .unwrap();
        self.declare(
            &parent,
            "codex/agent-result-delivery",
            TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender: Author::Agent {
                    agent: OwnerRef::Agent {
                        agent: child.clone(),
                    },
                },
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent {
                        agent: parent.clone(),
                    },
                },
                route: DeliveryRoute::VendorMailbox {
                    namespace: "codex.agent-result".into(),
                    occurrence: vec![rollout.into(), edge.into(), turn.into()],
                },
            }),
        );
        self.add(
            &parent,
            "codex/agent-result-delivery-revision",
            Fact::DeliveryRevisionObserved {
                delivery: delivery.clone(),
                revision,
            },
        );
        // Pinned core/session/mod.rs records this only after send_op succeeds.
        // Success proves harness submission, not history admission or inference.
        self.add(
            &parent,
            "codex/agent-result-accepted",
            Fact::DeliveryStage {
                delivery: delivery.clone(),
                stage: DeliveryStage::Accepted,
            },
        );
        self.add(
            &child,
            "codex/agent-result-interaction",
            Fact::Relation {
                relation: Relation::Interaction {
                    sender: child.clone(),
                    recipient: parent,
                    delivery: Some(delivery),
                    operation: None,
                },
            },
        );
    }
}
