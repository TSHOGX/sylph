//! Dispatched input correlation preserves submitted bytes and vendor representations.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NativeInput {
    owner: AgentRef,
    revision: MessageRevisionRef,
    turn: String,
    position: SourcePosition,
    evidence: ObservationRef,
}
impl Context<'_, '_> {
    pub(super) fn remember_native_input(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        revision: &MessageRevisionRef,
    ) {
        if self.input.capture.transport_kind != TransportKind::NativeJsonl
            || string(value, "type") != Some("message")
            || string(value, "role") != Some("user")
        {
            return;
        }
        let metadata = &value["internal_chat_message_metadata_passthrough"];
        let Some(turn) = string(metadata, "turn_id") else {
            return;
        };
        let Some(kinds) = metadata.get("content_item_kinds").and_then(Value::as_array) else {
            return;
        };
        let Some(parts) = value.get("content").and_then(Value::as_array) else { return; };
        if kinds.is_empty() || kinds.len() != parts.len()
            || kinds.iter().any(|kind| kind.as_str() != Some("user.text"))
            || parts.iter().any(|part| string(part, "type") != Some("input_text") || string(part, "text").is_none()) {
            return;
        }
        self.state.native_input = Some(NativeInput {
            owner: owner.clone(),
            revision: revision.clone(),
            turn: turn.into(),
            position: self.input.capture.source_position.clone(),
            evidence: self.input.canonical_observation.clone(),
        });
    }
    fn paired_native_input(&self, owner: &AgentRef, envelope: &Value) -> Option<&NativeInput> {
        let previous = self.preceding_input.as_ref()?;
        if previous.owner != *owner
            || string(envelope, "turn_id") != Some(previous.turn.as_str())
            || previous.position.source != self.input.capture.source_position.source
            || previous.position.generation != self.input.capture.source_position.generation
        {
            return None;
        }
        let (
            SourceLocator::Jsonl {
                byte_end,
                record_ordinal: before,
                ..
            },
            SourceLocator::Jsonl {
                byte_start,
                record_ordinal: current,
                ..
            },
        ) = (
            &previous.position.locator,
            &self.input.capture.source_position.locator,
        )
        else {
            return None;
        };
        (*byte_end == *byte_start && before.checked_add(1) == Some(*current)).then_some(previous)
    }
    pub(super) fn input_admission(
        &mut self,
        owner: &AgentRef,
        item: &Value,
        live: bool,
        envelope: &Value,
        revision: &MessageRevisionRef,
    ) {
        let Some(client) = string(item, if live { "clientId" } else { "client_id" })
            .filter(|id| sylph_protocol::trace::is_input_client_id(id))
        else {
            return;
        };
        let mut bindings = vec![];
        let mut evidence = vec![];
        let mut missing = false;
        for namespace in [
            "sylph.dispatched-input.delivery",
            "sylph.dispatched-input.revision",
        ] {
            let key = correlation_key(owner, namespace, &[client.into()]);
            if let Some(reference) = self.input.archive.correlations.get(&key) {
                bindings.push(reference.clone());
                evidence.extend(
                    self.input
                        .archive
                        .correlation_evidence(&key)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            } else {
                missing = true;
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: owner.clone(),
                    namespace: namespace.into(),
                    key: vec![client.into()],
                });
            }
        }
        if missing {
            return;
        }
        let [
            AnyObjectRef::Delivery(delivery),
            AnyObjectRef::MessageRevision(submitted),
        ] = bindings.as_slice()
        else {
            self.issue(
                owner,
                "codex/input-correlation-kind",
                DiagnosticCode::ConflictingIdentity,
                "Dispatched input correlation must identify its delivery and submitted revision",
            );
            return;
        };
        if delivery.scope() != owner || submitted.scope() != owner || evidence.is_empty() {
            self.issue(
                owner,
                "codex/input-correlation-owner",
                DiagnosticCode::ConflictingOwner,
                "Input correlation must preserve the actual recipient and retained dispatch proof",
            );
            return;
        }
        let first = self.output.claims.len();
        self.add(
            owner,
            "codex/input-admitted",
            Fact::DeliveryStage {
                delivery: delivery.clone(),
                stage: DeliveryStage::Admitted {
                    entry: self.output.state.previous_entry.clone(),
                },
            },
        );
        self.add(
            owner,
            "codex/input-display-representation",
            Fact::Relation {
                relation: Relation::Representation {
                    original: submitted.clone(),
                    transformed: revision.clone(),
                    rule: "codex/client-input-id".into(),
                },
            },
        );
        if !live {
            if let Some(previous) = self.paired_native_input(owner, envelope).cloned() {
                // The pinned producer serially records UserInput history and then
                // its display item. This certificate requires the complete adjacent
                // native records, matching owner/turn and explicit user.text kinds;
                // equal text or receipt clocks alone cannot create this relation.
                self.add(
                    owner,
                    "codex/input-history-representation",
                    Fact::Relation {
                        relation: Relation::Representation {
                            original: submitted.clone(),
                            transformed: previous.revision,
                            rule: "codex/record-user-prompt-and-emit-turn-item".into(),
                        },
                    },
                );
                self.output
                    .claims
                    .last_mut()
                    .unwrap()
                    .evidence
                    .push(previous.evidence);
            }
        }
        for claim in &mut self.output.claims[first..] {
            claim.evidence.extend(evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
    }
}
