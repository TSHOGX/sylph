//! Explicit retained-claim inspection, separate from ordinary reading summaries.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadClaimSummary {
    pub preview: String,
    pub preview_truncated: bool,
    pub claim_ref: ClaimRef,
    pub fact_kind: TraceFactKind,
    pub mapper_build: String,
    pub rule_id: String,
    pub evidence_count: u64,
    pub detail_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadClaimDetail {
    pub view: ReadView,
    pub subject: TraceAddress,
    pub claim: Claim,
    pub related: Vec<TraceAddress>,
    pub sources: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadEvidenceBytes {
    pub max_range_bytes: usize,
    pub view: ReadView,
    pub capture: Capture,
    pub start: u64,
    pub end_exclusive: u64,
    pub total_bytes: Option<u64>,
    pub coverage: PayloadCompleteness,
    pub bytes_base64: String,
}
