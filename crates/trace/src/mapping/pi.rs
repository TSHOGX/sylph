//! Pi's native entry tree and outer runtime loop, interpreted from retained evidence.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sylph_protocol::{trace::*, AgentName, NativeId, SessionId};

use super::{
    claim, correlation_key, derived_payload_with_media, ArchiveRequest, MapInput, MapOutput,
    MappingDiagnostic, SemanticMapper,
};

pub struct PiMapper;

#[derive(Clone, Default, Serialize, Deserialize)]
struct PiState {
    header_evidence: Option<ObservationRef>,
    fork_source: Option<String>,
    outer: Option<OuterRun>,
    message: Option<LiveMessage>,
    last_native_entry: Option<EntryRef>,
    current_pending: BTreeMap<String, OperationRef>,
    current_parent: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OuterRun {
    activation: ActivationRef,
    start: ObservationRef,
    terminal: Option<(
        TerminalOutcome,
        Option<ExecutionCloseReason>,
        ObservationRef,
    )>,
}

#[derive(Clone, Serialize, Deserialize)]
struct LiveMessage {
    reference: MessageRef,
    start: SourcePosition,
    previous_revision: Option<MessageRevisionRef>,
}

impl SemanticMapper for PiMapper {
    fn build(&self) -> &'static str {
        "pi-1"
    }

    fn map(&self, input: MapInput<'_>) -> MapOutput {
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let value = match serde_json::from_slice::<Value>(input.raw) {
            Ok(value) => value,
            Err(error) => {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::DecodeFailure,
                    message: error.to_string(),
                });
                return output;
            }
        };
        let mut state = match input
            .state
            .values
            .get("pi")
            .map(|value| serde_json::from_value::<PiState>(value.clone()))
            .transpose()
        {
            Ok(state) => state.unwrap_or_default(),
            Err(error) => {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::DecodeFailure,
                    message: format!("Invalid Pi mapper checkpoint: {error}"),
                });
                return output;
            }
        };
        let mut context = Context {
            input: &input,
            output: &mut output,
            state: &mut state,
            local_history: BTreeMap::new(),
        };
        if input.capture.transport_kind == TransportKind::NativeJsonl {
            context.native(&value);
        } else {
            context.live(&value);
        }
        output.state.values.insert(
            "pi".into(),
            serde_json::to_value(state).expect("typed Pi state serializes"),
        );
        for key in output.correlations.keys() {
            output
                .correlation_evidence
                .entry(key.clone())
                .or_insert_with(|| vec![input.canonical_observation.clone()]);
        }
        output
    }
}

struct Context<'a, 'b> {
    input: &'a MapInput<'b>,
    output: &'a mut MapOutput,
    state: &'a mut PiState,
    local_history: BTreeMap<EntryRef, HistoryEntry>,
}

impl Context<'_, '_> {
    fn add(&mut self, owner: &AgentRef, slot: &str, fact: Fact) {
        let mut claim = claim(self.input, owner, slot, "pi-1", fact);
        if let Some(evidence) = &self.state.header_evidence {
            if !claim.evidence.contains(evidence) {
                claim.evidence.push(evidence.clone());
            }
        }
        self.output.claims.push(claim);
    }
    fn object(&mut self, owner: &AgentRef, slot: &str, object: TraceObject) {
        self.add(owner, slot, Fact::Declare { object });
    }
    fn issue(&mut self, owner: Option<&AgentRef>, slot: &str, code: DiagnosticCode, message: &str) {
        if let Some(owner) = owner {
            self.add(
                owner,
                slot,
                Fact::Diagnostic {
                    diagnostic: TraceDiagnostic {
                        subject: Some(AnyObjectRef::Agent(owner.clone())),
                        code,
                        message: message.into(),
                        related_claims: vec![],
                    },
                },
            );
        } else {
            self.output.diagnostics.push(MappingDiagnostic {
                evidence: vec![self.input.canonical_observation.clone()],
                code,
                message: message.into(),
            });
        }
    }
    fn payload(&mut self, bytes: Vec<u8>, selector: &str) -> PayloadRef {
        let body = derived_payload_with_media(
            self.input,
            bytes,
            selector,
            "pi/json-field",
            "text/plain",
            PayloadEncoding::Utf8,
        );
        let reference = body.reference.clone();
        self.output.payloads.push(body);
        reference
    }
    fn json(&mut self, value: &Value, selector: &str) -> PayloadRef {
        let payload = derived_payload_with_media(
            self.input,
            serde_json::to_vec(value).expect("JSON serializes"),
            selector,
            "pi/json-field",
            "application/json",
            PayloadEncoding::Utf8,
        );
        let reference = payload.reference.clone();
        self.output.payloads.push(payload);
        reference
    }
    fn identify(&self, id: &str) -> Option<AgentRef> {
        if id.is_empty() {
            return None;
        }
        let Issuer::Environment(environment) = &self.input.capture.observation_ref.issuer else {
            return None;
        };
        Some(AgentRef::Session(SessionId::new(
            environment.clone(),
            AgentName::parse("pi").unwrap(),
            NativeId::new(id),
        )))
    }
    fn owner(&self) -> Option<AgentRef> {
        self.output.state.governing_agent.clone()
    }
    fn declare_agent(&mut self, owner: &AgentRef, model: Option<String>) {
        self.object(
            owner,
            "pi/agent",
            TraceObject::Agent(AgentThread {
                agent_ref: owner.clone(),
                vendor: AgentName::parse("pi").unwrap(),
                name: None,
                model,
            }),
        );
    }

    fn native(&mut self, value: &Value) {
        if text(value, "type") == Some("session") {
            let Some(owner) = text(value, "id").and_then(|id| self.identify(id)) else {
                self.issue(
                    None,
                    "pi/header",
                    DiagnosticCode::MissingOwner,
                    "Pi native header has no valid session identity",
                );
                return;
            };
            self.output.state.governing_agent = Some(owner.clone());
            self.output.source_owners.push(super::SourceOwnership {
                source: self.input.capture.source_position.source.clone(),
                generation: self.input.capture.source_position.generation,
                owner: super::SourceOwner {
                    agent: owner.clone(),
                    evidence: vec![self.input.canonical_observation.clone()],
                    rule: "pi/native-session-header-owner".into(),
                },
            });
            self.state.header_evidence = Some(self.input.canonical_observation.clone());
            self.state.fork_source = vendor_field(value, "parentSession", "parent_session")
                .and_then(Value::as_str)
                .map(str::to_owned);
            self.declare_agent(&owner, None);
            self.issue(
                Some(&owner),
                "pi/native-selection",
                DiagnosticCode::UnknownSelection,
                "Native append order does not establish the live selected leaf",
            );
            self.complete_record(&owner);
            return;
        }
        let Some(owner) = self.owner() else {
            self.issue(
                None,
                "pi/no-header",
                DiagnosticCode::MissingOwner,
                "Pi entry has no retained session header",
            );
            return;
        };
        self.entry(&owner, value, "native");
    }

    fn original_owner(&mut self, containing: &AgentRef, id: &str) -> Option<OwnerRef> {
        let path = self.state.fork_source.clone()?;
        let key = vec![path.clone(), id.into()];
        match self.input.archive.correlations.get(&correlation_key(
            containing,
            "pi.fork-entry",
            &key,
        )) {
            Some(AnyObjectRef::HistoryEntry(entry)) => Some(OwnerRef::Agent {
                agent: entry.scope().clone(),
            }),
            _ => {
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: containing.clone(),
                    namespace: "pi.fork-entry".into(),
                    key: key.clone(),
                });
                Some(OwnerRef::Unresolved {
                    scope: containing.clone(),
                    namespace: "pi.fork-entry".into(),
                    native_key: key,
                    reason: "Fork source and copied membership have not been resolved".into(),
                })
            }
        }
    }

    fn entry(&mut self, owner: &AgentRef, value: &Value, selector: &str) {
        let Some(id) = text(value, "id") else {
            self.issue(
                Some(owner),
                "pi/entry-id",
                DiagnosticCode::DecodeFailure,
                "Native entry is missing its scoped entry ID",
            );
            return;
        };
        let kind = text(value, "type").unwrap_or("");
        let slot = format!("pi/entry/{id}");
        self.state.current_parent = vendor_field(value, "parentId", "parent_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let parent = self
            .state
            .current_parent
            .as_ref()
            .map(|id| EntryRef::native(owner.clone(), "pi.entry", vec![id.clone()]).unwrap());
        if parent != self.state.last_native_entry {
            self.state.current_pending.clear();
        }
        let original = self.original_owner(owner, id);
        let production = original.clone().unwrap_or_else(|| OwnerRef::Agent {
            agent: owner.clone(),
        });
        let mut content = match kind {
            "message" if text(&value["message"], "role") == Some("bashExecution") => self.bash(
                owner,
                &production,
                id,
                &value["message"],
                &format!("{selector}/{id}/bash"),
            ),
            "message" => {
                let message = &value["message"];
                let reference =
                    MessageRef::native(owner.clone(), "pi.entry.message", vec![id.into()]).unwrap();
                let revision = self.message(
                    owner,
                    reference,
                    &production,
                    message,
                    &format!("{selector}/{id}/message"),
                    Some(id),
                    RevisionForm::Complete,
                    None,
                );
                vec![HistoryContent::Message(revision)]
            }
            "custom_message" => {
                let message = serde_json::json!({"role":"custom","content":value.get("content").cloned().unwrap_or(Value::Null),"customType":value.get("customType"),"display":value.get("display"),"details":value.get("details")});
                let reference =
                    MessageRef::native(owner.clone(), "pi.entry.message", vec![id.into()]).unwrap();
                let revision = self.message(
                    owner,
                    reference,
                    &production,
                    &message,
                    &format!("{selector}/{id}"),
                    Some(id),
                    RevisionForm::Complete,
                    None,
                );
                vec![HistoryContent::Message(revision)]
            }
            "compaction" | "branch_summary" => {
                let checkpoint =
                    CheckpointRef::native(owner.clone(), "pi.checkpoint", vec![id.into()]).unwrap();
                let summary = text(value, "summary").unwrap_or("");
                let payload = self.payload(
                    summary.as_bytes().to_vec(),
                    &format!("{selector}/{id}/summary"),
                );
                self.object(
                    owner,
                    &format!("{slot}/checkpoint"),
                    TraceObject::ContextCheckpoint(ContextCheckpoint {
                        checkpoint_ref: checkpoint.clone(),
                        owner: production,
                        basis: ContextBasis::Summary,
                        content: ContextContent::Summary { payload },
                        coverage: InclusionCoverage::Summary,
                    }),
                );
                if kind == "compaction" {
                    if let (Some(first), Some(parent)) = (
                        text(value, "firstKeptEntryId"),
                        vendor_field(value, "parentId", "parent_id").and_then(Value::as_str),
                    ) {
                        self.preserved_membership(owner, &checkpoint, first, parent);
                    }
                } else if let Some(id) = text(value, "fromId") {
                    self.output.requests.push(ArchiveRequest::Object {
                        object_ref: AnyObjectRef::HistoryEntry(
                            EntryRef::native(owner.clone(), "pi.entry", vec![id.into()]).unwrap(),
                        ),
                    });
                }
                let metadata = self.json(value, &format!("{selector}/{id}"));
                vec![
                    HistoryContent::Checkpoint(checkpoint),
                    HistoryContent::Opaque(metadata),
                ]
            }
            "model_change" | "thinking_level_change" | "custom" | "label" | "session_info" => {
                vec![HistoryContent::Opaque(
                    self.json(value, &format!("{selector}/{id}")),
                )]
            }
            _ => {
                self.issue(
                    Some(owner),
                    &format!("{slot}/unknown"),
                    DiagnosticCode::UnsupportedKind,
                    &format!("Unknown Pi native entry {kind:?} is retained"),
                );
                vec![HistoryContent::Opaque(
                    self.json(value, &format!("{selector}/{id}")),
                )]
            }
        };
        for operation in self.state.current_pending.values() {
            if matches!(operation.address(),ObjectAddress::Native{namespace,key,..} if namespace=="pi.tool-call"&&key.first().is_some_and(|entry|entry==id))
            {
                content.push(HistoryContent::Operation(operation.clone()));
            }
        }
        let entry = EntryRef::native(owner.clone(), "pi.entry", vec![id.into()]).unwrap();
        let predecessor = vendor_field(value, "parentId", "parent_id")
            .and_then(Value::as_str)
            .map(|parent| {
                EntryRef::native(owner.clone(), "pi.entry", vec![parent.into()]).unwrap()
            });
        let history = HistoryEntry {
            entry_ref: entry.clone(),
            containing_agent: owner.clone(),
            content,
            predecessor: predecessor.clone(),
            original_owner: original,
        };
        self.local_history.insert(entry.clone(), history.clone());
        self.object(
            owner,
            &format!("{slot}/history"),
            TraceObject::HistoryEntry(history),
        );
        if let Some(predecessor) = predecessor {
            self.add(
                owner,
                &format!("{slot}/predecessor"),
                Fact::Relation {
                    relation: Relation::HistoryPredecessor {
                        predecessor,
                        entry: entry.clone(),
                    },
                },
            );
        }
        self.state.last_native_entry = Some(entry.clone());
        for (call, operation) in &self.state.current_pending {
            self.output.correlations.insert(
                correlation_key(owner, "pi.tool-at-entry", &[id.into(), call.clone()]),
                AnyObjectRef::Operation(operation.clone()),
            );
        }
        self.output.correlations.insert(
            correlation_key(owner, "pi.entry", &[id.into()]),
            AnyObjectRef::HistoryEntry(entry.clone()),
        );
        if let SourceLocator::Jsonl { record_ordinal, .. } =
            self.input.capture.source_position.locator
        {
            self.add(
                owner,
                &format!("{slot}/position"),
                Fact::HistoryPosition {
                    entry,
                    position: self.input.capture.source_position.clone(),
                    coordinate: HistoryCoordinate::LocalRecordIndex,
                    value: record_ordinal,
                    mapping_rule: "pi/native-history".into(),
                },
            );
            self.complete_record(owner);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        owner: &AgentRef,
        reference: MessageRef,
        production: &OwnerRef,
        value: &Value,
        selector: &str,
        native_entry: Option<&str>,
        form: RevisionForm,
        activation: Option<ActivationRef>,
    ) -> MessageRevisionRef {
        let role = text(value, "role").unwrap_or("");
        let operation = text(value, "toolCallId").and_then(|id| self.tool(owner, id));
        let author = match role {
            "assistant" => Author::Agent {
                agent: production.clone(),
            },
            "toolResult" => Author::Tool { operation: None },
            "custom" => Author::Harness {
                identity: text(value, "customType").map(str::to_owned),
            },
            _ => Author::Unknown,
        };
        let protocol_role = match role {
            "assistant" => Some(ProtocolRole::Assistant),
            "user" => Some(ProtocolRole::User),
            "toolResult" => Some(ProtocolRole::Tool),
            _ => None,
        };
        let purpose = if role == "custom" {
            MessagePurpose::Context
        } else {
            MessagePurpose::Unclassified
        };
        self.object(
            owner,
            &format!("{selector}/message"),
            TraceObject::Message(Message {
                message_ref: reference.clone(),
                author: author.clone(),
                protocol_role,
                purpose,
                production: if role == "assistant" {
                    Some(ProductionScope {
                        owner: production.clone(),
                        activation: activation.clone(),
                    })
                } else {
                    None
                },
            }),
        );
        let revision = MessageRevisionRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec![selector.into(), "revision".into()],
        )
        .unwrap();
        let values = if value.get("content").is_none() {
            self.issue(
                Some(owner),
                &format!("{selector}/content-missing"),
                DiagnosticCode::DecodeFailure,
                "Message content is absent, not an observed empty body",
            );
            vec![value.clone()]
        } else {
            content_parts(value.get("content"))
        };
        let mut parts = vec![];
        for (index, part) in values.iter().enumerate() {
            let part_slot = format!("{selector}/content/{index}");
            let (kind, payload) = match text(part, "type") {
                Some("text") if text(part, "text").is_some() => (
                    ContentKind::Text,
                    self.payload(
                        text(part, "text").unwrap_or("").as_bytes().to_vec(),
                        &format!("{part_slot}/text"),
                    ),
                ),
                Some("thinking") if text(part, "thinking").is_some() => (
                    ContentKind::DisclosedReasoning,
                    self.payload(
                        text(part, "thinking").unwrap_or("").as_bytes().to_vec(),
                        &format!("{part_slot}/thinking"),
                    ),
                ),
                Some("text" | "thinking") => {
                    self.issue(
                        Some(owner),
                        &format!("{part_slot}/missing-text"),
                        DiagnosticCode::DecodeFailure,
                        "Disclosed text part is missing its content field",
                    );
                    (ContentKind::Opaque, self.json(part, &part_slot))
                }
                Some("image") => {
                    let data = text(part, "data");
                    let mime = text(part, "mimeType");
                    match (data, mime) {
                        (Some(data), Some(mime)) => match STANDARD.decode(data) {
                            Ok(bytes) => {
                                let body = derived_payload_with_media(
                                    self.input,
                                    bytes,
                                    &part_slot,
                                    "pi/base64-image",
                                    mime,
                                    PayloadEncoding::Binary,
                                );
                                let reference = body.reference.clone();
                                self.output.payloads.push(body);
                                (ContentKind::Image, reference)
                            }
                            Err(_) => {
                                self.issue(
                                    Some(owner),
                                    &format!("{part_slot}/decode"),
                                    DiagnosticCode::DecodeFailure,
                                    "Image data is invalid base64",
                                );
                                (ContentKind::Opaque, self.json(part, &part_slot))
                            }
                        },
                        _ => {
                            self.issue(
                                Some(owner),
                                &format!("{part_slot}/descriptor"),
                                DiagnosticCode::DecodeFailure,
                                "Image lacks its MIME type or encoded bytes",
                            );
                            (ContentKind::Opaque, self.json(part, &part_slot))
                        }
                    }
                }
                Some("toolCall") => {
                    if let (Some(id), Some(name)) = (text(part, "id"), text(part, "name")) {
                        if native_entry.is_some() || matches!(form, RevisionForm::Final { .. }) {
                            let operation = if let Some(entry) = native_entry {
                                OperationRef::native(
                                    owner.clone(),
                                    "pi.tool-call",
                                    vec![entry.into(), index.to_string(), id.into()],
                                )
                                .unwrap()
                            } else {
                                OperationRef::occurrence(
                                    owner.clone(),
                                    self.input.capture.source_position.clone(),
                                    vec!["tool-call".into(), id.into(), index.to_string()],
                                )
                                .unwrap()
                            };
                            let input =
                                self.json(&part["arguments"], &format!("{part_slot}/arguments"));
                            self.object(
                                owner,
                                &format!("{part_slot}/operation"),
                                TraceObject::Operation(Operation {
                                    operation_ref: operation.clone(),
                                    owner: production.clone(),
                                    activation: activation.clone(),
                                    requester: Some(RequesterRef::Agent(owner.clone())),
                                    category: OperationCategory::Tool {
                                        namespace: Some("pi".into()),
                                        name: name.into(),
                                    },
                                    input_revision: Some(input),
                                    argument_state: ArgumentState::Sealed,
                                }),
                            );
                            self.state.current_pending.insert(id.into(), operation);
                        }
                    } else {
                        self.issue(
                            Some(owner),
                            &format!("{part_slot}/malformed"),
                            DiagnosticCode::DecodeFailure,
                            "Tool-call part lacks its ID or name",
                        );
                    }
                    (ContentKind::Structured, self.json(part, &part_slot))
                }
                _ => {
                    self.issue(
                        Some(owner),
                        &format!("{part_slot}/unknown"),
                        DiagnosticCode::UnsupportedKind,
                        "Unknown Pi content part is retained as opaque JSON",
                    );
                    (ContentKind::Opaque, self.json(part, &part_slot))
                }
            };
            let part_ref = if let Some(entry) = native_entry {
                MessagePartRef::native(
                    owner.clone(),
                    "pi.entry.part",
                    vec![entry.into(), index.to_string()],
                )
                .unwrap()
            } else {
                MessagePartRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec![selector.into(), index.to_string()],
                )
                .unwrap()
            };
            let descriptor = self
                .output
                .payloads
                .iter()
                .find(|body| body.reference == payload)
                .expect("part payload was just materialized");
            parts.push(ContentPart {
                part_ref,
                kind,
                payload,
                media_type: Some(descriptor.media_type.clone()),
                encoding: descriptor.encoding,
            });
        }
        self.object(
            owner,
            &format!("{selector}/revision"),
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: reference,
                representation: Representation::Conversation,
                form,
                parts,
            }),
        );
        if role == "toolResult" {
            if let Some(operation) = operation {
                for field in ["persistedOutputPath","outputFile"] {if let Some(path)=text(&value["details"],field) {let key=super::attachment_key(self.input.canonical_observation,selector,path);self.output.correlations.insert(key.clone(),AnyObjectRef::Operation(operation.clone()));self.output.correlation_evidence.insert(key,vec![self.input.canonical_observation.clone()]);}}
                let payload = self.json(&value["content"], &format!("{selector}/content"));
                self.result(
                    owner,
                    operation.clone(),
                    payload,
                    Representation::CallerResult,
                    ResultDisposition::FinalReturn,
                    selector,
                );
                if let Some(details) = value.get("details") {
                    let payload = self.json(details, &format!("{selector}/details"));
                    self.result(
                        owner,
                        operation,
                        payload,
                        Representation::CallerResult,
                        ResultDisposition::FinalReturn,
                        &format!("{selector}/details"),
                    );
                }
            } else if let (Some(id), Some(parent)) =
                (text(value, "toolCallId"), self.state.current_parent.clone())
            {
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: owner.clone(),
                    namespace: "pi.tool-at-entry".into(),
                    key: vec![parent, id.into()],
                });
            }
            if let Some(id) = text(value, "toolCallId") {
                self.state.current_pending.remove(id);
            }
        }
        if protocol_role == Some(ProtocolRole::User) || role == "toolResult" {
            let delivery = DeliveryRef::occurrence(
                owner.clone(),
                self.input.capture.source_position.clone(),
                vec![selector.into(), "delivery".into()],
            )
            .unwrap();
            self.object(
                owner,
                &format!("{selector}/delivery"),
                TraceObject::Delivery(Delivery {
                    delivery_ref: delivery.clone(),
                    sender: author,
                    recipient: Recipient::Agent {
                        agent: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    },
                    message_revision: revision.clone(),
                    route: DeliveryRoute::NativeHistory,
                }),
            );
            self.add(
                owner,
                &format!("{selector}/admitted"),
                Fact::DeliveryStage {
                    delivery,
                    stage: DeliveryStage::Admitted {
                        entry: native_entry.map(|id| {
                            EntryRef::native(owner.clone(), "pi.entry", vec![id.into()]).unwrap()
                        }),
                    },
                },
            );
        }
        self.usage(
            owner,
            &value["usage"],
            selector,
            AnyObjectRef::MessageRevision(revision.clone()),
        );
        revision
    }

    fn tool(&mut self, owner: &AgentRef, id: &str) -> Option<OperationRef> {
        if let Some(operation) = self.state.current_pending.get(id) {
            return Some(operation.clone());
        }
        let mut next = self
            .state
            .current_parent
            .as_ref()
            .map(|id| EntryRef::native(owner.clone(), "pi.entry", vec![id.clone()]).unwrap());
        let mut visited = std::collections::BTreeSet::new();
        while let Some(reference) = next {
            if !visited.insert(reference.clone()) {
                self.issue(
                    Some(owner),
                    "pi/tool-history-cycle",
                    DiagnosticCode::CyclicHistory,
                    "Tool result predecessor chain contains a cycle",
                );
                return None;
            }
            let Some(entry) = self
                .local_history
                .get(&reference)
                .or_else(|| self.input.archive.semantic.history_entries.get(&reference))
            else {
                self.output.requests.push(ArchiveRequest::Object {
                    object_ref: AnyObjectRef::HistoryEntry(reference),
                });
                return None;
            };
            for content in &entry.content {
                if let HistoryContent::Operation(operation) = content {
                    if matches!(operation.address(),ObjectAddress::Native{namespace,key,..} if namespace=="pi.tool-call"&&key.last().is_some_and(|native|native==id))
                    {
                        return Some(operation.clone());
                    }
                }
            }
            next = entry.predecessor.clone();
        }
        None
    }

    fn preserved_membership(
        &mut self,
        owner: &AgentRef,
        checkpoint: &CheckpointRef,
        first: &str,
        parent: &str,
    ) {
        let first = EntryRef::native(owner.clone(), "pi.entry", vec![first.into()]).unwrap();
        let mut current =
            Some(EntryRef::native(owner.clone(), "pi.entry", vec![parent.into()]).unwrap());
        let mut visited = std::collections::BTreeSet::new();
        let mut entries = vec![];
        let mut proof =
            std::collections::BTreeSet::from([self.input.canonical_observation.clone()]);
        let mut reached = false;
        while let Some(reference) = current {
            if !visited.insert(reference.clone()) {
                self.issue(
                    Some(owner),
                    "pi/context-cycle",
                    DiagnosticCode::CyclicHistory,
                    "Compaction retained path contains a native predecessor cycle",
                );
                return;
            }
            let object = AnyObjectRef::HistoryEntry(reference.clone());
            let (entry, evidence) = if let Some(entry) = self.local_history.get(&reference) {
                (entry, vec![self.input.canonical_observation.clone()])
            } else {
                let Some(entry) = self.input.archive.semantic.history_entries.get(&reference) else {
                    self.output
                        .requests
                        .push(ArchiveRequest::Object { object_ref: object });
                    return;
                };
                let Some(evidence) =
                    self.input
                        .archive
                        .semantic.object_evidence
                        .get(&object)
                        .filter(|evidence| {
                            !evidence.is_empty()
                                && evidence.iter().all(|reference| {
                                    self.input.archive.observations.contains(reference)
                                })
                        })
                else {
                    self.output
                        .requests
                        .push(ArchiveRequest::Object { object_ref: object });
                    return;
                };
                (entry, evidence.clone())
            };
            proof.extend(evidence);
            entries.push(reference.clone());
            if reference == first {
                reached = true;
                break;
            }
            current = entry.predecessor.clone();
        }
        if !reached {
            self.issue(
                Some(owner),
                "pi/context-membership-gap",
                DiagnosticCode::CaptureGap,
                "The retained predecessor evidence does not reach the declared first-kept entry",
            );
            return;
        }
        entries.reverse();
        let proof: Vec<_> = proof.into_iter().collect();
        let signature = blake3::hash(
            &serde_json::to_vec(&(&entries, &proof)).expect("typed membership proof serializes"),
        )
        .to_hex()
        .to_string();
        let selection = SelectionRef::native(
            owner.clone(),
            "pi.compaction-preserved",
            vec![checkpoint.encode()],
        )
        .unwrap();
        let first_claim = self.output.claims.len();
        self.object(
            owner,
            &format!("pi/context-membership/{signature}/selection"),
            TraceObject::HistorySelection(HistorySelection {
                selection_ref: selection.clone(),
                agent: owner.clone(),
                membership: SelectionMembership::EntrySet { entries },
                excluded_heads: vec![],
            }),
        );
        self.add(
            owner,
            &format!("pi/context-membership/{signature}/relation"),
            Fact::Relation {
                relation: Relation::ContextMembership {
                    checkpoint: checkpoint.clone(),
                    selection,
                    disposition: ContextMembershipKind::Preserved,
                },
            },
        );
        for claim in &mut self.output.claims[first_claim..] {
            claim.evidence = proof.clone();
        }
    }
    fn bash(
        &mut self,
        owner: &AgentRef,
        production: &OwnerRef,
        id: &str,
        value: &Value,
        slot: &str,
    ) -> Vec<HistoryContent> {
        let reference =
            OperationRef::native(owner.clone(), "pi.bash-entry", vec![id.into()]).unwrap();
        let input = text(value, "command")
            .map(|text| self.payload(text.as_bytes().to_vec(), &format!("{slot}/command")));
        self.object(
            owner,
            &format!("{slot}/operation"),
            TraceObject::Operation(Operation {
                operation_ref: reference.clone(),
                owner: production.clone(),
                activation: None,
                requester: None,
                category: OperationCategory::TerminalIo {
                    action: TerminalAction::Launch,
                },
                input_revision: input,
                argument_state: ArgumentState::Sealed,
            }),
        );
        if let Some(output) = text(value, "output") {
            let payload = self.payload(output.as_bytes().to_vec(), &format!("{slot}/output"));
            self.result(
                owner,
                reference.clone(),
                payload,
                Representation::Runtime,
                ResultDisposition::FinalReturn,
                slot,
            );
        }
        let metadata = self.json(value, slot);
        vec![
            HistoryContent::Operation(reference),
            HistoryContent::Opaque(metadata),
        ]
    }
    fn result(
        &mut self,
        owner: &AgentRef,
        operation: OperationRef,
        payload: PayloadRef,
        representation: Representation,
        disposition: ResultDisposition,
        slot: &str,
    ) {
        self.object(
            owner,
            &format!("{slot}/result"),
            TraceObject::Result(ResultOccurrence {
                result_ref: ResultRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec![slot.into(), "result".into()],
                )
                .unwrap(),
                operation,
                recipient: None,
                disposition,
                representation,
                payload: Some(payload),
                delivery: None,
            }),
        );
    }

    fn live(&mut self, value: &Value) {
        let kind = text(value, "type").unwrap_or("");
        if kind == "response"
            && text(value, "command") == Some("get_state")
            && value["success"] == true
        {
            if let Some(owner) = text(&value["data"], "sessionId").and_then(|id| self.identify(id))
            {
                self.output.state.governing_agent = Some(owner.clone());
                self.state.header_evidence = Some(self.input.canonical_observation.clone());
                self.declare_agent(
                    &owner,
                    text(&value["data"]["model"], "id").map(str::to_owned),
                );
            } else {
                self.issue(
                    None,
                    "pi/state-identity",
                    DiagnosticCode::MissingOwner,
                    "Successful state response does not name its session",
                );
            }
            return;
        }
        let Some(owner) = self.owner() else {
            self.issue(
                None,
                "pi/live-owner",
                DiagnosticCode::MissingOwner,
                "Pi live capture requires a retained state response or source inventory binding",
            );
            return;
        };
        match kind {
            "response" => self.response(&owner, value),
            "agent_start" => {
                if self.state.outer.is_none() {
                    let reference = ActivationRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec!["outer-run".into()],
                    )
                    .unwrap();
                    self.object(
                        &owner,
                        "pi/outer-run",
                        TraceObject::Activation(Activation {
                            activation_ref: reference.clone(),
                            owner: OwnerRef::Agent {
                                agent: owner.clone(),
                            },
                        }),
                    );
                    self.add(
                        &owner,
                        "pi/outer-start",
                        Fact::Execution {
                            execution: ExecutionRef::Activation(reference.clone()),
                            boundary: ExecutionBoundary::Started { time: None },
                        },
                    );
                    self.state.current_pending.clear();
                    self.state.outer = Some(OuterRun {
                        activation: reference.clone(),
                        start: self.input.canonical_observation.clone(),
                        terminal: None,
                    });
                    self.output.state.current_activation = Some(reference);
                }
            }
            "agent_settled" => {
                let Some(run) = self.state.outer.take() else {
                    self.issue(
                        Some(&owner),
                        "pi/unobserved-run",
                        DiagnosticCode::CaptureGap,
                        "Settlement was observed without its outer run start",
                    );
                    return;
                };
                let (outcome, reason, evidence) =
                    run.terminal
                        .unwrap_or((TerminalOutcome::Unknown, None, run.start.clone()));
                self.add(
                    &owner,
                    "pi/outer-terminated",
                    Fact::Execution {
                        execution: ExecutionRef::Activation(run.activation.clone()),
                        boundary: ExecutionBoundary::Terminated {
                            time: None,
                            reason: reason.clone(),
                        },
                    },
                );
                self.add(
                    &owner,
                    "pi/outer-outcome",
                    Fact::Execution {
                        execution: ExecutionRef::Activation(run.activation),
                        boundary: ExecutionBoundary::Outcome {
                            outcome,
                            reason,
                            detail: None,
                        },
                    },
                );
                let claim = self.output.claims.last_mut().unwrap();
                for evidence in [run.start, evidence] {
                    if !claim.evidence.contains(&evidence) {
                        claim.evidence.push(evidence);
                    }
                }
                self.output.state.current_activation = None;
                self.state.current_pending.clear();
            }
            "agent_end"
            | "turn_start"
            | "turn_end"
            | "auto_retry_start"
            | "auto_retry_end"
            | "auto_compaction_start"
            | "auto_compaction_end" => {}
            "message_start" => {
                let reference = MessageRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["live-message".into()],
                )
                .unwrap();
                self.state.message = Some(LiveMessage {
                    reference,
                    start: self.input.capture.source_position.clone(),
                    previous_revision: None,
                });
                if text(&value["message"], "role") == Some("assistant") {
                    if let Some(run) = &mut self.state.outer {
                        run.terminal = None;
                    }
                }
            }
            "message_update" | "message_end" => {
                let Some(mut current) = self.state.message.clone() else {
                    self.issue(
                        Some(&owner),
                        "pi/message-start-missing",
                        DiagnosticCode::CaptureGap,
                        "Live message update has no observed message start",
                    );
                    return;
                };
                // `message` is a complete partial snapshot. Only its concrete delta
                // fields may be appended; replacing the snapshot preserves extensions.
                let message = &value["message"];
                let form = if kind == "message_end" {
                    RevisionForm::Final {
                        covers: Some(SourcePrefix {
                            through: self.input.capture.source_position.clone(),
                        }),
                    }
                } else {
                    RevisionForm::Replacement {
                        covers: Some(SourcePrefix {
                            through: self.input.capture.source_position.clone(),
                        }),
                    }
                };
                let revision = self.message(
                    &owner,
                    current.reference.clone(),
                    &OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                    message,
                    "live/message",
                    None,
                    form,
                    self.state.outer.as_ref().map(|run| run.activation.clone()),
                );
                current.previous_revision = Some(revision.clone());
                if kind == "message_end" {
                    if text(message, "role") == Some("assistant") {
                        if let Some(run) = &mut self.state.outer {
                            run.terminal = terminal(message).map(|(outcome, reason)| {
                                (outcome, reason, self.input.canonical_observation.clone())
                            });
                        }
                    }
                    let entry = EntryRef::occurrence(
                        owner.clone(),
                        current.start,
                        vec!["live-history".into()],
                    )
                    .unwrap();
                    self.object(
                        &owner,
                        "pi/live-history",
                        TraceObject::HistoryEntry(HistoryEntry {
                            entry_ref: entry,
                            containing_agent: owner.clone(),
                            content: vec![HistoryContent::Message(revision)],
                            predecessor: None,
                            original_owner: None,
                        }),
                    );
                    self.state.message = None;
                    self.issue(Some(&owner),"pi/live-native-unjoined",DiagnosticCode::CaptureGap,"Live message_end precedes native entry ID allocation; no live/native identity certificate was captured");
                } else {
                    self.state.message = Some(current);
                }
            }
            "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
                self.runtime_tool(&owner, value, kind)
            }
            "extension_ui_request" => {
                let payload = self.json(value, "/extension_ui_request");
                let operation = OperationRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["extension-ui".into()],
                )
                .unwrap();
                self.object(
                    &owner,
                    "pi/extension-request",
                    TraceObject::Operation(Operation {
                        operation_ref: operation,
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: self.state.outer.as_ref().map(|run| run.activation.clone()),
                        requester: None,
                        category: OperationCategory::Extension {
                            namespace: "pi.extension-ui".into(),
                            name: text(value, "method").unwrap_or("unknown").into(),
                        },
                        input_revision: Some(payload),
                        argument_state: ArgumentState::Sealed,
                    }),
                );
            }
            _ => self.issue(
                Some(&owner),
                "pi/live-unsupported",
                DiagnosticCode::UnsupportedKind,
                &format!("Unknown Pi runtime event {kind:?} is retained"),
            ),
        }
    }

    fn response(&mut self, owner: &AgentRef, value: &Value) {
        let command = text(value, "command").unwrap_or("");
        if value["success"] != true {
            self.issue(
                Some(owner),
                "pi/rpc-rejected",
                DiagnosticCode::DecodeFailure,
                "RPC command was rejected; the exact response remains retained",
            );
            return;
        }
        if matches!(command, "get_entries" | "get_tree") {
            if let Some(entries) = value["data"]["entries"].as_array() {
                for entry in entries {
                    self.entry(owner, entry, "snapshot");
                }
            }
            if let Some(tree) = value["data"]["tree"].as_array() {
                let mut pending: Vec<_> = tree.iter().rev().collect();
                while let Some(node) = pending.pop() {
                    if let Some(entry) = node.get("entry") {
                        self.entry(owner, entry, "snapshot");
                    }
                    if let Some(children) = node["children"].as_array() {
                        pending.extend(children.iter().rev());
                    }
                }
            }
            if let Some(leaf) = value["data"].get("leafId") {
                let membership = if let Some(id) = leaf.as_str() {
                    SelectionMembership::EntryPath {
                        head: EntryRef::native(owner.clone(), "pi.entry", vec![id.into()]).unwrap(),
                    }
                } else if leaf.is_null() {
                    SelectionMembership::EntrySet { entries: vec![] }
                } else {
                    self.issue(
                        Some(owner),
                        "pi/leaf-shape",
                        DiagnosticCode::DecodeFailure,
                        "Selected leaf ID is neither a native ID nor null",
                    );
                    return;
                };
                let selection = SelectionRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["selected-leaf".into()],
                )
                .unwrap();
                self.object(
                    owner,
                    "pi/history-selection",
                    TraceObject::HistorySelection(HistorySelection {
                        selection_ref: selection.clone(),
                        agent: owner.clone(),
                        membership,
                        excluded_heads: vec![],
                    }),
                );
                self.add(
                    owner,
                    "pi/select-history",
                    Fact::SelectHistory {
                        agent: owner.clone(),
                        selection,
                        selected_at: self.input.capture.source_position.clone(),
                    },
                );
            } else {
                self.issue(
                    Some(owner),
                    "pi/leaf-missing",
                    DiagnosticCode::UnknownSelection,
                    "Entry snapshot does not report its selected leaf",
                );
            }
        } else if matches!(command, "prompt" | "steer" | "follow_up") {
            self.issue(Some(owner),"pi/queue-not-consumption",DiagnosticCode::CaptureGap,"RPC acceptance does not establish which immutable input revision a later native message consumed");
        }
    }

    fn runtime_tool(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        let Some(id) = text(value, "toolCallId") else {
            self.issue(
                Some(owner),
                "pi/tool-id",
                DiagnosticCode::MissingOwner,
                "Runtime tool event lacks its toolCallId",
            );
            return;
        };
        let reference = OperationRef::native(
            owner.clone(),
            "pi.runtime-tool",
            vec![
                self.input.capture.observation_ref.producer_epoch.clone(),
                id.into(),
            ],
        )
        .unwrap();
        if kind == "tool_execution_start" {
            let Some(name) = text(value, "toolName") else {
                self.issue(
                    Some(owner),
                    "pi/tool-name",
                    DiagnosticCode::DecodeFailure,
                    "Runtime tool start lacks its toolName",
                );
                return;
            };
            let payload = self.json(&value["args"], "/args");
            self.object(
                owner,
                "pi/runtime-tool",
                TraceObject::Operation(Operation {
                    operation_ref: reference.clone(),
                    owner: OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                    activation: self.state.outer.as_ref().map(|run| run.activation.clone()),
                    requester: Some(RequesterRef::Agent(owner.clone())),
                    category: OperationCategory::Tool {
                        namespace: Some("pi".into()),
                        name: name.into(),
                    },
                    input_revision: Some(payload),
                    argument_state: ArgumentState::Sealed,
                }),
            );
            self.add(
                owner,
                "pi/runtime-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(reference),
                    boundary: ExecutionBoundary::Started { time: None },
                },
            );
        } else {
            let payload = self.json(
                &value[if kind == "tool_execution_update" {
                    "partialResult"
                } else {
                    "result"
                }],
                "/result",
            );
            self.result(
                owner,
                reference.clone(),
                payload,
                Representation::Runtime,
                if kind == "tool_execution_update" {
                    ResultDisposition::Progress
                } else {
                    ResultDisposition::FinalReturn
                },
                "pi/runtime",
            );
            if kind == "tool_execution_end" {
                let outcome = match value.get("isError").and_then(Value::as_bool) {
                    Some(true) => TerminalOutcome::Failed,
                    Some(false) => TerminalOutcome::Succeeded,
                    None => TerminalOutcome::Unknown,
                };
                self.add(
                    owner,
                    "pi/runtime-outcome",
                    Fact::Execution {
                        execution: ExecutionRef::Operation(reference),
                        boundary: ExecutionBoundary::Outcome {
                            outcome,
                            reason: None,
                            detail: None,
                        },
                    },
                );
            }
        }
    }

    fn usage(&mut self, owner: &AgentRef, value: &Value, selector: &str, subject: AnyObjectRef) {
        for (field, metric) in [
            ("input", Metric::InputTokens),
            ("output", Metric::OutputTokens),
            ("cacheRead", Metric::CachedTokens),
            (
                "cacheWrite",
                Metric::Extension {
                    namespace: "pi.usage".into(),
                    name: "cache_write_tokens".into(),
                },
            ),
            ("totalTokens", Metric::TotalTokens),
        ] {
            if let Some(number) = value.get(field).and_then(Value::as_number) {
                self.object(
                    owner,
                    &format!("{selector}/usage/{field}"),
                    TraceObject::Measurement(Measurement {
                        measurement_ref: MeasurementRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec![selector.into(), "usage".into(), field.into()],
                        )
                        .unwrap(),
                        subject: subject.clone(),
                        metric,
                        unit: MeasurementUnit::Tokens,
                        value: number.clone(),
                        mode: MeasurementMode::Gauge,
                        aggregation_scope: AggregationScope::NamedSubject,
                        components: vec![],
                    }),
                );
            }
        }
        // The vendor reports a total after its own pricing calculation. Its raw
        // component amounts remain retained; they are not additional total costs.
        if let Some(number) = value["cost"].get("total").and_then(Value::as_number) {
            self.object(
                owner,
                &format!("{selector}/cost/total"),
                TraceObject::Measurement(Measurement {
                    measurement_ref: MeasurementRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec![selector.into(), "cost".into(), "total".into()],
                    )
                    .unwrap(),
                    subject,
                    metric: Metric::Cost {
                        pricing_basis: "pi/vendor-reported-usage".into(),
                    },
                    unit: MeasurementUnit::Currency { code: "USD".into() },
                    value: number.clone(),
                    mode: MeasurementMode::Gauge,
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![],
                }),
            );
        }
    }
    fn complete_record(&mut self, owner: &AgentRef) {
        if self.output.claims.iter().any(|claim| {
            matches!(
                claim.fact,
                Fact::Diagnostic {
                    diagnostic: TraceDiagnostic {
                        code: DiagnosticCode::UnsupportedKind | DiagnosticCode::DecodeFailure,
                        ..
                    }
                }
            )
        }) {
            return;
        }
        if let SourceLocator::Jsonl { record_ordinal, .. } =
            self.input.capture.source_position.locator
        {
            if let Some(end) = record_ordinal.checked_add(1) {
                self.add(
                    owner,
                    "pi/mapped-record",
                    Fact::HistoryMappingComplete {
                        extent: SourceExtent {
                            source: self.input.capture.source_position.source.clone(),
                            generation: self.input.capture.source_position.generation,
                            coordinate: HistoryCoordinate::LocalRecordIndex,
                            start_inclusive: record_ordinal,
                            end_exclusive: end,
                            mapping_rule: "pi/native-history".into(),
                            original_owner: None,
                        },
                    },
                );
            }
        }
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn vendor_field<'a>(value: &'a Value, current: &str, historical: &str) -> Option<&'a Value> {
    value.get(current).or_else(|| value.get(historical))
}
fn content_parts(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(parts)) => parts.clone(),
        Some(Value::String(text)) => vec![serde_json::json!({"type":"text","text":text})],
        Some(value) => vec![value.clone()],
        None => vec![],
    }
}
fn terminal(message: &Value) -> Option<(TerminalOutcome, Option<ExecutionCloseReason>)> {
    match vendor_field(message, "stopReason", "stop_reason").and_then(Value::as_str) {
        Some("stop") => Some((
            TerminalOutcome::Succeeded,
            Some(ExecutionCloseReason::VendorStop),
        )),
        Some("length") => Some((
            TerminalOutcome::Interrupted,
            Some(ExecutionCloseReason::BudgetExhausted),
        )),
        Some("aborted") => Some((
            TerminalOutcome::Interrupted,
            Some(ExecutionCloseReason::VendorStop),
        )),
        Some("error") => Some((
            TerminalOutcome::Failed,
            Some(ExecutionCloseReason::VendorStop),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
