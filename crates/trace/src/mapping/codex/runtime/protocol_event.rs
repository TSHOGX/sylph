//! Raw trace wraps serialized EventMsg bytes without changing their native spelling.
//! The wrapper's event_type is a separate discriminator; both must agree before
//! a payload can contribute execution facts.
use super::*;

impl Context<'_, '_> {
    pub(super) fn protocol_boundary(
        &mut self,
        owner: &AgentRef,
        envelope: &Value,
        wrapper: &Value,
    ) {
        let Some(event) = self.raw_payload_value("event_payload") else {
            self.issue(
                owner,
                "codex/protocol-event-decode",
                DiagnosticCode::DecodeFailure,
                "Captured protocol event is not a JSON value",
            );
            return;
        };
        let expected = match string(wrapper, "event_type") {
            Some("turn_started") => "task_started",
            Some("turn_complete") => "task_complete",
            Some("turn_aborted") => "turn_aborted",
            _ => {
                self.issue(
                    owner,
                    "codex/protocol-event-kind",
                    DiagnosticCode::UnsupportedKind,
                    "Retained protocol breadcrumb has no supported semantic boundary mapping",
                );
                return;
            }
        };
        if string(&event, "type") != Some(expected) {
            self.issue(
                owner,
                "codex/protocol-event-type",
                DiagnosticCode::DecodeFailure,
                "Protocol breadcrumb type differs from its retained EventMsg discriminator",
            );
            return;
        }
        let Some(turn) = string(&event, "turn_id").filter(|id| !id.is_empty()) else {
            self.issue(
                owner,
                "codex/protocol-event-turn",
                DiagnosticCode::MissingOwner,
                "Protocol execution boundary lacks its own native turn identity",
            );
            return;
        };
        if string(envelope, "codex_turn_id").is_some_and(|id| id != turn)
            || string(&event, "thread_id").is_some_and(|id| self.agent(id).as_ref() != Some(owner))
        {
            self.issue(
                owner,
                "codex/protocol-event-scope",
                DiagnosticCode::ConflictingOwner,
                "Protocol execution boundary differs from its explicit captured thread or turn",
            );
            return;
        }
        let reason = if expected == "turn_aborted" {
            match string(&event, "reason") {
                Some(reason @ ("interrupted" | "replaced" | "review_ended" | "budget_limited")) => {
                    Some(reason)
                }
                _ => {
                    self.issue(
                        owner,
                        "codex/protocol-event-abort-reason",
                        DiagnosticCode::UnsupportedKind,
                        "Protocol abort has no supported native reason",
                    );
                    return;
                }
            }
        } else {
            None
        };
        let detail = reason.and_then(|_| {
            self.raw_payload(
                owner,
                &wrapper["event_payload"],
                "event_payload",
                AnyObjectRef::Activation(super::super::activation(owner, turn)),
            )
        });
        let first_claim = self.output.claims.len();
        let first_payload = self.output.payloads.len();
        self.native_boundary(owner, expected, &event);
        if let Some(reason) = reason {
            for claim in &mut self.output.claims[first_claim..] {
                match &mut claim.fact {
                    Fact::Execution {
                        boundary:
                            ExecutionBoundary::Outcome {
                                detail: observed,
                                reason: closed,
                                ..
                            },
                        ..
                    } => {
                        *observed = detail.as_ref().map(|payload| payload.reference.clone());
                        *closed = Some(ExecutionCloseReason::Extension {
                            namespace: "codex.turn-abort".into(),
                            code: reason.into(),
                        });
                    }
                    Fact::Execution {
                        boundary: ExecutionBoundary::Terminated { reason: closed, .. },
                        ..
                    } => {
                        *closed = Some(ExecutionCloseReason::Extension {
                            namespace: "codex.turn-abort".into(),
                            code: reason.into(),
                        });
                    }
                    _ => {}
                }
            }
        }
        let key = super::super::super::captured_payload_key(
            self.input.canonical_observation,
            "/payload/event_payload",
        );
        if let Some(captured) = self.input.archive.captured_payloads.get(&key) {
            for payload in &mut self.output.payloads[first_payload..] {
                payload.evidence.extend(
                    captured
                        .bindings
                        .iter()
                        .flat_map(|binding| binding.evidence.iter().cloned()),
                );
                payload.evidence.sort();
                payload.evidence.dedup();
                payload.selector = format!("/payload/event_payload{}", payload.selector);
                payload.rule = "codex/captured-protocol-event-field".into();
            }
        }
    }
}
