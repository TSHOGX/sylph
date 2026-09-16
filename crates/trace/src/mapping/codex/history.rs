//! Physical Codex rollout segments and their recorded ordinal/byte coordinates.

use crate::mapping::{MapInput, MapOutput};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use sylph_protocol::{trace::*, AgentName, NativeId, SessionId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub position: SourcePosition,
    pub observation: ObservationRef,
    pub vendor_ordinal: Option<u64>,
    pub header: Option<HistoryHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryHeader {
    pub agent: AgentRef,
    pub history_base: Option<Value>,
    pub inherited_before: Option<u64>,
    pub inherited_from: Option<AgentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedSource {
    pub definition: SourceDefinition,
    pub observation: ObservationRef,
}

pub type HistoryRecords = BTreeMap<(SourceRef, u64), BTreeMap<u64, HistoryRecord>>;

pub(super) fn record_source(input: &MapInput<'_>, value: &Value, output: &mut MapOutput) {
    if input.capture.transport_kind != TransportKind::NativeJsonl {
        return;
    }
    let SourceLocator::Jsonl { record_ordinal, .. } = input.capture.source_position.locator else {
        return;
    };
    let agent = |native: &str| match &input.capture.observation_ref.issuer {
        Issuer::Environment(environment) => Some(AgentRef::Session(SessionId::new(
            environment.clone(),
            AgentName::parse("codex").unwrap(),
            NativeId::new(native),
        ))),
        _ => None,
    };
    let header = if record_ordinal == 0
        && value.get("type").and_then(Value::as_str) == Some("session_meta")
    {
        let payload = &value["payload"];
        payload
            .get("id")
            .and_then(Value::as_str)
            .and_then(agent)
            .map(|owner| HistoryHeader {
                agent: owner,
                history_base: payload
                    .get("history_base")
                    .filter(|base| !base.is_null())
                    .cloned(),
                inherited_before: payload
                    .get("subagent_history_start_ordinal")
                    .and_then(Value::as_u64),
                inherited_from: payload
                    .get("parent_thread_id")
                    .or_else(|| payload.get("forked_from_id"))
                    .and_then(Value::as_str)
                    .and_then(agent),
            })
    } else {
        None
    };
    output.history_records.push(HistoryRecord {
        position: input.capture.source_position.clone(),
        observation: input.canonical_observation.clone(),
        vendor_ordinal: value.get("ordinal").and_then(Value::as_u64),
        header,
    });
}

use crate::mapping::{claim, ArchiveRequest, MapperState, MappingDiagnostic};
use std::collections::BTreeSet;
use sylph_protocol::{NativeDatabaseSnapshot, NativeDatabaseValue};

const ALIAS_PAGE: usize = 48;
const MAX_BASE_SEGMENTS: usize = 128;

#[derive(Debug, Clone)]
struct Segment {
    source: SourceRef,
    generation: u64,
    start_ordinal: u64,
    end_ordinal: u64,
    local_end: u64,
    owner: AgentRef,
    evidence: Vec<ObservationRef>,
}

#[derive(Default, Serialize, Deserialize)]
struct Expansion {
    next_entry: usize,
    prior: Option<EntryRef>,
}

struct HistoryFailure {
    code: DiagnosticCode,
    message: String,
    request: Option<Box<ArchiveRequest>>,
}
fn failure(code: DiagnosticCode, message: impl Into<String>) -> HistoryFailure {
    HistoryFailure {
        code,
        message: message.into(),
        request: None,
    }
}

pub fn physical_id(locator: &str) -> Option<&str> {
    let name = locator.rsplit('/').next()?.strip_suffix(".jsonl")?;
    // Canonical rollout filenames end in an immutable UUID. This is a physical
    // locator contract only; the retained governing header owns logical identity.
    if !name.starts_with("rollout-") {
        return None;
    }
    let boundary = name.len().checked_sub(37)?;
    if name.as_bytes().get(boundary) != Some(&b'-') {
        return None;
    }
    let id = name.get(boundary + 1..)?;
    let valid = id.bytes().enumerate().all(|(index, byte)| {
        if [8, 13, 18, 23].contains(&index) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    });
    valid.then_some(id)
}

fn records<'a>(
    input: &'a MapInput<'_>,
    source: &SourceRef,
    generation: u64,
) -> Result<&'a BTreeMap<u64, HistoryRecord>, HistoryFailure> {
    input
        .archive
        .history_records
        .get(&(source.clone(), generation))
        .ok_or_else(|| HistoryFailure {
            code: DiagnosticCode::MissingSource,
            message: "History source has no retained lexical coordinate witnesses".into(),
            request: Some(Box::new(ArchiveRequest::HistorySource {
                source: source.clone(),
                generation,
            })),
        })
}

fn segment(
    input: &MapInput<'_>,
    source: &SourceRef,
    generation: u64,
    end_local: u64,
    expected_end: Option<(u64, u64)>,
) -> Result<Segment, HistoryFailure> {
    let records = records(input, source, generation)?;
    let first = records.get(&0).ok_or_else(|| {
        failure(
            DiagnosticCode::MissingSource,
            "History source omits its governing physical header",
        )
    })?;
    let header = first.header.as_ref().ok_or_else(|| {
        failure(
            DiagnosticCode::MissingOwner,
            "History source has no valid governing header",
        )
    })?;
    if end_local == 0 {
        return Err(failure(
            DiagnosticCode::InvalidRelation,
            "A frozen rollout prefix cannot omit its header",
        ));
    }
    let mut previous_bytes = 0u64;
    let start = first.vendor_ordinal.unwrap_or(0);
    let mut evidence = vec![first.observation.clone()];
    for local in 0..end_local {
        let record = records.get(&local).ok_or_else(|| HistoryFailure {
            code: DiagnosticCode::CaptureGap,
            message: "Frozen history prefix is not completely captured".into(),
            request: Some(Box::new(ArchiveRequest::HistorySource {
                source: source.clone(),
                generation,
            })),
        })?;
        let SourceLocator::Jsonl {
            byte_start,
            byte_end,
            record_ordinal,
        } = record.position.locator
        else {
            return Err(failure(
                DiagnosticCode::InvalidRelation,
                "History coordinate witness is not lexical JSONL",
            ));
        };
        if record.position.source != *source
            || record.position.generation != generation
            || record_ordinal != local
            || byte_start != previous_bytes
            || byte_end <= byte_start
        {
            return Err(failure(
                DiagnosticCode::InvalidRelation,
                "Frozen history byte extents are missing or contradictory",
            ));
        }
        if let Some(initial) = first.vendor_ordinal {
            if record.vendor_ordinal != initial.checked_add(local) {
                return Err(failure(
                    DiagnosticCode::InvalidRelation,
                    "Recorded native ordinals are missing, skipped or regressed",
                ));
            }
        } else if expected_end.is_some() || header.history_base.is_some() {
            return Err(failure(
                DiagnosticCode::InvalidRelation,
                "A referenced history prefix requires recorded native ordinals",
            ));
        }
        previous_bytes = byte_end;
        if local + 1 == end_local {
            evidence.push(record.observation.clone());
        }
    }
    let end = start.checked_add(end_local).ok_or_else(|| {
        failure(
            DiagnosticCode::InvalidRelation,
            "History ordinal extent overflows",
        )
    })?;
    if expected_end.is_some_and(|expected| expected != (end, previous_bytes)) {
        return Err(failure(
            DiagnosticCode::InvalidRelation,
            "Frozen history exclusive byte and ordinal boundaries disagree",
        ));
    }
    if let Some(base) = &header.history_base {
        if base.get("end_ordinal_exclusive").and_then(Value::as_u64) != first.vendor_ordinal {
            return Err(failure(
                DiagnosticCode::InvalidRelation,
                "Reverted header ordinal does not begin at its recorded history base",
            ));
        }
    }
    Ok(Segment {
        source: source.clone(),
        generation,
        start_ordinal: start,
        end_ordinal: end,
        local_end: end_local,
        owner: header.agent.clone(),
        evidence,
    })
}

fn chain(
    input: &MapInput<'_>,
    source: &SourceRef,
    generation: u64,
    end_local: u64,
    expected_end: Option<(u64, u64)>,
    visiting: &mut BTreeSet<(SourceRef, u64)>,
    output: &mut Vec<Segment>,
) -> Result<(), HistoryFailure> {
    if !visiting.insert((source.clone(), generation)) {
        return Err(failure(
            DiagnosticCode::CyclicHistory,
            "Physical history bases form a cycle",
        ));
    }
    if visiting.len() > MAX_BASE_SEGMENTS {
        return Err(failure(
            DiagnosticCode::ResourceLimit,
            "Physical history base chain exceeds its declared segment budget",
        ));
    }
    let header = records(input, source, generation)?
        .get(&0)
        .and_then(|record| record.header.as_ref())
        .ok_or_else(|| {
            failure(
                DiagnosticCode::MissingOwner,
                "Physical source omits its retained governing header",
            )
        })?;
    if let Some(base) = &header.history_base {
        let physical = base
            .get("thread_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                failure(
                    DiagnosticCode::InvalidRelation,
                    "History base omits its physical rollout identity",
                )
            })?;
        let end_ordinal = base
            .get("end_ordinal_exclusive")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                failure(
                    DiagnosticCode::InvalidRelation,
                    "History base omits its exclusive native ordinal",
                )
            })?;
        let end_byte = base
            .get("end_byte_offset")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                failure(
                    DiagnosticCode::InvalidRelation,
                    "History base omits its exclusive physical byte offset",
                )
            })?;
        let candidates: Vec<_> = input
            .archive
            .source_definitions
            .values()
            .filter(|definition| {
                definition.definition.source.issuer == source.issuer
                    && physical_id(&definition.definition.locator) == Some(physical)
            })
            .collect();
        if candidates.is_empty() {
            return Err(HistoryFailure {
                code: DiagnosticCode::MissingSource,
                message: format!("Physical history base {physical} has no retained source binding"),
                request: Some(Box::new(ArchiveRequest::HistoryLocator {
                    issuer: source.issuer.clone(),
                    physical_id: physical.into(),
                })),
            });
        }
        if candidates.len() != 1 {
            return Err(failure(
                DiagnosticCode::ConflictingIdentity,
                "Physical history base resolves to multiple retained source generations",
            ));
        }
        let bound = candidates[0];
        let prior = &bound.definition;
        let prior_records = records(input, &prior.source, prior.generation)?;
        let last=prior_records.values().find(|record|matches!(record.position.locator,SourceLocator::Jsonl {byte_end,..} if byte_end==end_byte)).ok_or_else(||HistoryFailure {code:DiagnosticCode::CaptureGap,message:"Physical history base byte boundary has no complete retained record".into(),request:Some(Box::new(ArchiveRequest::HistorySource {source:prior.source.clone(),generation:prior.generation}))})?;
        let SourceLocator::Jsonl { record_ordinal, .. } = last.position.locator else {
            unreachable!()
        };
        chain(
            input,
            &prior.source,
            prior.generation,
            record_ordinal + 1,
            Some((end_ordinal, end_byte)),
            visiting,
            output,
        )?;
        let prior_segment = output.last_mut().unwrap();
        if prior_segment.owner != prior.owner {
            return Err(failure(
                DiagnosticCode::ConflictingOwner,
                "Physical source inventory disagrees with its retained governing header",
            ));
        }
        prior_segment.evidence.push(bound.observation.clone());
    }
    let current = segment(input, source, generation, end_local, expected_end)?;
    output.push(current);
    visiting.remove(&(source.clone(), generation));
    Ok(())
}

fn require_complete(input: &MapInput<'_>, segment: &Segment) -> Result<(), HistoryFailure> {
    let mut intervals: Vec<_> = input
        .archive
        .source_prefixes
        .get(&segment.source)
        .into_iter()
        .flatten()
        .filter(|extent| {
            extent.generation == segment.generation
                && extent.coordinate == HistoryCoordinate::LocalRecordIndex
                && extent.mapping_rule == "codex/native-history"
        })
        .map(|extent| (extent.start_inclusive, extent.end_exclusive))
        .collect();
    intervals.sort();
    let mut through = 0;
    for (start, end) in intervals {
        if start > through {
            break;
        }
        through = through.max(end);
        if through >= segment.local_end {
            return Ok(());
        }
    }
    Err(HistoryFailure {
        code: DiagnosticCode::CaptureGap,
        message: "Frozen history awaits complete semantic mapping of its lexical range".into(),
        request: Some(Box::new(ArchiveRequest::HistorySource {
            source: segment.source.clone(),
            generation: segment.generation,
        })),
    })
}

pub(super) fn map_selection(input: MapInput<'_>, snapshot: NativeDatabaseSnapshot) -> MapOutput {
    let mut output = MapOutput {
        state: input.state.clone(),
        ..MapOutput::default()
    };
    let result = (|| {
        let Some(NativeDatabaseValue::Text(id)) = snapshot.row.get("id") else {
            return Err(failure(
                DiagnosticCode::MissingOwner,
                "Selected rollout row omits the native thread identity",
            ));
        };
        let Issuer::Environment(environment) = &input.capture.observation_ref.issuer else {
            return Err(failure(
                DiagnosticCode::ConflictingOwner,
                "Selected rollout requires its acquiring environment",
            ));
        };
        let agent = AgentRef::Session(SessionId::new(
            environment.clone(),
            AgentName::parse("codex").unwrap(),
            NativeId::new(id),
        ));
        let Some((source, generation)) = snapshot
            .selected_source
            .clone()
            .zip(snapshot.selected_generation)
        else {
            return Err(failure(
                DiagnosticCode::MissingSource,
                "Selected rollout has no retained source inventory binding",
            ));
        };
        let Some(owner) = input
            .archive
            .source_owners
            .get(&(source.clone(), generation))
        else {
            return Err(HistoryFailure {
                code: DiagnosticCode::MissingOwner,
                message: "Selected source has not established its owner".into(),
                request: Some(Box::new(ArchiveRequest::Source { source, generation })),
            });
        };
        if owner.agent != agent {
            return Err(failure(
                DiagnosticCode::ConflictingOwner,
                "Selected source header and metadata thread disagree",
            ));
        }
        let count = snapshot.selected_record_count.ok_or_else(|| {
            failure(
                DiagnosticCode::UnknownSelection,
                "Selected source complete lexical extent is unknown",
            )
        })?;
        let mut segments = Vec::new();
        chain(
            &input,
            &source,
            generation,
            count,
            None,
            &mut BTreeSet::new(),
            &mut segments,
        )?;
        let mut evidence = owner.evidence.clone();
        evidence.push(input.canonical_observation.clone());
        for segment in &segments {
            evidence.extend(segment.evidence.iter().cloned());
        }
        evidence.sort();
        evidence.dedup();
        let selected = SelectionRef::occurrence(
            agent.clone(),
            input.capture.source_position.clone(),
            vec!["selected-rollout".into()],
        )
        .unwrap();
        let rule = format!(
            "codex/history-base-context:{}:{generation}",
            source.encode()
        );
        let mut entries = Vec::new();
        for (segment_index, segment) in segments.iter().enumerate() {
            // A header listed among an entry's provenance is not that entry's
            // coordinate. Read the occurrence address itself, then verify its
            // record witness and complete semantic mapping for the frozen range.
            require_complete(&input, segment)?;
            for (entry, position) in &input.archive.native_history_positions {
                if position.source == segment.source
                    && position.generation == segment.generation
                    && matches!(position.locator,SourceLocator::Jsonl {record_ordinal,..} if record_ordinal<segment.local_end)
                    && !input.archive.history_entries.contains_key(entry)
                {
                    return Err(HistoryFailure {
                        code: DiagnosticCode::MissingSource,
                        message: "Frozen history awaits its reduced entry declaration".into(),
                        request: Some(Box::new(ArchiveRequest::Object {
                            object_ref: AnyObjectRef::HistoryEntry(entry.clone()),
                        })),
                    });
                }
            }
            for entry in input.archive.history_entries.values() {
                if entry.containing_agent == agent {
                    continue;
                }
                let ObjectAddress::Occurrence { position, .. } = entry.entry_ref.address() else {
                    continue;
                };
                if position.source != segment.source || position.generation != segment.generation {
                    continue;
                }
                let SourceLocator::Jsonl { record_ordinal, .. } = position.locator else {
                    continue;
                };
                if record_ordinal >= segment.local_end {
                    continue;
                }
                if records(&input, &segment.source, segment.generation)?
                    .get(&record_ordinal)
                    .is_none_or(|record| record.position != *position)
                {
                    return Err(failure(
                        DiagnosticCode::InvalidRelation,
                        "History entry does not match its retained physical record",
                    ));
                }
                entries.push((segment_index, record_ordinal, entry.clone()));
            }
        }
        entries.sort_by_key(|(segment_index, ordinal, entry)| {
            (*segment_index, *ordinal, entry.entry_ref.clone())
        });
        let mut expansion: Expansion = input
            .state
            .values
            .get("codex-history-expansion")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| {
                failure(
                    DiagnosticCode::DecodeFailure,
                    "History continuation checkpoint is invalid",
                )
            })?
            .unwrap_or_default();
        if expansion.next_entry > entries.len() {
            return Err(failure(
                DiagnosticCode::InvalidRelation,
                "Frozen history membership changed during continuation",
            ));
        }
        let end = (expansion.next_entry + ALIAS_PAGE).min(entries.len());
        for (segment_index, local, entry) in &entries[expansion.next_entry.min(entries.len())..end]
        {
            let segment = &segments[*segment_index];
            if entry.containing_agent == agent {
                expansion.prior = Some(entry.entry_ref.clone());
                continue;
            }
            let alias = EntryRef::occurrence(
                agent.clone(),
                records(&input, &source, generation)?
                    .get(&0)
                    .unwrap()
                    .position
                    .clone(),
                vec![
                    "inherited-prefix".into(),
                    blake3::hash(entry.entry_ref.encode().as_bytes())
                        .to_hex()
                        .to_string(),
                ],
            )
            .unwrap();
            let original = entry
                .original_owner
                .clone()
                .unwrap_or_else(|| OwnerRef::Agent {
                    agent: entry.containing_agent.clone(),
                });
            let mut declared = claim(
                &input,
                &agent,
                &format!(
                    "codex/history-alias/{}",
                    blake3::hash(entry.entry_ref.encode().as_bytes()).to_hex()
                ),
                "codex-1",
                Fact::Declare {
                    object: TraceObject::HistoryEntry(HistoryEntry {
                        entry_ref: alias.clone(),
                        containing_agent: agent.clone(),
                        content: entry.content.clone(),
                        predecessor: expansion.prior.clone(),
                        original_owner: Some(original),
                    }),
                },
            );
            declared.evidence.extend(
                input
                    .archive
                    .object_evidence
                    .get(&AnyObjectRef::HistoryEntry(entry.entry_ref.clone()))
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            output.claims.push(declared);
            let position = &records(&input, &segment.source, segment.generation)?
                .get(local)
                .unwrap()
                .position;
            output.claims.push(claim(
                &input,
                &agent,
                &format!(
                    "codex/history-alias-position/{}",
                    blake3::hash(entry.entry_ref.encode().as_bytes()).to_hex()
                ),
                "codex-1",
                Fact::HistoryPosition {
                    entry: alias.clone(),
                    position: position.clone(),
                    coordinate: HistoryCoordinate::VendorOrdinal {
                        domain_ref: segment.source.clone(),
                    },
                    value: segment.start_ordinal + local,
                    mapping_rule: rule.clone(),
                },
            ));
            expansion.prior = Some(alias);
        }
        expansion.next_entry = end;
        if end < entries.len() {
            let mut state: MapperState = input.state.clone();
            state.values.insert(
                "codex-history-expansion".into(),
                serde_json::to_value(expansion).unwrap(),
            );
            output.continuation_state = Some(state);
            output.requests.push(ArchiveRequest::ContinueHistory);
            return Ok(());
        }
        let mut extents = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            let inherited = segment.owner != agent;
            let coordinate = if index + 1 == segments.len() {
                HistoryCoordinate::LocalRecordIndex
            } else {
                HistoryCoordinate::VendorOrdinal {
                    domain_ref: segment.source.clone(),
                }
            };
            let extent = SourceExtent {
                source: segment.source.clone(),
                generation: segment.generation,
                coordinate,
                start_inclusive: if index + 1 == segments.len() {
                    0
                } else {
                    segment.start_ordinal
                },
                end_exclusive: if index + 1 == segments.len() {
                    segment.local_end
                } else {
                    segment.end_ordinal
                },
                mapping_rule: if inherited {
                    rule.clone()
                } else {
                    "codex/native-history".into()
                },
                original_owner: None,
            };
            if inherited {
                let mut completed = claim(
                    &input,
                    &agent,
                    &format!("codex/history-alias-complete/{index}"),
                    "codex-1",
                    Fact::HistoryMappingComplete {
                        extent: extent.clone(),
                    },
                );
                completed.evidence.extend(evidence.iter().cloned());
                output.claims.push(completed);
            }
            extents.push(extent);
        }
        let mut declaration = claim(
            &input,
            &agent,
            "codex/database-selection",
            "codex-1",
            Fact::Declare {
                object: TraceObject::HistorySelection(HistorySelection {
                    selection_ref: selected.clone(),
                    agent: agent.clone(),
                    membership: SelectionMembership::SourceExtents {
                        extents,
                        inherited_selections: vec![],
                    },
                    excluded_heads: vec![],
                }),
            },
        );
        declaration.evidence.extend(evidence.iter().cloned());
        output.claims.push(declaration);
        let mut selection = claim(
            &input,
            &agent,
            "codex/database-selected",
            "codex-1",
            Fact::SelectHistory {
                agent: agent.clone(),
                selection: selected,
                selected_at: input.capture.source_position.clone(),
            },
        );
        selection.evidence.extend(evidence);
        output.claims.push(selection);
        Ok(())
    })();
    if let Err(problem) = result {
        if let Some(request) = problem.request {
            output.requests.push(*request);
        }
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code: problem.code,
            message: problem.message,
        });
    }
    if snapshot
        .selected_pending_bytes
        .is_some_and(|bytes| bytes > 0)
    {
        output.diagnostics.push(MappingDiagnostic {
            evidence: vec![input.canonical_observation.clone()],
            code: DiagnosticCode::CaptureGap,
            message: "Selected source has an incomplete pending lexical record".into(),
        });
    }
    output
}

#[cfg(test)]
mod tests;
