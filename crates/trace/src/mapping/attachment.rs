//! Late sidecar acquisition is evidence of another representation, never a raw rewrite.
use super::{claim, ArchiveRequest, MapInput, MapOutput, MappingDiagnostic, SemanticMapper};
use sylph_protocol::{trace::*, SourceAttachmentEvent};

pub struct AttachmentMapper;
impl SemanticMapper for AttachmentMapper {
    fn build(&self) -> &'static str {
        "source-attachment-2"
    }
    fn map(&self, input: MapInput<'_>) -> MapOutput {
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let event: SourceAttachmentEvent = match serde_json::from_slice(input.raw) {
            Ok(event) => event,
            Err(error) => {
                output.diagnostics.push(MappingDiagnostic {
                    evidence: vec![input.canonical_observation.clone()],
                    code: DiagnosticCode::DecodeFailure,
                    message: error.to_string(),
                });
                return output;
            }
        };
        if event.parent_observation.issuer != input.capture.observation_ref.issuer
            || event
                .attachment
                .source_position
                .as_ref()
                .is_some_and(|position| {
                    position.source.issuer != input.capture.observation_ref.issuer
                })
        {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::ConflictingOwner,
                message: "Sidecar binding crosses its acquiring issuer".into(),
            });
            return output;
        }
        if !input.capture.attachments.contains(&event.attachment) {
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::CaptureGap,
                message: "Sidecar event has no matching capture-owned payload declaration".into(),
            });
            return output;
        }
        if !input
            .archive
            .observations
            .contains(&event.parent_observation)
        {
            output.requests.push(ArchiveRequest::Observation {
                observation: event.parent_observation,
            });
            return output;
        }
        output
            .capture_payload_bindings
            .push(super::CapturedPayloadBinding {
                parent: event.parent_observation.clone(),
                slot: event.parent_slot.clone(),
                attachment: event.attachment.clone(),
                evidence: vec![
                    event.parent_observation.clone(),
                    input.canonical_observation.clone(),
                ],
            });
        if event.attachment.relation.starts_with("codex.raw.") {
            return output;
        }
        let key = super::attachment_key(
            &event.parent_observation,
            &event.parent_slot,
            &event.attachment.locator,
        );
        let Some(AnyObjectRef::Operation(operation)) = input.archive.correlations.get(&key) else {
            output.requests.push(ArchiveRequest::AttachmentTarget {
                parent: event.parent_observation,
                slot: event.parent_slot,
                locator: event.attachment.locator,
            });
            output.diagnostics.push(MappingDiagnostic {
                evidence: vec![input.canonical_observation.clone()],
                code: DiagnosticCode::MissingOwner,
                message: "Sidecar awaits its vendor-proven parent invocation".into(),
            });
            return output;
        };
        let operation = operation.clone();
        let scope = operation.scope();
        let payload = event.attachment.payload_ref;
        let state = match &event.attachment.availability {
            AttachmentAvailability::Captured => CoverageState::Complete,
            AttachmentAvailability::Partial { .. } => CoverageState::Gap {
                reason: "Sidecar has only captured ranges".into(),
            },
            other => CoverageState::Unavailable {
                reason: format!("{other:?}"),
            },
        };
        let mut evidence = vec![
            event.parent_observation,
            input.canonical_observation.clone(),
        ];
        evidence.sort();
        evidence.dedup();
        if let Some(payload) = payload {
            // A physical snapshot can be returned by several invocations. The
            // file identifies payload bytes; the operation qualifies the result.
            let result = ResultRef::occurrence(
                scope.clone(),
                event
                    .attachment
                    .source_position
                    .clone()
                    .unwrap_or_else(|| input.capture.source_position.clone()),
                vec![
                    "sidecar".into(),
                    event.attachment.locator.clone(),
                    operation.encode(),
                ],
            )
            .unwrap();
            let mut declaration = claim(
                &input,
                scope,
                "attachment/runtime-result",
                self.build(),
                Fact::Declare {
                    object: TraceObject::Result(ResultOccurrence {
                        result_ref: result,
                        operation: operation.clone(),
                        recipient: None,
                        disposition: ResultDisposition::Unknown,
                        representation: ResultRepresentation::Runtime,
                        payload: Some(payload),
                        delivery: None,
                    }),
                },
            );
            declaration.evidence = evidence.clone();
            output.claims.push(declaration);
        }
        let mut coverage = claim(
            &input,
            scope,
            "attachment/coverage",
            self.build(),
            Fact::Coverage {
                coverage: Coverage {
                    subject: AnyObjectRef::Operation(operation.clone()),
                    source: input.capture.source_position.source.clone(),
                    dimension: CoverageDimension::PayloadRanges,
                    interval: CoverageInterval::Object {
                        object: AnyObjectRef::Operation(operation.clone()),
                    },
                    state,
                },
            },
        );
        coverage.evidence = evidence;
        output.claims.push(coverage);
        output
    }
}

#[cfg(test)]
mod tests;
