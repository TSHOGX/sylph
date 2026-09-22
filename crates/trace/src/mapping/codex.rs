//! Codex native records and app-server notifications mapped from retained bytes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sylph_protocol::{trace::*, AgentName, NativeId, SessionId};

use super::{
    claim, correlation_key, derived_payload_with_media, ArchiveRequest, MapInput, MapOutput,
    MappingDiagnostic, SemanticMapper,
};

pub mod history;

pub struct CodexMapper;
impl CodexMapper {
    const BUILD: &'static str = "codex-3";
}
mod collaboration;
mod goal;
mod runtime;

/// Checkpointed source knowledge. A copied parent header cannot replace the first
/// governing header, and a parent connection does not own forwarded child frames.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CodexState {
    inherited_before: Option<u64>,
    inherited_from: Option<AgentRef>,
    governing_header_seen: bool,
    header_evidence: Option<ObservationRef>,
    accounting_root: Option<AgentRef>,
    agent_path: Option<String>,
    communication_metadata: Option<(bool, ObservationRef)>,
    last_vendor_ordinal: Option<u64>,
    active_by_agent: BTreeMap<String, ActivationRef>,
    revision_by_message: BTreeMap<String, MessageRevisionRef>,
    entry_by_agent: BTreeMap<String, EntryRef>,
    rpc_requests: BTreeMap<String, (String, ObservationRef)>,
    runtime: runtime::RuntimeState,
}

impl SemanticMapper for CodexMapper {
    fn build(&self) -> &'static str {
        Self::BUILD
    }

    fn map(&self, input: MapInput<'_>) -> MapOutput {
        if input.capture.transport_kind == TransportKind::NativeDatabase {
            return map_database(input);
        }
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let Ok(value) = serde_json::from_slice::<Value>(input.raw) else {
            if let Some(scope) = input
                .state
                .governing_agent
                .as_ref()
                .or(input.capture.declared_subject_refs.first())
            {
                output.claims.push(claim(
                    &input,
                    scope,
                    "codex/decode",
                    self.build(),
                    diagnostic(
                        scope,
                        DiagnosticCode::DecodeFailure,
                        "The retained Codex observation is not valid JSON",
                    ),
                ));
            } else {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::DecodeFailure,
                    message:
                        "The retained Codex observation is not valid JSON and has no known owner"
                            .into(),
                });
            }
            return output;
        };
        history::record_source(&input, &value, &mut output);
        let mut state = match input.state.values.get("codex") {
            Some(value) => match serde_json::from_value(value.clone()) {
                Ok(state) => state,
                Err(_) => {
                    if let Some(scope) = input
                        .state
                        .governing_agent
                        .as_ref()
                        .or(input.capture.declared_subject_refs.first())
                    {
                        output.claims.push(claim(
                            &input,
                            scope,
                            "codex/state",
                            self.build(),
                            diagnostic(
                                scope,
                                DiagnosticCode::DecodeFailure,
                                "The Codex mapper checkpoint is invalid",
                            ),
                        ));
                    }
                    return output;
                }
            },
            None => CodexState::default(),
        };
        let is_live = matches!(
            input.capture.transport_kind,
            TransportKind::LiveStdout | TransportKind::LiveStdin
        );
        let mut context = Context {
            input: &input,
            output: &mut output,
            state: &mut state,
            build: self.build(),
            vendor_ordinal: value.get("ordinal").and_then(Value::as_u64),
        };
        if value.get("schema_version").is_some() && value.get("rollout_id").is_some() {
            context.raw_trace(&value);
        } else if is_live {
            context.live(&value);
        } else {
            context.native(&value);
        }
        output.state.values.insert(
            "codex".into(),
            serde_json::to_value(state).expect("typed Codex checkpoint serializes"),
        );
        output
    }
}

fn map_database(input: MapInput<'_>) -> MapOutput {
    use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseStatus};
    let mut output = MapOutput {
        state: input.state.clone(),
        ..MapOutput::default()
    };
    let snapshot: NativeDatabaseSnapshot = match serde_json::from_slice(input.raw) {
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
    let issue = |output: &mut MapOutput, code, message: &str| {
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code,
            message: message.into(),
        })
    };
    if snapshot.table == "thread_goals" {
        return goal::map_database(input, snapshot);
    }
    if snapshot.status != NativeDatabaseStatus::Available {
        issue(
            &mut output,
            DiagnosticCode::UnknownSelection,
            "Native selected-rollout metadata is unavailable",
        );
        return output;
    }
    if snapshot.table != "threads" {
        issue(
            &mut output,
            DiagnosticCode::UnsupportedKind,
            "Unknown Codex native database table",
        );
        return output;
    }
    history::map_selection(input, snapshot)
}

struct Context<'a, 'b> {
    input: &'a MapInput<'b>,
    output: &'a mut MapOutput,
    state: &'a mut CodexState,
    build: &'static str,
    vendor_ordinal: Option<u64>,
}

impl Context<'_, '_> {
    fn add(&mut self, scope: &AgentRef, slot: &str, fact: Fact) {
        let mut claim = claim(self.input, scope, slot, self.build, fact);
        if self.input.capture.transport_kind == TransportKind::NativeJsonl {
            if let Some(header) = &self.state.header_evidence {
                if !claim.evidence.contains(header) {
                    claim.evidence.push(header.clone());
                }
            }
        }
        self.output.claims.push(claim);
    }

    fn declare(&mut self, scope: &AgentRef, slot: &str, object: TraceObject) {
        self.add(scope, slot, Fact::Declare { object });
    }

    fn issue(&mut self, scope: &AgentRef, slot: &str, code: DiagnosticCode, message: &str) {
        self.add(scope, slot, diagnostic(scope, code, message));
    }

    fn owner_missing(&mut self, message: &str) {
        if let Some(scope) = self
            .output
            .state
            .governing_agent
            .clone()
            .or_else(|| self.input.capture.declared_subject_refs.first().cloned())
        {
            self.issue(
                &scope,
                "codex/owner-missing",
                DiagnosticCode::MissingOwner,
                message,
            );
        } else {
            self.output.diagnostics.push(MappingDiagnostic {
                evidence: vec![self.input.canonical_observation.clone()],
                code: DiagnosticCode::MissingOwner,
                message: message.into(),
            });
        }
    }

    fn agent(&self, native_id: &str) -> Option<AgentRef> {
        if native_id.is_empty() {
            return None;
        }
        let environment = match &self.input.capture.observation_ref.issuer {
            Issuer::Environment(environment) => environment.clone(),
            Issuer::Server(_) => return None,
        };
        Some(AgentRef::Session(SessionId::new(
            environment,
            AgentName::parse("codex").unwrap(),
            NativeId::new(native_id),
        )))
    }

    fn content(&mut self, bytes: Vec<u8>, selector: &str) -> PayloadRef {
        let payload = derived_payload_with_media(
            self.input,
            bytes,
            selector,
            "codex/json-field-decoding",
            "text/plain",
            PayloadEncoding::Utf8,
        );
        let reference = payload.reference.clone();
        self.output.payloads.push(payload);
        reference
    }

    fn opaque(&mut self, value: &Value, selector: &str) -> PayloadRef {
        let payload = derived_payload_with_media(
            self.input,
            serde_json::to_vec(value).expect("retained JSON value serializes"),
            selector,
            "codex/json-field-decoding",
            "application/json",
            PayloadEncoding::Utf8,
        );
        let reference = payload.reference.clone();
        self.output.payloads.push(payload);
        reference
    }

    fn announce_agent(&mut self, agent: &AgentRef, value: &Value, live: bool) {
        if !live {
            self.output.source_owners.push(super::SourceOwnership {
                source: self.input.capture.source_position.source.clone(),
                generation: self.input.capture.source_position.generation,
                owner: super::SourceOwner {
                    agent: agent.clone(),
                    evidence: vec![self.input.canonical_observation.clone()],
                    rule: "codex/governing-native-header".into(),
                },
            });
        }
        let name = string(
            value,
            if live {
                "agentNickname"
            } else {
                "agent_nickname"
            },
        )
        .map(str::to_owned);
        self.declare(
            agent,
            "codex/agent",
            TraceObject::Agent(AgentThread {
                agent_ref: agent.clone(),
                vendor: AgentName::parse("codex").unwrap(),
                name,
                model: string(value, "model").map(str::to_owned),
            }),
        );
        self.add(
            agent,
            "codex/agent-exists",
            Fact::AgentState {
                agent: agent.clone(),
                state: AgentState::Exists,
            },
        );
        let parent = string(
            value,
            if live {
                "parentThreadId"
            } else {
                "parent_thread_id"
            },
        )
        .or_else(|| {
            value
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .and_then(Value::as_str)
        });
        if let Some(parent) = parent.and_then(|id| self.agent(id)) {
            self.add(
                agent,
                "codex/spawn-parent",
                Fact::Relation {
                    relation: Relation::Spawn {
                        parent,
                        child: agent.clone(),
                        operation: None,
                    },
                },
            );
        }
    }

    fn native(&mut self, value: &Value) {
        let outer = string(value, "type").unwrap_or("");
        let payload = &value["payload"];
        if outer == "session_meta" {
            if self.state.governing_header_seen {
                if let Some(agent) = self.output.state.governing_agent.clone() {
                    self.mapping_complete(&agent);
                }
                return;
            }
            let Some(agent) = string(payload, "id").and_then(|id| self.agent(id)) else {
                self.owner_missing(
                    "Native header omits a valid ID or its capture lacks an environment issuer",
                );
                return;
            };
            self.output.state.governing_agent = Some(agent.clone());
            self.state.governing_header_seen = true;
            self.state.header_evidence = Some(self.input.canonical_observation.clone());
            self.state.accounting_root =
                string(payload, "session_id").and_then(|id| self.agent(id));
            self.state.agent_path = string(payload, "agent_path").map(str::to_owned);
            if let (Some(root), Some(path)) = (&self.state.accounting_root, &self.state.agent_path)
            {
                self.output.correlations.insert(
                    correlation_key(root, "codex.agent-path", std::slice::from_ref(path)),
                    AnyObjectRef::Agent(agent.clone()),
                );
            }
            self.state.inherited_before = payload
                .get("subagent_history_start_ordinal")
                .and_then(Value::as_u64);
            self.state.inherited_from = string(payload, "parent_thread_id")
                .or_else(|| string(payload, "forked_from_id"))
                .and_then(|id| self.agent(id));
            self.state.last_vendor_ordinal = value.get("ordinal").and_then(Value::as_u64);
            self.announce_agent(&agent, payload, false);
            self.issue(&agent, "codex/selection-unknown", DiagnosticCode::UnknownSelection,
                "A native rollout header does not establish the vendor's currently selected rollout");
            self.mapping_complete(&agent);
            return;
        }
        let Some(containing) = self.output.state.governing_agent.clone() else {
            self.owner_missing("Native record has no retained governing session header");
            return;
        };
        let ordinal = value.get("ordinal").and_then(Value::as_u64);
        if let (Some(previous), Some(current)) = (self.state.last_vendor_ordinal, ordinal) {
            if current <= previous {
                self.issue(
                    &containing,
                    "codex/ordinal-regression",
                    DiagnosticCode::InvalidRelation,
                    "Native ordinals repeat or regress within this physical source",
                );
            }
        }
        self.state.last_vendor_ordinal = ordinal.or(self.state.last_vendor_ordinal);
        let inherited = match (self.state.inherited_before, ordinal) {
            (Some(cutoff), Some(ordinal)) => ordinal < cutoff,
            (Some(_), None) => {
                self.issue(&containing,"codex/inheritance-coordinate-missing",DiagnosticCode::MissingOwner,"Inherited ownership needs the recorded vendor ordinal; physical line count is not that coordinate");
                return;
            }
            _ => false,
        };
        if inherited {
            // Inherited telemetry and lifecycle are retained evidence, never local
            // execution. Content keeps its original owner when the header proves it.
            if outer == "response_item" && string(payload, "type") == Some("message") {
                if let Some(origin) = self.state.inherited_from.clone() {
                    let content =
                        self.message(&origin, payload, "/payload", None, RevisionForm::Complete);
                    if let Some(content) = content {
                        self.history(
                            &containing,
                            vec![HistoryContent::Message(content)],
                            Some(OwnerRef::Agent { agent: origin }),
                        );
                    }
                } else {
                    self.issue(
                        &containing,
                        "codex/inherited-owner-unknown",
                        DiagnosticCode::MissingOwner,
                        "The inherited content has no evidenced original agent",
                    );
                }
            }
            if matches!(outer, "turn_context" | "world_state")
                || (outer == "response_item" && string(payload, "type") == Some("message"))
            {
                self.mapping_complete(&containing);
            }
            return;
        }
        let kind = if outer == "event_msg" || outer == "response_item" {
            string(payload, "type").unwrap_or("")
        } else {
            outer
        };
        match kind {
            "exec_command_begin"
            | "exec_command_end"
            | "exec_command_output_delta"
            | "terminal_interaction" => self.core_runtime(&containing, kind, payload),
            "thread_goal_updated" => self.goal_update(&containing, payload),
            kind if kind.starts_with("collab_") => {
                self.collaboration_event(&containing, kind, payload)
            }
            "task_started" | "task_complete" | "turn_aborted" => {
                self.native_boundary(&containing, kind, payload)
            }
            "agent_message" => self.communication(&containing, payload),
            "message" => {
                let activation = self
                    .state
                    .active_by_agent
                    .get(&containing.encode())
                    .cloned();
                if let Some(revision) = self.message(
                    &containing,
                    payload,
                    "/payload",
                    activation,
                    RevisionForm::Complete,
                ) {
                    self.history(&containing, vec![HistoryContent::Message(revision)], None);
                }
            }
            "function_call" | "custom_tool_call" => self.model_call(&containing, payload, kind),
            "function_call_output" | "custom_tool_call_output" => {
                self.model_result(&containing, payload)
            }
            "item_completed" => {
                let owner = string(payload, "thread_id")
                    .and_then(|id| self.agent(id))
                    .unwrap_or(containing.clone());
                let activation = string(payload, "turn_id").map(|id| activation(&owner, id));
                self.item(&owner, &payload["item"], false, false, activation, payload);
            }
            "reasoning" => self.reasoning(&containing, payload, "/payload"),
            "compacted" => self.compaction(&containing, payload),
            "token_count" => self.token_count(&containing, &payload["info"], false),
            "token_usage_record" => self.response_usage(&containing, payload),
            "inter_agent_communication_metadata" => {
                self.state.communication_metadata = payload
                    .get("trigger_turn")
                    .and_then(Value::as_bool)
                    .map(|trigger| (trigger, self.input.canonical_observation.clone()));
            }
            "turn_context" | "world_state" | "thread_settings_applied" => {}
            _ => self.issue(
                &containing,
                "codex/unsupported-native",
                DiagnosticCode::UnsupportedKind,
                &format!("Retained native record kind {kind:?} has no declared semantic mapping"),
            ),
        }
        if kind != "inter_agent_communication_metadata" {
            self.state.communication_metadata = None;
        }
        self.mapping_complete(&containing);
    }

    fn mapping_complete(&mut self, owner: &AgentRef) {
        if self.input.capture.transport_kind != TransportKind::NativeJsonl
            || self.output.claims.iter().any(|claim| {
                matches!(
                    &claim.fact,
                    Fact::Diagnostic {
                        diagnostic: TraceDiagnostic {
                            code: DiagnosticCode::DecodeFailure
                                | DiagnosticCode::UnsupportedKind
                                | DiagnosticCode::MissingOwner
                                | DiagnosticCode::InvalidRelation,
                            ..
                        }
                    }
                )
            })
        {
            return;
        }
        let position = self.input.capture.source_position.clone();
        let mut coordinates = vec![];
        if let SourceLocator::Jsonl { record_ordinal, .. } = position.locator {
            coordinates.push(("local", HistoryCoordinate::LocalRecordIndex, record_ordinal));
        }
        if let Some(ordinal) = self.vendor_ordinal {
            coordinates.push((
                "vendor",
                HistoryCoordinate::VendorOrdinal {
                    domain_ref: position.source.clone(),
                },
                ordinal,
            ));
        }
        for (slot, coordinate, value) in coordinates {
            let Some(end) = value.checked_add(1) else {
                continue;
            };
            self.add(
                owner,
                &format!("codex/mapped-record/{slot}"),
                Fact::HistoryMappingComplete {
                    extent: SourceExtent {
                        source: position.source.clone(),
                        generation: position.generation,
                        coordinate,
                        start_inclusive: value,
                        end_exclusive: end,
                        mapping_rule: "codex/native-history".into(),
                        original_owner: None,
                    },
                },
            );
        }
    }

    fn native_boundary(&mut self, owner: &AgentRef, kind: &str, payload: &Value) {
        let Some(id) = string(payload, "turn_id") else {
            self.issue(
                owner,
                "codex/activation-id-missing",
                DiagnosticCode::MissingOwner,
                "Execution boundary has no native turn ID",
            );
            return;
        };
        let activation = activation(owner, id);
        self.declare(
            owner,
            "codex/activation",
            TraceObject::Activation(Activation {
                activation_ref: activation.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            }),
        );
        if kind == "task_started" {
            self.state
                .active_by_agent
                .insert(owner.encode(), activation.clone());
            self.output.state.current_activation = Some(activation.clone());
            self.add(
                owner,
                "codex/activation-start",
                Fact::Execution {
                    execution: ExecutionRef::Activation(activation),
                    boundary: ExecutionBoundary::Started {
                        time: time(payload, "started_at", TimeUnit::Seconds, owner),
                    },
                },
            );
            return;
        }
        let outcome = if kind == "turn_aborted" {
            TerminalOutcome::Interrupted
        } else if payload.get("error").is_some_and(|error| !error.is_null()) {
            TerminalOutcome::Failed
        } else {
            TerminalOutcome::Succeeded
        };
        if let Some(start) = time(payload, "started_at", TimeUnit::Seconds, owner) {
            self.add(
                owner,
                "codex/reported-activation-start",
                Fact::Execution {
                    execution: ExecutionRef::Activation(activation.clone()),
                    boundary: ExecutionBoundary::Started { time: Some(start) },
                },
            );
        }
        self.close(
            owner,
            ExecutionRef::Activation(activation.clone()),
            outcome,
            time(payload, "completed_at", TimeUnit::Seconds, owner),
            payload.get("error"),
        );
        if let Some(duration) = payload.get("duration_ms").and_then(Value::as_number) {
            self.duration(
                owner,
                AnyObjectRef::Activation(activation.clone()),
                duration.clone(),
            );
        }
        if self.state.active_by_agent.get(&owner.encode()) == Some(&activation) {
            self.state.active_by_agent.remove(&owner.encode());
            self.output.state.current_activation = None;
        }
    }

    fn live(&mut self, value: &Value) {
        if self.input.capture.transport_kind == TransportKind::LiveStdin {
            if let (Some(id), Some(method)) = (value.get("id"), string(value, "method")) {
                self.state.rpc_requests.insert(
                    id.to_string(),
                    (method.into(), self.input.canonical_observation.clone()),
                );
            }
            return;
        }
        if value.get("method").is_none() {
            if let Some(id) = value.get("id") {
                if let Some((method, request)) = self.state.rpc_requests.remove(&id.to_string()) {
                    if matches!(method.as_str(), "thread/start" | "thread/resume") {
                        let thread = &value["result"]["thread"];
                        if let Some(agent) = string(thread, "id").and_then(|id| self.agent(id)) {
                            let start = self.output.claims.len();
                            self.announce_agent(&agent, thread, true);
                            self.output.state.governing_agent = Some(agent);
                            for claim in &mut self.output.claims[start..] {
                                claim.evidence.push(request.clone());
                            }
                        }
                    }
                } else {
                    self.output.diagnostics.push(MappingDiagnostic {
                        evidence: vec![self.input.canonical_observation.clone()],
                        code: DiagnosticCode::MissingSource,
                        message: "RPC reply has no retained request in this source incarnation"
                            .into(),
                    });
                }
            }
            return;
        }
        let method = string(value, "method").unwrap_or("");
        let params = &value["params"];
        if method == "thread/started" {
            if let Some(agent) = string(&params["thread"], "id").and_then(|id| self.agent(id)) {
                self.announce_agent(&agent, &params["thread"], true);
            }
            return;
        }
        let Some(owner) = string(params, "threadId").and_then(|id| self.agent(id)) else {
            if let Some(scope) = self.input.capture.declared_subject_refs.first() {
                self.issue(scope,"codex/live-owner-missing",DiagnosticCode::MissingOwner,"The live frame does not establish an emitting thread; retained request correlation is required");
            }
            return;
        };
        let activation = string(params, "turnId").map(|id| activation(&owner, id));
        match method {
            "turn/started" | "turn/completed" => {
                let turn = &params["turn"];
                let Some(id) = string(turn, "id") else {
                    self.issue(
                        &owner,
                        "codex/live-turn-id",
                        DiagnosticCode::MissingOwner,
                        "Live turn boundary has no turn ID",
                    );
                    return;
                };
                let reference = activation_ref(&owner, id);
                self.declare(
                    &owner,
                    "codex/activation",
                    TraceObject::Activation(Activation {
                        activation_ref: reference.clone(),
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                    }),
                );
                if method == "turn/started" {
                    self.state
                        .active_by_agent
                        .insert(owner.encode(), reference.clone());
                    self.add(
                        &owner,
                        "codex/activation-start",
                        Fact::Execution {
                            execution: ExecutionRef::Activation(reference),
                            boundary: ExecutionBoundary::Started {
                                time: time(turn, "startedAt", TimeUnit::Seconds, &owner),
                            },
                        },
                    );
                } else {
                    let outcome = if turn.get("error").is_some_and(|error| !error.is_null()) {
                        TerminalOutcome::Failed
                    } else {
                        match string(turn, "status") {
                            Some("completed") => TerminalOutcome::Succeeded,
                            Some("failed") => TerminalOutcome::Failed,
                            Some("interrupted") => TerminalOutcome::Interrupted,
                            _ => TerminalOutcome::Unknown,
                        }
                    };
                    if let Some(start) = time(turn, "startedAt", TimeUnit::Seconds, &owner) {
                        self.add(
                            &owner,
                            "codex/reported-activation-start",
                            Fact::Execution {
                                execution: ExecutionRef::Activation(reference.clone()),
                                boundary: ExecutionBoundary::Started { time: Some(start) },
                            },
                        );
                    }
                    self.close(
                        &owner,
                        ExecutionRef::Activation(reference.clone()),
                        outcome,
                        time(turn, "completedAt", TimeUnit::Seconds, &owner),
                        turn.get("error"),
                    );
                    if let Some(duration) = turn.get("durationMs").and_then(Value::as_number) {
                        self.duration(
                            &owner,
                            AnyObjectRef::Activation(reference.clone()),
                            duration.clone(),
                        );
                    }
                    if self.state.active_by_agent.get(&owner.encode()) == Some(&reference) {
                        self.state.active_by_agent.remove(&owner.encode());
                    }
                }
            }
            "item/started" | "item/completed" => self.item(
                &owner,
                &params["item"],
                true,
                method == "item/started",
                activation,
                params,
            ),
            "item/agentMessage/delta" => {
                let Some(id) = string(params, "itemId") else {
                    return;
                };
                let Some(delta) = string(params, "delta") else {
                    self.issue(
                        &owner,
                        "codex/delta-shape",
                        DiagnosticCode::DecodeFailure,
                        "Agent message delta is not text",
                    );
                    return;
                };
                let synthetic = serde_json::json!({"id":id,"role":"assistant","content":[{"type":"text","text":delta}]});
                let message =
                    MessageRef::native(owner.clone(), "codex.message", vec![id.into()]).unwrap();
                let predecessor = self
                    .state
                    .revision_by_message
                    .get(&message.encode())
                    .cloned();
                if let Some(revision) = self.message(
                    &owner,
                    &synthetic,
                    "/params/delta",
                    activation,
                    RevisionForm::Append { predecessor },
                ) {
                    self.history(&owner, vec![HistoryContent::Message(revision)], None);
                }
            }
            "thread/goal/updated" => self.goal_update(&owner, params),
            "thread/tokenUsage/updated" => self.token_count(&owner, &params["tokenUsage"], true),
            _ => self.issue(
                &owner,
                "codex/unsupported-live",
                DiagnosticCode::UnsupportedKind,
                &format!("Retained app-server method {method:?} has no declared semantic mapping"),
            ),
        }
    }

    fn item(
        &mut self,
        owner: &AgentRef,
        item: &Value,
        live: bool,
        started: bool,
        activation: Option<ActivationRef>,
        envelope: &Value,
    ) {
        let kind = string(item, "type").unwrap_or("");
        match kind {
            "userMessage" | "UserMessage" | "agentMessage" | "AgentMessage" => {
                let mut content = item.clone();
                if let Some(object) = content.as_object_mut() {
                    object.insert(
                        "role".into(),
                        Value::String(
                            if kind == "userMessage" || kind == "UserMessage" {
                                "user"
                            } else {
                                "assistant"
                            }
                            .into(),
                        ),
                    );
                    if let Some(text) = string(item, "text") {
                        object.insert(
                            "content".into(),
                            serde_json::json!([{"type":"text","text":text}]),
                        );
                    }
                }
                let form = if started {
                    RevisionForm::Replacement { covers: None }
                } else {
                    RevisionForm::Final {
                        covers: Some(SourcePrefix {
                            through: self.input.capture.source_position.clone(),
                        }),
                    }
                };
                if let Some(revision) = self.message(
                    owner,
                    &content,
                    if live {
                        "/params/item"
                    } else {
                        "/payload/item"
                    },
                    activation,
                    form,
                ) {
                    self.history(owner, vec![HistoryContent::Message(revision)], None);
                }
            }
            "collabAgentToolCall" | "CollabAgentToolCall" => {
                self.collaboration_item(owner, item, live, started, activation)
            }
            "subAgentActivity" | "SubAgentActivity" => {
                let child = string(
                    item,
                    if live {
                        "agentThreadId"
                    } else {
                        "agent_thread_id"
                    },
                )
                .and_then(|id| self.agent(id));
                if let Some(child) = child {
                    self.declare(
                        owner,
                        "codex/activity-agent",
                        TraceObject::Agent(AgentThread {
                            agent_ref: child.clone(),
                            vendor: AgentName::parse("codex").unwrap(),
                            name: None,
                            model: None,
                        }),
                    );
                    // Activity proves which agent it concerns, not a child run or a
                    // spawn parent; the actual header/spawn result supplies that.
                    self.add(
                        owner,
                        "codex/activity-interaction",
                        Fact::Relation {
                            relation: Relation::Interaction {
                                sender: owner.clone(),
                                recipient: child,
                                delivery: None,
                                operation: None,
                            },
                        },
                    );
                } else {
                    self.issue(
                        owner,
                        "codex/activity-target-missing",
                        DiagnosticCode::MissingOwner,
                        "Activity ID is not a native child agent ID",
                    );
                }
            }
            "reasoning" | "Reasoning" => self.reasoning(
                owner,
                item,
                if live {
                    "/params/item"
                } else {
                    "/payload/item"
                },
            ),
            "commandExecution" | "CommandExecution" | "fileChange" | "FileChange" => {
                self.runtime_operation(owner, item, live, started, activation, envelope)
            }
            "contextCompaction" | "ContextCompaction" => self.issue(
                owner,
                "codex/compaction-announcement",
                DiagnosticCode::CaptureGap,
                "Compaction announcement does not contain the replacement history",
            ),
            _ => self.issue(
                owner,
                "codex/unsupported-item",
                DiagnosticCode::UnsupportedKind,
                &format!("Retained item kind {kind:?} has no declared semantic mapping"),
            ),
        }
    }

    fn message(
        &mut self,
        owner: &AgentRef,
        value: &Value,
        selector: &str,
        activation: Option<ActivationRef>,
        form: RevisionForm,
    ) -> Option<MessageRevisionRef> {
        let message = match string(value, "id") {
            Some(id) => {
                MessageRef::native(owner.clone(), "codex.message", vec![id.into()]).unwrap()
            }
            None => MessageRef::occurrence(
                owner.clone(),
                self.input.capture.source_position.clone(),
                vec![selector.into()],
            )
            .unwrap(),
        };
        let revision = MessageRevisionRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec![selector.into(), "revision".into()],
        )
        .unwrap();
        let role = match string(value, "role") {
            Some("assistant") => Some(ProtocolRole::Assistant),
            Some("user") => Some(ProtocolRole::User),
            Some("developer") => Some(ProtocolRole::Developer),
            Some("system") => Some(ProtocolRole::System),
            Some("tool") => Some(ProtocolRole::Tool),
            _ => None,
        };
        let author = match role {
            Some(ProtocolRole::Assistant) => Author::Agent {
                agent: OwnerRef::Agent {
                    agent: owner.clone(),
                },
            },
            Some(ProtocolRole::Developer | ProtocolRole::System) => Author::Harness {
                identity: Some("codex".into()),
            },
            _ => Author::Unknown,
        };
        let purpose = match string(value, "phase").or_else(|| string(value, "channel")) {
            Some("final_answer" | "final") => MessagePurpose::Answer,
            Some("commentary" | "analysis") => MessagePurpose::Progress,
            _ => {
                if matches!(role, Some(ProtocolRole::System | ProtocolRole::Developer)) {
                    MessagePurpose::Context
                } else {
                    MessagePurpose::Unclassified
                }
            }
        };
        self.declare(
            owner,
            "codex/message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author,
                protocol_role: role,
                purpose,
                production: if role == Some(ProtocolRole::Assistant) {
                    Some(ProductionScope {
                        owner: OwnerRef::Agent {
                            agent: owner.clone(),
                        },
                        activation,
                    })
                } else {
                    None
                },
            }),
        );
        let mut parts = vec![];
        if let Some(content) = value.get("content") {
            let values = if let Some(content) = content.as_array() {
                content.clone()
            } else if content.is_string() {
                vec![serde_json::json!({"type":"text","text":content})]
            } else {
                vec![content.clone()]
            };
            for (index, part) in values.iter().enumerate() {
                let kind = match string(part, "type") {
                    Some("text" | "Text" | "input_text" | "output_text") => ContentKind::Text,
                    Some("image" | "Image" | "input_image") => ContentKind::Image,
                    Some("audio" | "input_audio") => ContentKind::Audio,
                    Some("video") => ContentKind::Video,
                    _ => ContentKind::Opaque,
                };
                let pointer = format!("{selector}/content/{index}");
                let payload = if kind == ContentKind::Text {
                    match string(part, "text") {
                        Some(text) => {
                            self.content(text.as_bytes().to_vec(), &format!("{pointer}/text"))
                        }
                        None => {
                            self.issue(
                                owner,
                                &format!("codex/part-shape/{index}"),
                                DiagnosticCode::DecodeFailure,
                                "A text content part lacks its text field",
                            );
                            self.opaque(part, &pointer)
                        }
                    }
                } else {
                    self.opaque(part, &pointer)
                };
                if kind == ContentKind::Opaque {
                    self.issue(
                        owner,
                        &format!("codex/unknown-part/{index}"),
                        DiagnosticCode::UnsupportedKind,
                        "Unknown content part is retained as opaque JSON",
                    );
                }
                parts.push(ContentPart {
                    media_type: Some(
                        if kind == ContentKind::Text {
                            "text/plain"
                        } else {
                            "application/json"
                        }
                        .into(),
                    ),
                    encoding: PayloadEncoding::Utf8,
                    part_ref: if let Some(id) = string(value, "id") {
                        MessagePartRef::native(
                            owner.clone(),
                            "codex.message-part",
                            vec![id.into(), index.to_string()],
                        )
                        .unwrap()
                    } else {
                        MessagePartRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec![selector.into(), index.to_string()],
                        )
                        .unwrap()
                    },
                    kind,
                    payload,
                });
            }
        } else {
            self.issue(
                owner,
                "codex/content-missing",
                DiagnosticCode::CaptureGap,
                "The message occurrence omits content",
            );
        }
        self.declare(
            owner,
            "codex/message-revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message: message.clone(),
                representation: Representation::Conversation,
                form: form.clone(),
                parts,
            }),
        );
        if self.input.capture.transport_kind == TransportKind::LiveStdout {
            if matches!(form, RevisionForm::Final { .. }) {
                self.state.revision_by_message.remove(&message.encode());
            } else {
                self.state
                    .revision_by_message
                    .insert(message.encode(), revision.clone());
            }
        }
        Some(revision)
    }

    fn history(
        &mut self,
        owner: &AgentRef,
        content: Vec<HistoryContent>,
        original_owner: Option<OwnerRef>,
    ) {
        let entry = EntryRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["history".into()],
        )
        .unwrap();
        let predecessor = self.state.entry_by_agent.get(&owner.encode()).cloned();
        self.declare(
            owner,
            "codex/history-entry",
            TraceObject::HistoryEntry(HistoryEntry {
                entry_ref: entry.clone(),
                containing_agent: owner.clone(),
                predecessor: predecessor.clone(),
                original_owner,
            }),
        );
        self.add(
            owner,
            "codex/history-content",
            Fact::HistoryContentObserved {
                observation: HistoryContentObservation::complete(
                    entry.clone(),
                    self.input.capture.source_position.clone(),
                    if self.input.capture.transport_kind == TransportKind::NativeJsonl {
                        "codex/native-history"
                    } else {
                        "codex/live-history"
                    },
                    content,
                ),
            },
        );
        if self.input.capture.transport_kind == TransportKind::NativeJsonl {
            if let SourceLocator::Jsonl { record_ordinal, .. } =
                self.input.capture.source_position.locator
            {
                self.add(
                    owner,
                    "codex/history-local-position",
                    Fact::HistoryPosition {
                        entry: entry.clone(),
                        position: self.input.capture.source_position.clone(),
                        coordinate: HistoryCoordinate::LocalRecordIndex,
                        value: record_ordinal,
                        mapping_rule: "codex/native-history".into(),
                    },
                );
            }
            if let Some(ordinal) = self.vendor_ordinal {
                self.add(
                    owner,
                    "codex/history-vendor-position",
                    Fact::HistoryPosition {
                        entry: entry.clone(),
                        position: self.input.capture.source_position.clone(),
                        coordinate: HistoryCoordinate::VendorOrdinal {
                            domain_ref: self.input.capture.source_position.source.clone(),
                        },
                        value: ordinal,
                        mapping_rule: "codex/native-history".into(),
                    },
                );
            }
        }
        if let Some(predecessor) = predecessor {
            self.add(
                owner,
                "codex/history-order",
                Fact::Relation {
                    relation: Relation::HistoryPredecessor {
                        predecessor,
                        entry: entry.clone(),
                    },
                },
            );
        }
        self.state
            .entry_by_agent
            .insert(owner.encode(), entry.clone());
        self.output.state.previous_entry = Some(entry);
    }

    fn communication(&mut self, containing: &AgentRef, value: &Value) {
        let root = self
            .state
            .accounting_root
            .clone()
            .unwrap_or(containing.clone());
        let resolve = |path: &str| -> OwnerRef {
            if self.state.agent_path.as_deref() == Some(path) {
                return OwnerRef::Agent {
                    agent: containing.clone(),
                };
            }
            let key = correlation_key(&root, "codex.agent-path", &[path.into()]);
            match self.input.archive.correlations.get(&key) {
                Some(AnyObjectRef::Agent(agent)) => OwnerRef::Agent {
                    agent: agent.clone(),
                },
                _ => OwnerRef::Unresolved {
                    scope: root.clone(),
                    namespace: "codex.agent-path".into(),
                    native_key: vec![path.into()],
                    reason: "Agent path requires a retained root-scoped identity binding".into(),
                },
            }
        };
        let author = string(value, "author").map(resolve);
        let recipient = string(value, "recipient").map(resolve);
        for path in [string(value, "author"), string(value, "recipient")]
            .into_iter()
            .flatten()
        {
            if self.state.agent_path.as_deref() != Some(path)
                && !self
                    .input
                    .archive
                    .correlations
                    .contains_key(&correlation_key(&root, "codex.agent-path", &[path.into()]))
            {
                self.output.requests.push(ArchiveRequest::Correlation {
                    agent: root.clone(),
                    namespace: "codex.agent-path".into(),
                    key: vec![path.into()],
                });
            }
        }
        let owner = match &author {
            Some(OwnerRef::Agent { agent }) => agent.clone(),
            _ => containing.clone(),
        };
        let Some(revision) = self.message(&owner, value, "/payload", None, RevisionForm::Complete)
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
            author,
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
                recipient: recipient.map_or(Recipient::Unknown, |agent| Recipient::Agent { agent }),
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
    }

    fn model_call(&mut self, owner: &AgentRef, value: &Value, kind: &str) {
        let Some(id) = string(value, "call_id") else {
            self.issue(
                owner,
                "codex/call-id-missing",
                DiagnosticCode::MissingOwner,
                "Model invocation has no scoped call ID",
            );
            return;
        };
        let reference =
            OperationRef::native(owner.clone(), "codex.model-call", vec![id.into()]).unwrap();
        let key = if kind == "custom_tool_call" {
            "input"
        } else {
            "arguments"
        };
        let payload = string(value, key)
            .map(|text| self.content(text.as_bytes().to_vec(), &format!("/payload/{key}")));
        if payload.is_none() {
            self.issue(
                owner,
                "codex/call-input-missing",
                DiagnosticCode::DecodeFailure,
                "Model invocation input is missing or not a lexical string",
            );
        }
        if kind == "function_call"
            && string(value, key).is_some_and(|input| serde_json::from_str::<Value>(input).is_err())
        {
            self.issue(
                owner,
                "codex/call-arguments-undecodable",
                DiagnosticCode::DecodeFailure,
                "Function arguments are retained exactly but are not valid JSON",
            );
        }
        let namespace = string(value, "namespace");
        let Some(name) = string(value, "name") else {
            self.issue(
                owner,
                "codex/tool-name-missing",
                DiagnosticCode::DecodeFailure,
                "Model invocation does not name its tool",
            );
            return;
        };
        let category = match (namespace, name) {
            (Some("collaboration"), "spawn_agent") => OperationCategory::Delegation,
            (
                Some("collaboration"),
                "send_message" | "followup_task" | "interrupt_agent" | "close_agent",
            ) => OperationCategory::Communication,
            (Some("clock"), "sleep") | (Some("collaboration"), "wait_agent") => {
                OperationCategory::Wait
            }
            _ => OperationCategory::Tool {
                namespace: namespace.map(str::to_owned),
                name: name.into(),
            },
        };
        let activation = self.state.active_by_agent.get(&owner.encode()).cloned();
        self.declare(
            owner,
            "codex/model-operation",
            TraceObject::Operation(Operation {
                operation_ref: reference.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation,
                requester: Some(RequesterRef::Agent(owner.clone())),
                category,
                input_revision: payload,
                argument_state: ArgumentState::Sealed,
            }),
        );
        self.history(owner, vec![HistoryContent::Operation(reference)], None);
    }

    fn model_result(&mut self, owner: &AgentRef, value: &Value) {
        let Some(id) = string(value, "call_id") else {
            self.issue(
                owner,
                "codex/result-call-id-missing",
                DiagnosticCode::MissingOwner,
                "Model result has no scoped invocation reference",
            );
            return;
        };
        let operation =
            OperationRef::native(owner.clone(), "codex.model-call", vec![id.into()]).unwrap();
        let payload = value.get("output").map(|value| {
            if let Some(text) = value.as_str() {
                self.content(text.as_bytes().to_vec(), "/payload/output")
            } else {
                self.opaque(value, "/payload/output")
            }
        });
        let revision = MessageRevisionRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["tool-result".into(), "revision".into()],
        )
        .unwrap();
        let message = MessageRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["tool-result".into()],
        )
        .unwrap();
        let delivery = DeliveryRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["tool-result".into()],
        )
        .unwrap();
        let author = Author::Tool {
            operation: Some(operation.clone()),
        };
        self.declare(
            owner,
            "codex/tool-result-message",
            TraceObject::Message(Message {
                message_ref: message.clone(),
                author: author.clone(),
                protocol_role: Some(ProtocolRole::Tool),
                purpose: MessagePurpose::Unclassified,
                production: None,
            }),
        );
        self.declare(
            owner,
            "codex/tool-result-revision",
            TraceObject::MessageRevision(MessageRevision {
                revision_ref: revision.clone(),
                message,
                representation: Representation::CallerResult,
                form: RevisionForm::Complete,
                parts: payload
                    .as_ref()
                    .map(|payload| ContentPart {
                        media_type: Some(
                            if value["output"].is_string() {
                                "text/plain"
                            } else {
                                "application/json"
                            }
                            .into(),
                        ),
                        encoding: PayloadEncoding::Utf8,
                        part_ref: MessagePartRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec!["tool-result".into(), "0".into()],
                        )
                        .unwrap(),
                        kind: if value["output"].is_string() {
                            ContentKind::Text
                        } else {
                            ContentKind::Structured
                        },
                        payload: payload.clone(),
                    })
                    .into_iter()
                    .collect(),
            }),
        );
        self.history(owner, vec![HistoryContent::Message(revision.clone())], None);
        self.declare(
            owner,
            "codex/tool-result-delivery",
            TraceObject::Delivery(Delivery {
                delivery_ref: delivery.clone(),
                sender: author,
                recipient: Recipient::Agent {
                    agent: OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                },
                route: DeliveryRoute::ToolReturn {
                    operation: operation.clone(),
                },
            }),
        );
        self.add(
            owner,
            "codex/tool-result-delivery-revision",
            Fact::DeliveryRevisionObserved {
                delivery: delivery.clone(),
                revision,
            },
        );
        self.add(
            owner,
            "codex/tool-result-admitted",
            Fact::DeliveryStage {
                delivery: delivery.clone(),
                stage: DeliveryStage::Admitted {
                    entry: self.output.state.previous_entry.clone(),
                },
            },
        );
        self.declare(
            owner,
            "codex/caller-result",
            TraceObject::Result(ResultOccurrence {
                result_ref: ResultRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["caller-result".into()],
                )
                .unwrap(),
                operation,
                recipient: Some(Recipient::Agent {
                    agent: OwnerRef::Agent {
                        agent: owner.clone(),
                    },
                }),
                disposition: ResultDisposition::FinalReturn,
                representation: Representation::CallerResult,
                payload,
                delivery: Some(delivery),
            }),
        );
    }

    fn runtime_operation(
        &mut self,
        owner: &AgentRef,
        item: &Value,
        live: bool,
        started: bool,
        activation: Option<ActivationRef>,
        envelope: &Value,
    ) {
        let Some(id) = string(item, "id") else {
            self.issue(
                owner,
                "codex/runtime-id-missing",
                DiagnosticCode::MissingOwner,
                "Runtime item lacks its own invocation ID",
            );
            return;
        };
        let reference =
            OperationRef::native(owner.clone(), "codex.runtime-item", vec![id.into()]).unwrap();
        let is_command = matches!(
            string(item, "type"),
            Some("commandExecution" | "CommandExecution")
        );
        let input = if is_command {
            string(item, "command").map(|text| {
                self.content(
                    text.as_bytes().to_vec(),
                    if live {
                        "/params/item/command"
                    } else {
                        "/payload/item/command"
                    },
                )
            })
        } else {
            Some(self.opaque(&item["changes"], "/item/changes"))
        };
        self.declare(
            owner,
            "codex/runtime-operation",
            TraceObject::Operation(Operation {
                operation_ref: reference.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                activation,
                requester: None,
                category: if is_command {
                    OperationCategory::TerminalIo {
                        action: TerminalAction::Launch,
                    }
                } else {
                    OperationCategory::Edit
                },
                input_revision: input,
                argument_state: ArgumentState::Sealed,
            }),
        );
        let resource = if is_command {
            string(item, if live { "processId" } else { "process_id" }).map(|handle| {
                let resource = ResourceRef::native(
                    owner.clone(),
                    "codex.process-creation",
                    vec![id.into(), handle.into()],
                )
                .unwrap();
                self.declare(
                    owner,
                    "codex/process-resource",
                    TraceObject::Resource(Resource {
                        resource_ref: resource.clone(),
                        creator: Some(reference.clone()),
                        host: Some(OwnerRef::Agent {
                            agent: owner.clone(),
                        }),
                        kind: ResourceKind::Terminal {
                            native_handle: handle.into(),
                            incarnation: reference.encode(),
                        },
                    }),
                );
                self.add(
                    owner,
                    "codex/process-use",
                    Fact::Relation {
                        relation: Relation::UsesResource {
                            operation: reference.clone(),
                            resource: resource.clone(),
                        },
                    },
                );
                resource
            })
        } else {
            None
        };
        let start = time(
            envelope,
            if live { "startedAtMs" } else { "started_at_ms" },
            TimeUnit::Milliseconds,
            owner,
        );
        if start.is_some() {
            self.add(
                owner,
                "codex/runtime-start",
                Fact::Execution {
                    execution: ExecutionRef::Operation(reference.clone()),
                    boundary: ExecutionBoundary::Started { time: start },
                },
            );
        }
        if started {
            if let Some(resource) = resource.clone() {
                self.add(
                    owner,
                    "codex/process-running",
                    Fact::ResourceState {
                        resource,
                        state: ResourceState::Running,
                    },
                );
            }
        }
        if !started {
            let key = if live {
                "aggregatedOutput"
            } else {
                "aggregated_output"
            };
            if let Some(text) = string(item, key) {
                let payload = self.content(text.as_bytes().to_vec(), &format!("/item/{key}"));
                self.declare(
                    owner,
                    "codex/runtime-result",
                    TraceObject::Result(ResultOccurrence {
                        result_ref: ResultRef::occurrence(
                            owner.clone(),
                            self.input.capture.source_position.clone(),
                            vec!["runtime-result".into()],
                        )
                        .unwrap(),
                        operation: reference.clone(),
                        recipient: None,
                        disposition: ResultDisposition::FinalReturn,
                        representation: Representation::Runtime,
                        payload: Some(payload),
                        delivery: None,
                    }),
                );
            }
            for channel in ["stdout", "stderr", "formatted_output"] {
                if let Some(text) = string(item, channel) {
                    let payload =
                        self.content(text.as_bytes().to_vec(), &format!("/item/{channel}"));
                    self.declare(
                        owner,
                        &format!("codex/runtime-result/{channel}"),
                        TraceObject::Result(ResultOccurrence {
                            result_ref: ResultRef::occurrence(
                                owner.clone(),
                                self.input.capture.source_position.clone(),
                                vec!["runtime-result".into(), channel.into()],
                            )
                            .unwrap(),
                            operation: reference.clone(),
                            recipient: None,
                            disposition: ResultDisposition::FinalReturn,
                            representation: Representation::Runtime,
                            payload: Some(payload),
                            delivery: None,
                        }),
                    );
                }
            }
            let outcome = match string(item, "status") {
                Some("failed") => TerminalOutcome::Failed,
                Some("completed") => {
                    if item
                        .get(if live { "exitCode" } else { "exit_code" })
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0)
                    {
                        TerminalOutcome::Failed
                    } else {
                        TerminalOutcome::Succeeded
                    }
                }
                Some("declined") => TerminalOutcome::Cancelled,
                _ => TerminalOutcome::Unknown,
            };
            self.close(
                owner,
                ExecutionRef::Operation(reference.clone()),
                outcome,
                time(
                    envelope,
                    if live {
                        "completedAtMs"
                    } else {
                        "completed_at_ms"
                    },
                    TimeUnit::Milliseconds,
                    owner,
                ),
                None,
            );
            if let Some(resource) = resource {
                let state = if matches!(
                    string(item, "status"),
                    Some("completed" | "failed" | "declined")
                ) {
                    ResourceState::Closed {
                        outcome,
                        reason: Some(ExecutionCloseReason::VendorStop),
                    }
                } else {
                    ResourceState::Unknown {
                        reason: "Runtime return did not establish process termination".into(),
                    }
                };
                self.add(
                    owner,
                    "codex/process-state",
                    Fact::ResourceState { resource, state },
                );
            }
            self.history(owner, vec![HistoryContent::Operation(reference)], None);
        }
    }

    fn reasoning(&mut self, owner: &AgentRef, value: &Value, selector: &str) {
        // Ciphertext/signatures and disclosed summaries retain distinct named fields.
        let body = self.opaque(value, selector);
        self.history(owner, vec![HistoryContent::Opaque(body)], None);
    }

    fn compaction(&mut self, owner: &AgentRef, value: &Value) {
        let Some(replacement) = value.get("replacement_history") else {
            self.issue(
                owner,
                "codex/replacement-missing",
                DiagnosticCode::CaptureGap,
                "Compaction omitted its replacement history",
            );
            return;
        };
        let payload = self.opaque(replacement, "/payload/replacement_history");
        let checkpoint = CheckpointRef::occurrence(
            owner.clone(),
            self.input.capture.source_position.clone(),
            vec!["compaction".into()],
        )
        .unwrap();
        self.declare(
            owner,
            "codex/context-replacement",
            TraceObject::ContextCheckpoint(ContextCheckpoint {
                checkpoint_ref: checkpoint.clone(),
                owner: OwnerRef::Agent {
                    agent: owner.clone(),
                },
                basis: ContextBasis::NativeReplacement,
                content: ContextContent::OpaqueReplacement { payload },
                coverage: InclusionCoverage::Unknown {
                    reason: "Native replacement is retained; no exact model request was captured"
                        .into(),
                },
            }),
        );
        self.history(owner, vec![HistoryContent::Checkpoint(checkpoint)], None);
    }

    fn close(
        &mut self,
        owner: &AgentRef,
        execution: ExecutionRef,
        outcome: TerminalOutcome,
        time: Option<TimeEvidence>,
        error: Option<&Value>,
    ) {
        let detail = error
            .filter(|error| !error.is_null())
            .map(|error| self.opaque(error, "/error"));
        self.add(
            owner,
            "codex/execution-terminated",
            Fact::Execution {
                execution: execution.clone(),
                boundary: ExecutionBoundary::Terminated {
                    time,
                    reason: Some(ExecutionCloseReason::VendorStop),
                },
            },
        );
        self.add(
            owner,
            "codex/execution-outcome",
            Fact::Execution {
                execution,
                boundary: ExecutionBoundary::Outcome {
                    outcome,
                    reason: Some(ExecutionCloseReason::VendorStop),
                    detail,
                },
            },
        );
    }

    fn duration(&mut self, owner: &AgentRef, subject: AnyObjectRef, value: serde_json::Number) {
        self.declare(
            owner,
            "codex/reported-duration",
            TraceObject::Measurement(Measurement {
                measurement_ref: MeasurementRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["reported-duration".into()],
                )
                .unwrap(),
                subject: subject.clone(),
                metric: Metric::ReportedDuration,
                unit: MeasurementUnit::Milliseconds,
                value,
                mode: MeasurementMode::Interval {
                    interval_ref: subject,
                },
                aggregation_scope: AggregationScope::NamedSubject,
                components: vec![],
            }),
        );
    }

    fn token_count(&mut self, owner: &AgentRef, value: &Value, live: bool) {
        if value.is_null() {
            return;
        }
        let total = &value[if live { "total" } else { "total_token_usage" }];
        self.tokens(owner, total, live, None);
    }

    fn response_usage(&mut self, owner: &AgentRef, value: &Value) {
        let reported_owner = string(value, "thread_id")
            .and_then(|id| self.agent(id))
            .unwrap_or(owner.clone());
        let Some(response) = string(value, "response_id") else {
            self.issue(
                owner,
                "codex/response-id-missing",
                DiagnosticCode::MissingOwner,
                "Response accounting omits its response ID",
            );
            return;
        };
        self.tokens(&reported_owner, &value["usage"], false, Some(response));
    }

    fn tokens(&mut self, owner: &AgentRef, value: &Value, live: bool, response: Option<&str>) {
        let mut observed = false;
        for (snake, camel, metric) in [
            ("input_tokens", "inputTokens", Metric::InputTokens),
            ("output_tokens", "outputTokens", Metric::OutputTokens),
            (
                "cached_input_tokens",
                "cachedInputTokens",
                Metric::CachedTokens,
            ),
            (
                "reasoning_output_tokens",
                "reasoningOutputTokens",
                Metric::ReasoningTokens,
            ),
            ("total_tokens", "totalTokens", Metric::TotalTokens),
        ] {
            let key = if live { camel } else { snake };
            let Some(number) = value.get(key).and_then(Value::as_number) else {
                continue;
            };
            observed = true;
            let reference = if let Some(response) = response {
                MeasurementRef::native(
                    owner.clone(),
                    "codex.response-usage",
                    vec![response.into(), key.into()],
                )
                .unwrap()
            } else {
                MeasurementRef::occurrence(
                    owner.clone(),
                    self.input.capture.source_position.clone(),
                    vec!["usage".into(), key.into()],
                )
                .unwrap()
            };
            self.declare(
                owner,
                &format!("codex/usage/{key}"),
                TraceObject::Measurement(Measurement {
                    measurement_ref: reference,
                    subject: AnyObjectRef::Agent(owner.clone()),
                    metric,
                    unit: MeasurementUnit::Tokens,
                    value: number.clone(),
                    mode: if response.is_some() {
                        MeasurementMode::Gauge
                    } else {
                        MeasurementMode::Cumulative {
                            counter_domain: Some(correlation_key(
                                owner,
                                "codex.thread-token-count",
                                &[key.into()],
                            )),
                            reset_epoch: None,
                            sample_position: self.input.capture.source_position.clone(),
                        }
                    },
                    aggregation_scope: AggregationScope::NamedSubject,
                    components: vec![],
                }),
            );
        }
        if observed && response.is_none() {
            self.issue(
                owner,
                "codex/unknown-counter-baseline",
                DiagnosticCode::UnknownCounterBaseline,
                "The thread reports cumulative usage without an evidenced reset epoch or baseline",
            );
        }
    }
}

fn activation(owner: &AgentRef, id: &str) -> ActivationRef {
    activation_ref(owner, id)
}
fn activation_ref(owner: &AgentRef, id: &str) -> ActivationRef {
    ActivationRef::native(owner.clone(), "codex.turn", vec![id.into()]).unwrap()
}
fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn time(value: &Value, key: &str, unit: TimeUnit, owner: &AgentRef) -> Option<TimeEvidence> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .map(|value| TimeEvidence {
            domain: format!("codex-wall:{}", owner.environment()),
            value,
            unit,
            precision: Some(1),
            basis: TimeBasis::SourceWall,
        })
}
fn diagnostic(scope: &AgentRef, code: DiagnosticCode, message: &str) -> Fact {
    Fact::Diagnostic {
        diagnostic: TraceDiagnostic {
            subject: Some(AnyObjectRef::Agent(scope.clone())),
            code,
            message: message.into(),
            related_claims: vec![],
        },
    }
}

#[cfg(test)]
mod tests;
