//! Physical Codex rollout segments and their recorded ordinal/byte coordinates.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sylph_protocol::{trace::*, AgentName, NativeId, SessionId};
use crate::mapping::{MapInput, MapOutput};

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

pub type HistoryRecords = BTreeMap<(SourceRef,u64), BTreeMap<u64,HistoryRecord>>;

pub(super) fn record_source(input: &MapInput<'_>, value:&Value, output:&mut MapOutput) {
    if input.capture.transport_kind!=TransportKind::NativeJsonl { return; }
    let SourceLocator::Jsonl {record_ordinal,..}=input.capture.source_position.locator else { return; };
    let agent=|native:&str| match &input.capture.observation_ref.issuer {
        Issuer::Environment(environment)=>Some(AgentRef::Session(SessionId::new(environment.clone(),AgentName::parse("codex").unwrap(),NativeId::new(native)))),
        _=>None,
    };
    let header=if record_ordinal==0 && value.get("type").and_then(Value::as_str)==Some("session_meta") {
        let payload=&value["payload"];
        payload.get("id").and_then(Value::as_str).and_then(agent).map(|owner|HistoryHeader {
            agent:owner,
            history_base:payload.get("history_base").filter(|base|!base.is_null()).cloned(),
            inherited_before:payload.get("subagent_history_start_ordinal").and_then(Value::as_u64),
            inherited_from:payload.get("parent_thread_id").or_else(||payload.get("forked_from_id")).and_then(Value::as_str).and_then(agent),
        })
    } else { None };
    output.history_records.push(HistoryRecord {position:input.capture.source_position.clone(),observation:input.canonical_observation.clone(),vendor_ordinal:value.get("ordinal").and_then(Value::as_u64),header});
}
