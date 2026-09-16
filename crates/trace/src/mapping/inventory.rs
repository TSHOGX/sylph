//! Interpret retained acquisition relationships without treating routing hints as owners.

use sylph_protocol::trace::*;

use super::{
    ArchiveRequest, MapInput, MapOutput, MappingDiagnostic, SemanticMapper, SourceOwner,
    SourceOwnership,
};

pub struct InventoryMapper;

impl SemanticMapper for InventoryMapper {
    fn build(&self) -> &'static str {
        "source-inventory-1"
    }

    fn map(&self, input: MapInput<'_>) -> MapOutput {
        let mut output = MapOutput {
            state: input.state.clone(),
            ..MapOutput::default()
        };
        let definition: SourceDefinition = match serde_json::from_slice(input.raw) {
            Ok(value) => value,
            Err(error) => {
                issue(
                    &input,
                    &mut output,
                    DiagnosticCode::DecodeFailure,
                    &error.to_string(),
                );
                return output;
            }
        };
        if input.capture.transport_kind != TransportKind::SourceInventory
            || definition.locator.is_empty()
            || definition.source.issuer != input.capture.observation_ref.issuer
            || !matches!(&definition.source.issuer,Issuer::Environment(environment) if environment==definition.owner.environment())
        {
            issue(
                &input,
                &mut output,
                DiagnosticCode::ConflictingOwner,
                "Source definition differs from its acquiring environment",
            );
            return output;
        }
        let (mut evidence, rule) = match &definition.basis {
            SourceOwnerBasis::NativeIdentity { observation } => {
                let Some(position) = input.archive.observation_sources.get(observation) else {
                    output.requests.push(ArchiveRequest::Observation {
                        observation: observation.clone(),
                    });
                    return output;
                };
                if position.source != definition.source
                    || position.generation != definition.generation
                {
                    issue(
                        &input,
                        &mut output,
                        DiagnosticCode::ConflictingOwner,
                        "Native identity proof belongs to another source generation",
                    );
                    return output;
                }
                let Some(owner) = input
                    .archive
                    .source_owners
                    .get(&(definition.source.clone(), definition.generation))
                else {
                    output.requests.push(ArchiveRequest::Source {
                        source: definition.source.clone(),
                        generation: definition.generation,
                    });
                    return output;
                };
                if owner.agent != definition.owner || !owner.evidence.contains(observation) {
                    issue(
                        &input,
                        &mut output,
                        DiagnosticCode::ConflictingOwner,
                        "Native source has another proved containing agent",
                    );
                    return output;
                }
                (owner.evidence.clone(), owner.rule.as_str())
            }
            SourceOwnerBasis::CompanionTranscript { source, generation } => {
                let Some(owner) = input
                    .archive
                    .source_owners
                    .get(&(source.clone(), *generation))
                else {
                    output.requests.push(ArchiveRequest::Source {
                        source: source.clone(),
                        generation: *generation,
                    });
                    return output;
                };
                if owner.agent != definition.owner || owner.evidence.is_empty() {
                    issue(
                        &input,
                        &mut output,
                        DiagnosticCode::ConflictingOwner,
                        "Companion source has a different evidenced owner",
                    );
                    return output;
                }
                (owner.evidence.clone(), "source/companion-transcript")
            }
        };
        evidence.push(input.canonical_observation.clone());
        evidence.sort();
        evidence.dedup();
        output.source_definitions.push(super::codex::history::RecordedSource {
            definition:definition.clone(),observation:input.canonical_observation.clone(),
        });
        output.source_owners.push(SourceOwnership {
            source: definition.source,
            generation: definition.generation,
            owner: SourceOwner {
                agent: definition.owner,
                evidence,
                rule: rule.into(),
            },
        });
        output
    }
}

fn issue(input: &MapInput<'_>, output: &mut MapOutput, code: DiagnosticCode, message: &str) {
    output.diagnostics.push(MappingDiagnostic {
        evidence: vec![input.canonical_observation.clone()],
        code,
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{ArchiveContext, MapperState};

    fn fixture() -> (Capture, SourceDefinition, ArchiveContext) {
        let agent = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:claude:native-session").unwrap(),
        );
        let source = SourceRef {
            issuer: Issuer::Environment(agent.environment().clone()),
            instance: "native-transcript".into(),
        };
        let proof = ObservationRef {
            issuer: source.issuer.clone(),
            producer_id: "native-capture".into(),
            producer_epoch: "first".into(),
            producer_seq: 1,
        };
        let position = SourcePosition {
            source: source.clone(),
            generation: 0,
            locator: SourceLocator::Jsonl {
                byte_start: 0,
                byte_end: 30,
                record_ordinal: 0,
            },
        };
        let capture = Capture {
            observation_ref: ObservationRef {
                producer_seq: 2,
                ..proof.clone()
            },
            source_position: SourcePosition {
                source: SourceRef {
                    instance: "source-inventory".into(),
                    ..source.clone()
                },
                generation: 0,
                locator: SourceLocator::Frame { ordinal: 0 },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("scope").unwrap(),
            declared_subject_refs: vec![],
            captured_at: TimeEvidence {
                domain: "capture-clock".into(),
                value: 100,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::SourceWall,
            },
            source_clock: None,
            transport_kind: TransportKind::SourceInventory,
            vendor_build: None,
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes("a".repeat(64)).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        let definition = SourceDefinition {
            source,
            generation: 0,
            locator: "/native/transcript.jsonl".into(),
            owner: agent.clone(),
            basis: SourceOwnerBasis::NativeIdentity {
                observation: proof.clone(),
            },
        };
        let archive = ArchiveContext {
            semantic:std::sync::Arc::new(super::super::ArchiveSemantic {object_evidence:[(AnyObjectRef::Agent(agent.clone()),vec![proof.clone()])].into(),..Default::default()}),
            source_owners: [(
                (definition.source.clone(), 0),
                SourceOwner {
                    agent,
                    evidence: vec![proof.clone()],
                    rule: "claude/native-identity".into(),
                },
            )]
            .into(),
            observation_sources: [(proof, position)].into(),
            ..ArchiveContext::default()
        };
        (capture, definition, archive)
    }

    fn run(
        capture: &Capture,
        definition: &SourceDefinition,
        archive: &ArchiveContext,
    ) -> MapOutput {
        InventoryMapper.map(MapInput {
            capture,
            canonical_observation: &capture.observation_ref,
            raw: &serde_json::to_vec(definition).unwrap(),
            state: &MapperState::default(),
            archive,
        })
    }

    #[test]
    fn a_routing_subject_never_substitutes_for_retained_native_owner_proof() {
        let (mut capture, definition, archive) = fixture();
        capture.declared_subject_refs = vec![definition.owner.clone()];
        let absent = run(&capture, &definition, &ArchiveContext::default());
        assert!(absent.source_owners.is_empty());
        assert!(matches!(
            absent.requests.as_slice(),
            [ArchiveRequest::Observation { .. }]
        ));
        let known = run(&capture, &definition, &archive);
        assert_eq!(known.source_owners.len(), 1);
        assert_eq!(known.source_owners[0].owner.agent, definition.owner);
        assert_eq!(known.source_owners[0].owner.evidence.len(), 2);
    }

    #[test]
    fn a_companion_metadata_source_inherits_only_the_proven_transcript_owner() {
        let (capture, definition, mut archive) = fixture();
        let native = run(&capture, &definition, &archive).source_owners.remove(0);
        archive
            .source_owners
            .insert((native.source.clone(), native.generation), native.owner);
        let mut companion = definition.clone();
        companion.source.instance = "metadata-sidecar".into();
        companion.basis = SourceOwnerBasis::CompanionTranscript {
            source: definition.source,
            generation: 0,
        };
        assert_eq!(
            run(&capture, &companion, &archive).source_owners[0]
                .owner
                .agent,
            companion.owner
        );
        companion.owner = AgentRef::Session(
            sylph_protocol::SessionId::decode("env7:claude:another-agent").unwrap(),
        );
        let wrong = run(&capture, &companion, &archive);
        assert!(wrong.source_owners.is_empty());
        assert_eq!(wrong.diagnostics[0].code, DiagnosticCode::ConflictingOwner);
    }

    #[test]
    fn a_rewritten_source_cannot_reuse_an_old_generation_owner_proof() {
        let (capture, mut definition, archive) = fixture();
        definition.generation = 1;
        let output = run(&capture, &definition, &archive);
        assert!(output.source_owners.is_empty());
        assert_eq!(output.diagnostics[0].code, DiagnosticCode::ConflictingOwner);
    }
}
