//! Claude record UUIDs, explicit child identities and independently observed loop outcomes.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sylph_protocol::{trace::*, AgentName, NativeId, SessionId};

use super::{
    claim, correlation_key, derived_payload_with_media, ArchiveRequest, MapInput, MapOutput,
    MappingDiagnostic, SemanticMapper,
};

pub struct ClaudeMapper;

mod workflow;

#[derive(Clone, Default, Serialize, Deserialize)]
struct ClaudeState {
    streams: BTreeMap<String, LiveStream>,
    runs: BTreeMap<String, (ActivationRef, ObservationRef)>,
    delegation_starts: BTreeMap<String, bool>,
}

#[derive(Clone, Serialize, Deserialize)]
struct LiveStream {
    message: MessageRef,
    start: SourcePosition,
    previous: Option<MessageRevisionRef>,
    parts: BTreeMap<u64, Value>,
    input_json: BTreeMap<u64, String>,
    previous_entry: Option<EntryRef>,
    evidence: Vec<ObservationRef>,
}

impl SemanticMapper for ClaudeMapper {
    fn build(&self) -> &'static str {
        "claude-3"
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
            .get("claude")
            .map(|value| serde_json::from_value::<ClaudeState>(value.clone()))
            .transpose()
        {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::DecodeFailure,
                    message: format!("Invalid Claude mapper checkpoint: {error}"),
                });
                return output;
            }
        };
        let mut context = Context {
            input: &input,
            output: &mut output,
            state: &mut state,
        };
        if input.capture.transport_kind == TransportKind::NativeSnapshot {
            context.sidecar(&value);
        } else if input.capture.transport_kind == TransportKind::NativeJsonl {
            context.native(&value);
        } else {
            context.live(&value);
        }
        output.state.values.insert(
            "claude".into(),
            serde_json::to_value(state).expect("typed Claude state serializes"),
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
    state: &'a mut ClaudeState,
}

impl Context<'_, '_> {
    fn add(&mut self, owner: &AgentRef, slot: &str, fact: Fact) {
        self.output
            .claims
            .push(claim(self.input, owner, slot, ClaudeMapper.build(), fact));
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
    fn payload(
        &mut self,
        bytes: Vec<u8>,
        selector: &str,
        media: &str,
        encoding: PayloadEncoding,
    ) -> PayloadRef {
        let payload = derived_payload_with_media(
            self.input,
            bytes,
            selector,
            "claude/json-field",
            media,
            encoding,
        );
        let reference = payload.reference.clone();
        self.output.payloads.push(payload);
        reference
    }
    fn json(&mut self, value: &Value, selector: &str) -> PayloadRef {
        self.payload(
            serde_json::to_vec(value).expect("JSON serializes"),
            selector,
            "application/json",
            PayloadEncoding::Utf8,
        )
    }
    fn root(&self, id: &str) -> Option<AgentRef> {
        if id.is_empty() {
            return None;
        }
        let Issuer::Environment(environment) = &self.input.capture.observation_ref.issuer else {
            return None;
        };
        Some(AgentRef::Session(SessionId::new(
            environment.clone(),
            AgentName::parse("claude").unwrap(),
            NativeId::new(id),
        )))
    }
    fn child(root: &AgentRef, id: &str) -> AgentRef {
        AgentRef::Embedded {
            anchor_session: root.anchor_session().clone(),
            namespace: "claude.agent".into(),
            native_child_id: id.into(),
        }
    }
    fn declare_agent(&mut self, owner: &AgentRef, name: Option<String>, model: Option<String>) {
        self.object(
            owner,
            &format!("claude/agent/{}", owner.encode()),
            TraceObject::Agent(AgentThread {
                agent_ref: owner.clone(),
                vendor: AgentName::parse("claude").unwrap(),
                name,
                model,
            }),
        );
    }
    fn request(&mut self, owner: &AgentRef, namespace: &str, key: Vec<String>) {
        self.output.requests.push(ArchiveRequest::Correlation {
            agent: owner.clone(),
            namespace: namespace.into(),
            key,
        });
    }

    fn native(&mut self, value: &Value) {
        if value.get("sessionId").is_none() && value.get("key").is_some() {
            self.workflow_journal(value);
            return;
        }
        let Some(root) = text(value, "sessionId").and_then(|id| self.root(id)) else {
            self.issue(
                None,
                "claude/native-owner",
                DiagnosticCode::MissingOwner,
                "Native Claude record lacks its sessionId",
            );
            return;
        };
        let owner = text(value, "agentId")
            .map(|id| Self::child(&root, id))
            .unwrap_or(root.clone());
        if matches!(
            self.input.capture.source_position.locator,
            SourceLocator::Jsonl {
                record_ordinal: 0,
                ..
            }
        ) {
            self.output.source_owners.push(super::SourceOwnership {
                source: self.input.capture.source_position.source.clone(),
                generation: self.input.capture.source_position.generation,
                owner: super::SourceOwner {
                    agent: owner.clone(),
                    evidence: vec![self.input.canonical_observation.clone()],
                    rule: "claude/initial-native-record-owner".into(),
                },
            });
        }
        self.output.state.governing_agent = Some(root.clone());
        self.declare_agent(&owner, None, None);
        let kind = text(value, "type").unwrap_or("");
        match kind {
            "assistant" | "user" => self.record(&owner, value, false),
            "attachment" => self.context_record(&owner, value, "attachment"),
            "system" if text(value, "subtype") == Some("compact_boundary") => {
                self.compaction(&owner, value)
            }
            "system" => self.context_record(&owner, value, "system"),
            "queue-operation"
            | "file-history-snapshot"
            | "mode"
            | "permission-mode"
            | "agent-name"
            | "custom-title"
            | "ai-title"
            | "last-prompt"
            | "atis-latch" => {}
            _ => {
                self.issue(
                    Some(&owner),
                    "claude/native-kind",
                    DiagnosticCode::UnsupportedKind,
                    &format!("Unknown Claude native record {kind:?} is retained"),
                );
                if text(value, "uuid").is_some() {
                    let body = self.json(value, "/");
                    self.history(&owner, value, vec![HistoryContent::Opaque(body)], None);
                }
            }
        }
        self.complete_record(&owner);
    }

    fn original_owner(&self, owner: &AgentRef, value: &Value) -> AgentRef {
        // A copied record explicitly names its own origin; that stamp establishes
        // ownership for this record, not one universal cutoff for the whole fork.
        text(&value["forkedFrom"], "sessionId")
            .and_then(|id| self.root(id))
            .unwrap_or(owner.clone())
    }

    fn record(&mut self, containing: &AgentRef, value: &Value, live: bool) {
        let Some(uuid) = text(value, "uuid") else {
            self.issue(
                Some(containing),
                "claude/record-id",
                DiagnosticCode::DecodeFailure,
                "Content-bearing Claude record lacks its record UUID",
            );
            return;
        };
        let owner = self.original_owner(containing, value);
        let source = &value["message"];
        let message =
            MessageRef::native(owner.clone(), "claude.record", vec![uuid.into()]).unwrap();
        let revision = MessageRevisionRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["record".into(), uuid.into()],
        )
        .unwrap();
        let values = if source.get("content").is_none() {
            self.issue(
                Some(&owner),
                "claude/content-missing",
                DiagnosticCode::DecodeFailure,
                "Message content is absent, not an observed empty body",
            );
            vec![source.clone()]
        } else {
            parts(source.get("content"))
        };
        let has_tool = values
            .iter()
            .any(|part| text(part, "type") == Some("tool_result"));
        let context = value["isMeta"] == true
            || value["isCompactSummary"] == true
            || value["isVisibleInTranscriptOnly"] == true;
        let synthetic = value["isSynthetic"] == true || value["isApiErrorMessage"] == true;
        let role = text(source, "role")
            .or_else(|| text(value, "type"))
            .unwrap_or("");
        let author = if context || synthetic {
            Author::Harness {
                identity: Some("claude".into()),
            }
        } else if has_tool {
            Author::Tool { operation: None }
        } else if role == "assistant" {
            Author::Agent {
                agent: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            }
        } else if live {
            self.live_author(containing, value)
        } else if text(value, "promptSource") == Some("typed") {
            Author::Human { subject: None }
        } else {
            Author::Unknown
        };
        let purpose = if context {
            MessagePurpose::Context
        } else if synthetic {
            MessagePurpose::Notice
        } else {
            MessagePurpose::Unclassified
        };
        let activation = if live {
            self.state
                .runs
                .get(&containing.encode())
                .map(|(activation, _)| activation.clone())
        } else {
            None
        };
        self.object(
            &owner,
            "claude/record-message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: author.clone(),
                protocol_role: match role {
                    "assistant" => Some(ProtocolRole::Assistant),
                    "user" => Some(ProtocolRole::User),
                    _ => None,
                },
                purpose,
                production: if role == "assistant" && !synthetic {
                    Some(ProductionScope {
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: activation.clone(),
                    })
                } else {
                    None
                },
            }),
        );
        let mut content = vec![];
        for (index, part) in values.iter().enumerate() {
            content.push(self.part(
                &owner,
                uuid,
                index as u64,
                part,
                &format!("/message/content/{index}"),
            ));
            if text(part, "type") == Some("tool_use") {
                self.tool_use(
                    &owner,
                    part,
                    &format!("claude/record/{uuid}/{index}"),
                    activation.clone(),
                );
            }
            if text(part, "type") == Some("tool_result") {
                for field in ["persistedOutputPath","outputFile"] {if let Some(path)=text(&value["toolUseResult"],field) {
                    if let Some(id)=text(part,"tool_use_id") {let key=super::attachment_key(self.input.canonical_observation,&format!("/message/content/{index}"),path);self.output.correlations.insert(key.clone(),AnyObjectRef::Operation(operation(&owner,id)));self.output.correlation_evidence.insert(key,vec![self.input.canonical_observation.clone()]);}
                }}
                self.tool_result(
                    &owner,
                    &root_of(containing),
                    part,
                    &value["toolUseResult"],
                    &format!("claude/record/{uuid}/{index}"),
                );
            }
        }
        self.object(
            &owner,
            "claude/record-revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: message.clone(),
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: content,
            }),
        );
        // A model-message ID groups records; it is deliberately absent from the
        // canonical record identity and cannot overwrite sibling UUIDs.
        if let Some(model_id) = text(source, "id") {
            self.output.correlations.insert(
                correlation_key(
                    &owner,
                    "claude.model-record",
                    &[model_id.into(), uuid.into()],
                ),
                AnyObjectRef::Message(message),
            );
        }
        self.history(
            containing,
            value,
            vec![HistoryContent::Message(revision.clone())],
            if owner != *containing {
                Some(OwnerRef::Agent {
                    agent: owner.clone(),
                })
            } else {
                None
            },
        );
        if role == "user" && value["isVisibleInTranscriptOnly"] != true {
            let delivery = DeliveryRef::native(
                containing.clone(),
                "claude.record-delivery",
                vec![uuid.into()],
            )
            .unwrap();
            self.object(
                containing,
                "claude/native-delivery",
                TraceObject::Delivery(Delivery {
                    delivery_ref: delivery.clone(),
                    sender: author,
                    recipient: Recipient::Agent {
                        agent: OwnerRef::Agent {
                            agent: containing.clone(),
                        },
                    },
                    route: DeliveryRoute::NativeHistory,
                }),
            );
            self.add(
                containing,
                "claude/native-delivery-revision",
                Fact::DeliveryRevisionObserved {
                    delivery: delivery.clone(),
                    revision: revision.clone(),
                },
            );
            self.add(
                containing,
                "claude/native-admission",
                Fact::DeliveryStage {
                    delivery,
                    stage: DeliveryStage::Admitted {
                        entry: Some(entry(containing, uuid)),
                    },
                },
            );
        }
        self.measurements(
            &owner,
            &source["usage"],
            &format!("claude/record/{uuid}/usage"),
            AnyObjectRef::MessageRevision(revision),
        );
    }

    fn live_author(&mut self, owner: &AgentRef, value: &Value) -> Author {
        let origin = &value["origin"];
        match text(origin, "kind") {
            Some("peer") => {
                let key = text(origin, "from")
                    .or_else(|| text(value, "from"))
                    .unwrap_or("");
                self.request(owner, "claude.peer", vec![key.into()]);
                Author::Agent {
                    agent: OwnerRef::Unresolved {
                        scope: owner.clone(),
                        namespace: "claude.peer".into(),
                        native_key: vec![key.into()],
                        reason: "Peer author requires a retained identity binding".into(),
                    },
                }
            }
            Some("channel") => Author::External {
                identity: text(origin, "channel").map(str::to_owned),
            },
            Some("task-notification" | "coordinator") => Author::Harness {
                identity: text(origin, "kind").map(str::to_owned),
            },
            None => Author::Human { subject: None },
            Some(_) => Author::Unknown,
        }
    }

    fn part(
        &mut self,
        owner: &AgentRef,
        uuid: &str,
        index: u64,
        value: &Value,
        selector: &str,
    ) -> ContentPart {
        let mut media = "application/json".to_owned();
        let mut encoding = PayloadEncoding::Utf8;
        let (kind, payload) = match text(value, "type") {
            Some("text") if text(value, "text").is_some() => {
                media = "text/plain".into();
                (
                    ContentKind::Text,
                    self.payload(
                        text(value, "text").unwrap_or("").as_bytes().to_vec(),
                        selector,
                        &media,
                        encoding,
                    ),
                )
            }
            Some("thinking") if text(value, "thinking").is_some() => {
                media = "text/plain".into();
                (
                    ContentKind::DisclosedReasoning,
                    self.payload(
                        text(value, "thinking").unwrap_or("").as_bytes().to_vec(),
                        selector,
                        &media,
                        encoding,
                    ),
                )
            }
            Some("text" | "thinking") => {
                self.issue(
                    Some(owner),
                    &format!("{selector}/missing-text"),
                    DiagnosticCode::DecodeFailure,
                    "Disclosed text part is missing its content field",
                );
                (ContentKind::Opaque, self.json(value, selector))
            }
            Some("image" | "document") if text(&value["source"], "type") == Some("base64") => {
                let source = &value["source"];
                match (text(source, "data"), text(source, "media_type")) {
                    (Some(data), Some(mime)) => match STANDARD.decode(data) {
                        Ok(bytes) => {
                            media = mime.into();
                            encoding = PayloadEncoding::Binary;
                            (
                                if text(value, "type") == Some("image") {
                                    ContentKind::Image
                                } else {
                                    ContentKind::Attachment
                                },
                                self.payload(
                                    bytes,
                                    &format!("{selector}/source/data"),
                                    &media,
                                    encoding,
                                ),
                            )
                        }
                        Err(_) => {
                            self.issue(Some(owner),&format!("{selector}/decode"),DiagnosticCode::DecodeFailure,"Base64 attachment could not be decoded; exact encoded part is retained");
                            (ContentKind::Opaque, self.json(value, selector))
                        }
                    },
                    _ => {
                        self.issue(
                            Some(owner),
                            &format!("{selector}/descriptor"),
                            DiagnosticCode::DecodeFailure,
                            "Encoded attachment lacks its bytes or media type",
                        );
                        (ContentKind::Opaque, self.json(value, selector))
                    }
                }
            }
            Some("tool_use" | "tool_result") => {
                (ContentKind::Structured, self.json(value, selector))
            }
            Some("redacted_thinking") => (ContentKind::Opaque, self.json(value, selector)),
            _ => {
                self.issue(
                    Some(owner),
                    &format!("{selector}/unknown"),
                    DiagnosticCode::UnsupportedKind,
                    "Unsupported Claude content part remains opaque",
                );
                (ContentKind::Opaque, self.json(value, selector))
            }
        };
        ContentPart {
            part_ref: MessagePartRef::native(
                owner.clone(),
                "claude.record.part",
                vec![uuid.into(), index.to_string()],
            )
            .unwrap(),
            kind,
            payload,
            media_type: Some(media),
            encoding,
        }
    }

    fn history(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        content: Vec<HistoryContent>,
        original_owner: Option<OwnerRef>,
    ) {
        let Some(uuid) = text(value, "uuid") else {
            return;
        };
        let reference = entry(owner, uuid);
        let predecessor = text(value, "parentUuid").map(|parent| entry(owner, parent));
        self.object(
            owner,
            "claude/history-entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: reference.clone(),
                containing_agent: owner.clone(),
                predecessor: predecessor.clone(),
                original_owner,
            }),
        );
        self.add(
            owner,
            "claude/history-content",
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    reference.clone(),
                    self.input.capture.source_position.clone(),
                    if self.input.capture.transport_kind == TransportKind::NativeJsonl {
                        "claude/native-history"
                    } else {
                        "claude/live-history"
                    },
                    content,
                ),
            },
        );
        if let Some(predecessor) = predecessor {
            self.add(
                owner,
                "claude/history-predecessor",
                Fact::Relation {
                    relation: Relation::HistoryPredecessor {
                        predecessor,
                        entry: reference.clone(),
                    },
                },
            );
        }
        if let SourceLocator::Jsonl { record_ordinal, .. } =
            self.input.capture.source_position.locator
        {
            self.add(
                owner,
                "claude/history-position",
                Fact::HistoryPosition {
                    entry: reference,
                    position: self.input.capture.source_position.clone(),
                    coordinate: HistoryCoordinate::LocalRecordIndex,
                    value: record_ordinal,
                    mapping_rule: "claude/native-history".into(),
                },
            );
        }
    }

    fn context_record(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        let Some(uuid) = text(value, "uuid") else {
            return;
        };
        let original = self.original_owner(owner, value);
        let payload = self.json(value, &format!("/{kind}"));
        let message =
            MessageRef::native(original.clone(), "claude.record", vec![uuid.into()]).unwrap();
        let revision = MessageRevisionRef::occurrence(
            original.clone(),
            self.input.capture.source_position.clone(),
            vec!["context".into(), uuid.into()],
        )
        .unwrap();
        self.object(
            &original,
            "claude/context-message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: Author::Harness {
                    identity: Some("claude".into()),
                },
                protocol_role: None,
                purpose: if kind == "attachment" {
                    MessagePurpose::Context
                } else {
                    MessagePurpose::Notice
                },
                production: None,
            }),
        );
        self.object(
            &original,
            "claude/context-revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::Conversation,
                form: RevisionForm::Complete,
                parts: vec![ContentPart {
                    part_ref: MessagePartRef::native(
                        original.clone(),
                        "claude.record.part",
                        vec![uuid.into(), "0".into()],
                    )
                    .unwrap(),
                    kind: ContentKind::Structured,
                    payload,
                    media_type: Some("application/json".into()),
                    encoding: PayloadEncoding::Utf8,
                }],
            }),
        );
        self.history(
            owner,
            value,
            vec![HistoryContent::Message(revision)],
            if original != *owner {
                Some(OwnerRef::Agent { agent: original })
            } else {
                None
            },
        );
    }

    fn compaction(&mut self, owner: &AgentRef, value: &Value) {
        let Some(uuid) = text(value, "uuid") else {
            self.issue(
                Some(owner),
                "claude/compact-id",
                DiagnosticCode::DecodeFailure,
                "Compaction checkpoint lacks its UUID",
            );
            return;
        };
        let reference =
            CheckpointRef::native(owner.clone(), "claude.compaction", vec![uuid.into()]).unwrap();
        let payload = self.json(value, "/");
        self.object(
            owner,
            "claude/compaction",
            TraceObject::ContextCheckpoint(ContextCheckpoint {
                checkpoint_ref: reference.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                basis: ContextBasis::Summary,
                content: ContextContent::OpaqueReplacement { payload },
                coverage: InclusionCoverage::Unknown {
                    reason: "Compaction metadata does not expose the exact next model request"
                        .into(),
                },
            }),
        );
        self.history(
            owner,
            value,
            vec![HistoryContent::Checkpoint(reference.clone())],
            None,
        );
        if let Some(ids) = value["compactMetadata"]["preservedMessages"]["allUuids"].as_array() {
            let entries: Option<Vec<_>> = ids
                .iter()
                .map(|id| {
                    id.as_str()
                        .filter(|id| !id.is_empty())
                        .map(|id| entry(owner, id))
                })
                .collect();
            let Some(entries) = entries else {
                self.issue(
                    Some(owner),
                    "claude/preserved-membership-shape",
                    DiagnosticCode::DecodeFailure,
                    "Preserved message membership contains a non-UUID value",
                );
                return;
            };
            let selection = SelectionRef::native(
                owner.clone(),
                "claude.compaction-preserved",
                vec![uuid.into()],
            )
            .unwrap();
            self.object(
                owner,
                "claude/preserved-membership",
                TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selection.clone(),
                    agent: owner.clone(),
                    membership: SelectionMembership::EntrySet { entries },
                    content_scope: HistoryContentScope::AllObservedAtCut,
                    excluded_heads: vec![],
                }),
            );
            self.add(
                owner,
                "claude/context-membership",
                Fact::Relation {
                    relation: Relation::ContextMembership {
                        checkpoint: reference,
                        selection,
                        disposition: ContextMembershipKind::Preserved,
                    },
                },
            );
        }
    }

    fn tool_use(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        slot: &str,
        activation: Option<ActivationRef>,
    ) {
        let (Some(id), Some(name)) = (text(value, "id"), text(value, "name")) else {
            self.issue(
                Some(owner),
                &format!("{slot}/id"),
                DiagnosticCode::DecodeFailure,
                "Claude tool use lacks its ID or name",
            );
            return;
        };
        let operation = operation(owner, id);
        let input = self.json(&value["input"], &format!("{slot}/input"));
        let delegation = matches!(name, "Agent" | "Task");
        self.object(
            owner,
            &format!("{slot}/operation"),
            TraceObject::Operation(Operation {
                operation_ref: operation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation,
                requester: Some(RequesterRef::Agent(owner.clone())),
                category: if delegation {
                    OperationCategory::Delegation
                } else {
                    OperationCategory::Tool {
                        namespace: Some("claude".into()),
                        name: name.into(),
                    }
                },
                input_revision: Some(input),
                argument_state: ArgumentState::Sealed,
            }),
        );
        if delegation {
            self.state
                .delegation_starts
                .insert(operation.encode(), value["input"].get("resume").is_none());
        }
        self.output.correlations.insert(
            correlation_key(owner, "claude.tool-owner", &[owner.encode(), id.into()]),
            AnyObjectRef::Operation(operation),
        );
    }

    fn tool_result(
        &mut self,
        owner: &AgentRef,
        root: &AgentRef,
        part: &Value,
        structured: &Value,
        slot: &str,
    ) {
        let Some(id) = text(part, "tool_use_id") else {
            self.issue(
                Some(owner),
                &format!("{slot}/pairing"),
                DiagnosticCode::MissingOwner,
                "Tool result is missing the content block's tool_use_id",
            );
            return;
        };
        let operation = operation(owner, id);
        let payload = self.json(part, &format!("{slot}/caller"));
        self.result(
            owner,
            &operation,
            payload,
            Representation::CallerResult,
            slot,
        );
        if !structured.is_null() {
            let payload = self.json(structured, &format!("{slot}/structured"));
            self.result(
                owner,
                &operation,
                payload,
                Representation::Runtime,
                &format!("{slot}/structured"),
            );
        }
        if text(structured, "taskType") == Some("local_workflow") {
            self.workflow_launch(owner, root, &operation, structured, slot);
        }
        if let Some(child_id) = text(structured, "agentId") {
            let child = Self::child(root, child_id);
            self.declare_agent(
                &child,
                None,
                text(structured, "resolvedModel").map(str::to_owned),
            );
            self.output.correlations.insert(
                correlation_key(root, "claude.parent-tool-use", &[id.into()]),
                AnyObjectRef::Agent(child.clone()),
            );
            if self.state.delegation_starts.get(&operation.encode()) == Some(&true) {
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
                self.output.correlations.insert(
                    correlation_key(&child, "claude.spawn-operation", &[id.into()]),
                    AnyObjectRef::Operation(operation.clone()),
                );
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
        }
        if let Some(error) = part.get("is_error").and_then(Value::as_bool) {
            self.add(
                owner,
                &format!("{slot}/outcome"),
                Fact::Execution {
                    execution: ExecutionRef::Operation(operation.clone()),
                    boundary: ExecutionBoundary::Outcome {
                        outcome: if error {
                            TerminalOutcome::Failed
                        } else {
                            TerminalOutcome::Succeeded
                        },
                        reason: None,
                        detail: None,
                    },
                },
            );
        }
        for field in ["persistedOutputPath", "outputFile"] {
            if let Some(path) = text(structured, field) {
                let attachment = self
                    .input
                    .capture
                    .attachments
                    .iter()
                    .find(|attachment| attachment.locator == path)
                    .cloned();
                match attachment {
                    Some(attachment) => {
                        if let Some(payload) = attachment.payload_ref {
                            self.result(
                                owner,
                                &operation,
                                payload,
                                Representation::Runtime,
                                &format!("{slot}/{field}"),
                            );
                        }
                        let state = match attachment.availability {
                            AttachmentAvailability::Captured => CoverageState::Complete,
                            AttachmentAvailability::Partial { reason } => {
                                CoverageState::Gap { reason }
                            }
                            AttachmentAvailability::Pending => CoverageState::Pending {
                                reason: "Output sidecar is pending acquisition".into(),
                            },
                            AttachmentAvailability::Missing => CoverageState::Unavailable {
                                reason: "Output sidecar was absent during acquisition".into(),
                            },
                            AttachmentAvailability::Unreadable { reason } => {
                                CoverageState::Unavailable { reason }
                            }
                        };
                        self.add(
                            owner,
                            &format!("{slot}/{field}/coverage"),
                            Fact::Coverage {
                                coverage: Coverage {
                                    subject: AnyObjectRef::Operation(operation.clone()),
                                    source: self.input.capture.source_position.source.clone(),
                                    dimension: CoverageDimension::PayloadRanges,
                                    interval: CoverageInterval::Object {
                                        object: AnyObjectRef::Operation(operation.clone()),
                                    },
                                    state,
                                },
                            },
                        );
                    }
                    None => self.issue(
                        Some(owner),
                        &format!("{slot}/{field}/missing"),
                        DiagnosticCode::MissingSource,
                        "A recorded output path does not prove its sidecar bytes were acquired",
                    ),
                }
            }
        }
        self.state.delegation_starts.remove(&operation.encode());
    }

    fn result(
        &mut self,
        owner: &AgentRef,
        operation: &OperationRef,
        payload: PayloadRef,
        representation: Representation,
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
                operation: operation.clone(),
                recipient: None,
                disposition: ResultDisposition::FinalReturn,
                representation,
                payload: Some(payload),
                delivery: None,
            }),
        );
    }

    fn sidecar(&mut self, value: &Value) {
        let source = &self.input.capture.source_position.source;
        let provenance = self
            .input
            .archive
            .source_owners
            .get(&(
                source.clone(),
                self.input.capture.source_position.generation,
            ))
            .filter(|owner| {
                !owner.evidence.is_empty()
                    && !owner.rule.is_empty()
                    && owner
                        .evidence
                        .iter()
                        .all(|evidence| self.input.archive.observations.contains(evidence))
            })
            .cloned();
        let Some(provenance) = provenance else {
            self.output.requests.push(ArchiveRequest::Source {
                source: source.clone(),
                generation: self.input.capture.source_position.generation,
            });
            self.issue(None,"claude/sidecar-owner",DiagnosticCode::MissingOwner,"Delegate metadata needs retained source-owner proof; admission subjects are not identity evidence");
            return;
        };
        let child = &provenance.agent;
        if value.get("runId").is_some() {
            self.workflow_manifest(value, &provenance);
            return;
        }
        if !matches!(child,AgentRef::Embedded{namespace,..}if namespace=="claude.agent") {
            self.issue(
                None,
                "claude/sidecar-kind",
                DiagnosticCode::MissingOwner,
                "The retained source descriptor does not name a Claude embedded agent",
            );
            return;
        }
        let root = root_of(child);
        self.declare_agent(child, text(value, "name").map(str::to_owned), None);
        let mut parent = text(value, "parentAgentId").map(|id| Self::child(&root, id));
        let mut invocation = None;
        if let Some(id) = text(value, "toolUseId") {
            if let Some(owner) = &parent {
                invocation = Some(operation(owner, id));
            } else {
                match self.input.archive.correlations.get(&correlation_key(
                    child,
                    "claude.spawn-operation",
                    &[id.into()],
                )) {
                    Some(AnyObjectRef::Operation(operation)) => {
                        parent = Some(operation.scope().clone());
                        invocation = Some(operation.clone());
                    }
                    _ => self.request(child, "claude.spawn-operation", vec![id.into()]),
                }
            }
            self.output.correlations.insert(
                correlation_key(&root, "claude.parent-tool-use", &[id.into()]),
                AnyObjectRef::Agent(child.clone()),
            );
        }
        if let Some(parent) = parent {
            self.add(
                child,
                "claude/sidecar-parent",
                Fact::Relation {
                    relation: Relation::Spawn {
                        parent,
                        child: child.clone(),
                        operation: invocation,
                    },
                },
            );
        }
        for claim in &mut self.output.claims {
            for evidence in &provenance.evidence {
                if !claim.evidence.contains(evidence) {
                    claim.evidence.push(evidence.clone());
                }
            }
        }
        for key in self.output.correlations.keys() {
            let mut evidence = provenance.evidence.clone();
            evidence.push(self.input.canonical_observation.clone());
            self.output
                .correlation_evidence
                .insert(key.clone(), evidence);
        }
    }

    fn live(&mut self, value: &Value) {
        let Some(root) = text(value, "session_id").and_then(|id| self.root(id)) else {
            self.issue(
                None,
                "claude/live-session",
                DiagnosticCode::MissingOwner,
                "Claude live frame has no evidenced session_id",
            );
            return;
        };
        self.output.state.governing_agent = Some(root.clone());
        let owner = if let Some(host) = text(value, "parent_tool_use_id") {
            match self.input.archive.correlations.get(&correlation_key(
                &root,
                "claude.parent-tool-use",
                &[host.into()],
            )) {
                Some(AnyObjectRef::Agent(agent)) => agent.clone(),
                _ => {
                    self.request(&root, "claude.parent-tool-use", vec![host.into()]);
                    self.issue(
                        Some(&root),
                        "claude/unresolved-forward",
                        DiagnosticCode::MissingOwner,
                        "Forwarded tool host is not a proven native child identity",
                    );
                    return;
                }
            }
        } else {
            root
        };
        match text(value, "type").unwrap_or("") {
            "system" if text(value, "subtype") == Some("init") => {
                self.declare_agent(&owner, None, text(value, "model").map(str::to_owned));
                self.issue(
                    Some(&owner),
                    "claude/live-selection",
                    DiagnosticCode::UnknownSelection,
                    "SDK initialization does not establish a native history head",
                );
            }
            "system"
                if matches!(
                    text(value, "subtype"),
                    Some("task_started" | "task_updated" | "task_notification")
                ) =>
            {
                self.task(&owner, value)
            }
            "system" => {}
            "stream_event" => self.stream(&owner, &value["event"]),
            "assistant" | "user" => self.record(&owner, value, true),
            "result" => {
                let Some((activation, start)) = self.state.runs.remove(&owner.encode()) else {
                    self.issue(
                        Some(&owner),
                        "claude/result-scope",
                        DiagnosticCode::CaptureGap,
                        "SDK result has no observed matching execution loop",
                    );
                    return;
                };
                let outcome = if value["is_error"] == true {
                    TerminalOutcome::Failed
                } else if text(value, "subtype") == Some("success") && value["is_error"] == false {
                    TerminalOutcome::Succeeded
                } else {
                    TerminalOutcome::Unknown
                };
                let detail = self.json(value, "/result");
                self.add(
                    &owner,
                    "claude/loop-terminated",
                    Fact::Execution {
                        execution: ExecutionRef::Activation(activation.clone()),
                        boundary: ExecutionBoundary::Terminated {
                            time: None,
                            reason: Some(ExecutionCloseReason::VendorStop),
                        },
                    },
                );
                self.add(
                    &owner,
                    "claude/loop-outcome",
                    Fact::Execution {
                        execution: ExecutionRef::Activation(activation.clone()),
                        boundary: ExecutionBoundary::Outcome {
                            outcome,
                            reason: text(value, "terminal_reason").map(|reason| {
                                ExecutionCloseReason::Extension {
                                    namespace: "claude.terminal_reason".into(),
                                    code: reason.into(),
                                }
                            }),
                            detail: Some(detail),
                        },
                    },
                );
                self.output.claims.last_mut().unwrap().evidence.push(start);
                self.measurements(
                    &owner,
                    &value["usage"],
                    "claude/result-usage",
                    AnyObjectRef::Activation(activation.clone()),
                );
                if let Some(amount) = value.get("total_cost_usd").and_then(Value::as_number) {
                    self.object(
                        &owner,
                        "claude/sdk-reported-cost",
                        TraceObject::Measurement(Measurement {
                            measurement_ref: MeasurementRef::occurrence(
                                owner.clone(),
                                self.input.capture.source_position.clone(),
                                vec!["sdk-cost".into()],
                            )
                            .unwrap(),
                            subject: AnyObjectRef::Activation(activation.clone()),
                            metric: Metric::Cost {
                                pricing_basis: "claude/sdk-reported-total_cost_usd".into(),
                            },
                            unit: MeasurementUnit::Currency { code: "USD".into() },
                            value: amount.clone(),
                            mode: MeasurementMode::Gauge,
                            aggregation_scope: AggregationScope::NamedSubject,
                            components: vec![],
                        }),
                    );
                }
                for field in ["duration_ms", "duration_api_ms"] {
                    if let Some(number) = value.get(field).and_then(Value::as_number) {
                        self.object(
                            &owner,
                            &format!("claude/result/{field}"),
                            TraceObject::Measurement(Measurement {
                                measurement_ref: MeasurementRef::occurrence(
                                    owner.clone(),
                                    self.input.capture.source_position.clone(),
                                    vec![field.into()],
                                )
                                .unwrap(),
                                subject: AnyObjectRef::Activation(activation.clone()),
                                metric: if field == "duration_ms" {
                                    Metric::ReportedDuration
                                } else {
                                    Metric::Extension {
                                        namespace: "claude".into(),
                                        name: field.into(),
                                    }
                                },
                                unit: MeasurementUnit::Milliseconds,
                                value: number.clone(),
                                mode: MeasurementMode::Gauge,
                                aggregation_scope: AggregationScope::NamedSubject,
                                components: vec![],
                            }),
                        );
                    }
                }
            }
            _ => self.issue(
                Some(&owner),
                "claude/live-kind",
                DiagnosticCode::UnsupportedKind,
                "Unknown Claude live envelope is retained",
            ),
        }
    }

    fn live_part_ref(
        owner: &AgentRef,
        stream: &LiveStream,
        index: u64,
        field: &str,
    ) -> MessagePartRef {
        MessagePartRef::occurrence(
            owner.clone(),
            stream.start.clone(),
            vec!["live-part".into(), index.to_string(), field.into()],
        )
        .unwrap()
    }

    fn stream_parts(
        &mut self,
        owner: &AgentRef,
        stream: &LiveStream,
        only: Option<u64>,
    ) -> Vec<ContentPart> {
        let mut parts = Vec::new();
        for (index, value) in stream
            .parts
            .iter()
            .filter(|(index, _)| only.is_none_or(|only| only == **index))
        {
            let selector = format!("/stream/content/{index}");
            let mut part = if let Some(raw) = stream.input_json.get(index) {
                // The initial input object is a placeholder once JSON deltas begin.
                // Never publish it as the final value when the accumulated input is invalid.
                let mut incomplete = value.clone();
                if let Some(object) = incomplete.as_object_mut() {
                    object.remove("input");
                    object.insert("partial_json".into(), Value::String(raw.clone()));
                }
                ContentPart {
                    part_ref: Self::live_part_ref(owner, stream, *index, "content"),
                    kind: ContentKind::Opaque,
                    payload: self.json(&incomplete, &selector),
                    media_type: Some("application/json".into()),
                    encoding: PayloadEncoding::Utf8,
                }
            } else {
                self.part(owner, &stream.message.encode(), *index, value, &selector)
            };
            part.part_ref = Self::live_part_ref(owner, stream, *index, "content");
            parts.push(part);
            if let Some(signature) = text(value, "signature") {
                parts.push(ContentPart {
                    part_ref: Self::live_part_ref(owner, stream, *index, "signature"),
                    kind: ContentKind::Opaque,
                    payload: self.payload(
                        signature.as_bytes().to_vec(),
                        &format!("{selector}/signature"),
                        "text/plain",
                        PayloadEncoding::Utf8,
                    ),
                    media_type: Some("text/plain".into()),
                    encoding: PayloadEncoding::Utf8,
                });
            }
        }
        parts
    }

    fn stream_revision(
        &mut self,
        owner: &AgentRef,
        stream: &mut LiveStream,
        slot: &str,
        form: RevisionForm,
        parts: Vec<ContentPart>,
    ) {
        // Append payloads depend on this frame and the message identity. Only a
        // replacement/final reconstructs prior bytes and needs the full prefix proof.
        let evidence = if matches!(form, RevisionForm::Append { .. }) {
            vec![
                stream.evidence[0].clone(),
                self.input.canonical_observation.clone(),
            ]
        } else {
            stream.evidence.clone()
        };
        let revision = MessageRevisionRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec![slot.into()],
        )
        .unwrap();
        self.object(
            owner,
            slot,
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: stream.message.clone(),
                representation: Representation::Conversation,
                form,
                parts,
            }),
        );
        // Reading membership observes this live prefix; it does not claim vendor
        // conversation admission or a selected native history head.
        let entry = EntryRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["live-message-prefix".into()],
        )
        .unwrap();
        self.object(
            owner,
            "claude/live-history",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: owner.clone(),
                predecessor: stream.previous_entry.clone(),
                original_owner: None,
            }),
        );
        self.add(
            owner,
            "claude/live-history-content",
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry.clone(),
                    self.input.capture.source_position.clone(),
                    "claude/live-history",
                    vec![HistoryContent::Message(revision.clone())],
                ),
            },
        );
        for claim in &mut self.output.claims {
            claim.evidence.extend(evidence.iter().cloned());
            claim.evidence.sort();
            claim.evidence.dedup();
        }
        for payload in &mut self.output.payloads {
            payload.evidence.extend(evidence.iter().cloned());
            payload.evidence.sort();
            payload.evidence.dedup();
        }
        stream.previous = Some(revision);
        stream.previous_entry = Some(entry);
    }

    fn seal_stream_input(&mut self, owner: &AgentRef, stream: &mut LiveStream, index: u64) {
        let Some(raw) = stream.input_json.get(&index) else {
            return;
        };
        match serde_json::from_str::<Value>(raw) {
            Ok(input) if input.is_object() => {
                stream.parts.get_mut(&index).unwrap()["input"] = input;
                stream.input_json.remove(&index);
            }
            _ => self.issue(
                Some(owner), &format!("claude/stream-input/{index}"),
                DiagnosticCode::DecodeFailure,
                "Stopped tool input is not a complete JSON object; retained fragments remain opaque",
            ),
        }
    }

    fn stream(&mut self, owner: &AgentRef, event: &Value) {
        let kind = text(event, "type").unwrap_or("");
        let key = owner.encode();
        if kind == "message_start" {
            if !self.state.runs.contains_key(&key) {
                let activation = ActivationRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["sdk-loop".into()],
                )
                .unwrap();
                self.object(
                    owner,
                    "claude/live-loop",
                    TraceObject::Activation(Activation {
                        activation_ref: activation.clone(),
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    }),
                );
                self.add(
                    owner,
                    "claude/live-loop-start",
                    Fact::Execution {
                        execution: ExecutionRef::Activation(activation.clone()),
                        boundary: ExecutionBoundary::Started { time: None },
                    },
                );
                self.state.runs.insert(
                    key.clone(),
                    (activation, self.input.canonical_observation.clone()),
                );
            }
            let message = MessageRef::occurrence(
                owner.clone(),
                self.input.capture.source_position.clone(),
                vec!["live-model-message".into()],
            )
            .unwrap();
            self.object(
                owner,
                "claude/live-message",
                TraceObject::Message(Message {
                    message_ref: message.clone(),
                    author: Author::Agent {
                        agent: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    },
                    protocol_role: Some(ProtocolRole::Assistant),
                    purpose: MessagePurpose::Unclassified,
                    production: Some(ProductionScope {
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation: self
                            .state
                            .runs
                            .get(&key)
                            .map(|(activation, _)| activation.clone()),
                    }),
                }),
            );
            self.state.streams.insert(
                key,
                LiveStream {
                    message,
                    start: self.input.capture.source_position.clone(),
                    previous: None,
                    parts: BTreeMap::new(),
                    input_json: BTreeMap::new(),
                    previous_entry: None,
                    evidence: vec![self.input.canonical_observation.clone()],
                },
            );
            return;
        }
        let Some(mut stream) = self.state.streams.get(&key).cloned() else {
            self.issue(
                Some(owner),
                "claude/stream-start",
                DiagnosticCode::CaptureGap,
                "Content update has no observed model-message start",
            );
            return;
        };
        stream
            .evidence
            .push(self.input.canonical_observation.clone());
        match kind {
            "content_block_start" => {
                let Some(index) = event["index"].as_u64() else {
                    return;
                };
                if stream.parts.contains_key(&index) {
                    self.issue(
                        Some(owner),
                        "claude/part-repeated-start",
                        DiagnosticCode::InvalidRelation,
                        "Content block index was started twice in one model message",
                    );
                    return;
                }
                stream.parts.insert(index, event["content_block"].clone());
                let parts = self.stream_parts(owner, &stream, Some(index));
                let form = RevisionForm::Append {
                    predecessor: stream.previous.clone(),
                };
                self.stream_revision(owner, &mut stream, "claude/live-part-start", form, parts);
            }
            "content_block_delta" => {
                let Some(index) = event["index"].as_u64() else {
                    return;
                };
                let Some(part) = stream.parts.get(&index) else {
                    self.issue(
                        Some(owner),
                        "claude/part-start",
                        DiagnosticCode::CaptureGap,
                        "Delta precedes its content-block start",
                    );
                    return;
                };
                let delta = &event["delta"];
                let field = match text(delta, "type") {
                    Some("text_delta") if text(part, "type") == Some("text") => "text",
                    Some("thinking_delta") if text(part, "type") == Some("thinking") => "thinking",
                    Some("input_json_delta")
                        if matches!(
                            text(part, "type"),
                            Some("tool_use" | "server_tool_use" | "mcp_tool_use")
                        ) =>
                    {
                        "partial_json"
                    }
                    Some("signature_delta") if text(part, "type") == Some("thinking") => {
                        "signature"
                    }
                    _ => {
                        self.issue(
                            Some(owner),
                            "claude/delta-kind",
                            DiagnosticCode::UnsupportedKind,
                            "Delta kind is unsupported or differs from its content block",
                        );
                        return;
                    }
                };
                let Some(segment) = text(delta, field) else {
                    self.issue(
                        Some(owner),
                        "claude/delta-data",
                        DiagnosticCode::DecodeFailure,
                        "Delta omitted its declared string field",
                    );
                    return;
                };
                if field == "partial_json" {
                    stream
                        .input_json
                        .entry(index)
                        .or_default()
                        .push_str(segment);
                } else if let Some(object) =
                    stream.parts.get_mut(&index).and_then(Value::as_object_mut)
                {
                    // Anthropic SDK 0.81.0 replaces signature_delta.signature;
                    // it appends text/thinking and accumulates input_json separately.
                    let prior = if field == "signature" {
                        ""
                    } else {
                        object.get(field).and_then(Value::as_str).unwrap_or("")
                    };
                    object.insert(field.into(), Value::String(prior.to_owned() + segment));
                }
                if field == "signature" {
                    let parts = self.stream_parts(owner, &stream, None);
                    let form = RevisionForm::Replacement {
                        covers: Some(SourcePrefix {
                            through: self.input.capture.source_position.clone(),
                        }),
                    };
                    self.stream_revision(owner, &mut stream, "claude/live-signature", form, parts);
                } else {
                    let part = ContentPart {
                        part_ref: Self::live_part_ref(
                            owner,
                            &stream,
                            index,
                            if field == "partial_json" {
                                "input_json"
                            } else {
                                "content"
                            },
                        ),
                        kind: match field {
                            "text" => ContentKind::Text,
                            "thinking" => ContentKind::DisclosedReasoning,
                            _ => ContentKind::Opaque,
                        },
                        payload: self.payload(
                            segment.as_bytes().to_vec(),
                            &format!("/event/delta/{field}"),
                            "text/plain",
                            PayloadEncoding::Utf8,
                        ),
                        media_type: Some("text/plain".into()),
                        encoding: PayloadEncoding::Utf8,
                    };
                    let form = RevisionForm::Append {
                        predecessor: stream.previous.clone(),
                    };
                    self.stream_revision(owner, &mut stream, "claude/live-delta", form, vec![part]);
                }
            }
            "content_block_stop" => {
                if let Some(index) = event["index"]
                    .as_u64()
                    .filter(|index| stream.input_json.contains_key(index))
                {
                    self.seal_stream_input(owner, &mut stream, index);
                    let parts = self.stream_parts(owner, &stream, None);
                    let form = RevisionForm::Replacement {
                        covers: Some(SourcePrefix {
                            through: self.input.capture.source_position.clone(),
                        }),
                    };
                    self.stream_revision(
                        owner,
                        &mut stream,
                        "claude/live-part-sealed",
                        form,
                        parts,
                    );
                }
            }
            "message_stop" => {
                for index in stream.input_json.keys().copied().collect::<Vec<_>>() {
                    self.seal_stream_input(owner, &mut stream, index);
                }
                let parts = self.stream_parts(owner, &stream, None);
                let form = RevisionForm::Final {
                    covers: Some(SourcePrefix {
                        through: self.input.capture.source_position.clone(),
                    }),
                };
                self.stream_revision(owner, &mut stream, "claude/live-final", form, parts);
                self.state.streams.remove(&key);
                return;
            }
            "message_delta" => {}
            _ => self.issue(
                Some(owner),
                "claude/stream-kind",
                DiagnosticCode::UnsupportedKind,
                "Unknown Anthropic stream event remains retained",
            ),
        }
        self.state.streams.insert(key, stream);
    }

    fn task(&mut self, owner: &AgentRef, value: &Value) {
        let Some(id) = text(value, "task_id") else {
            self.issue(
                Some(owner),
                "claude/task-id",
                DiagnosticCode::MissingOwner,
                "Task event has no task_id",
            );
            return;
        };
        let resource = ResourceRef::native(
            owner.clone(),
            "claude.task",
            vec![
                self.input.capture.observation_ref.producer_epoch.clone(),
                id.into(),
            ],
        )
        .unwrap();
        if let Some(task_type) = text(value, "task_type") {
            self.object(
                owner,
                "claude/task-resource",
                TraceObject::Resource(Resource {
                    resource_ref: resource.clone(),
                    creator: text(value, "tool_use_id").map(|id| operation(owner, id)),
                    host: Some(OwnerRef::Agent {
                        agent: owner.clone(),
                    }),
                    kind: ResourceKind::Extension {
                        namespace: "claude.task".into(),
                        name: task_type.into(),
                    },
                }),
            );
        }
        let status = text(&value["patch"], "status").or_else(|| text(value, "status"));
        let state = if text(value, "subtype") == Some("task_started") {
            ResourceState::Running
        } else {
            match status {
                Some("completed") => ResourceState::Closed {
                    outcome: TerminalOutcome::Succeeded,
                    reason: None,
                },
                Some("failed") => ResourceState::Closed {
                    outcome: TerminalOutcome::Failed,
                    reason: None,
                },
                Some("stopped") => ResourceState::Closed {
                    outcome: TerminalOutcome::Interrupted,
                    reason: None,
                },
                _ => ResourceState::Unknown {
                    reason: "Task status does not establish a terminal outcome".into(),
                },
            }
        };
        self.add(
            owner,
            "claude/task-state",
            Fact::ResourceState { resource, state },
        );
    }

    fn measurements(&mut self, owner: &AgentRef, value: &Value, slot: &str, subject: AnyObjectRef) {
        for (field, metric) in [
            ("input_tokens", Metric::InputTokens),
            ("output_tokens", Metric::OutputTokens),
            ("cache_read_input_tokens", Metric::CachedTokens),
            (
                "cache_creation_input_tokens",
                Metric::Extension {
                    namespace: "claude.usage".into(),
                    name: "cache_creation_input_tokens".into(),
                },
            ),
        ] {
            if let Some(number) = value.get(field).and_then(Value::as_number) {
                self.object(
                    owner,
                    &format!("{slot}/{field}"),
                    TraceObject::Measurement(Measurement {
                        measurement_ref: MeasurementRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec![slot.into(), field.into()],
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
        if let Some(number) = value
            .pointer("/output_tokens_details/thinking_tokens")
            .and_then(Value::as_number)
        {
            self.object(
                owner,
                &format!("{slot}/thinking_tokens"),
                TraceObject::Measurement(Measurement {
                    measurement_ref: MeasurementRef::occurrence(
                        owner.clone(),
                        self.input.capture.source_position.clone(),
                        vec![slot.into(), "thinking_tokens".into()],
                    )
                    .unwrap(),
                    subject,
                    metric: Metric::ReasoningTokens,
                    unit: MeasurementUnit::Tokens,
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
                        code: DiagnosticCode::UnsupportedKind
                            | DiagnosticCode::DecodeFailure
                            | DiagnosticCode::MissingOwner,
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
                    "claude/mapped-record",
                    Fact::HistoryMappingComplete {
                        extent: SourceExtent {
                            source: self.input.capture.source_position.source.clone(),
                            generation: self.input.capture.source_position.generation,
                            coordinate: HistoryCoordinate::LocalRecordIndex,
                            start_inclusive: record_ordinal,
                            end_exclusive: end,
                            mapping_rule: "claude/native-history".into(),
                            original_owner: None,
                        },
                    },
                );
            }
        }
    }
}

fn root_of(owner: &AgentRef) -> AgentRef {
    AgentRef::Session(owner.anchor_session().clone())
}
fn entry(owner: &AgentRef, id: &str) -> EntryRef {
    EntryRef::native(owner.clone(), "claude.record-entry", vec![id.into()]).unwrap()
}
fn operation(owner: &AgentRef, id: &str) -> OperationRef {
    OperationRef::native(owner.clone(), "claude.tool-call", vec![id.into()]).unwrap()
}
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn parts(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(parts)) => parts.clone(),
        Some(Value::String(text)) => vec![serde_json::json!({"type":"text","text":text})],
        Some(value) => vec![value.clone()],
        None => vec![],
    }
}

#[cfg(test)]
mod tests;
