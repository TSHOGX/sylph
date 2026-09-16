//! Runtime capture and control facts. Semantic interpretation belongs to archive mappers.

use crate::{
    trace::{
        Capture, IngestionEpoch, IngestionScope, ObservationRef, PayloadManifest, PayloadRef,
        TransportKind,
    },
    NativeId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Each actual process incarnation owns distinct stdout/stdin/stderr stream keys.
/// The sink commits exact bytes before the process transport parses or routes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCapture {
    pub stream: String,
    pub transport: TransportKind,
    pub bytes: Vec<u8>,
    pub captured_at_ms: i64,
}

#[async_trait]
pub trait RawCaptureSink: Send + Sync {
    async fn capture(&self, capture: RawCapture) -> anyhow::Result<ObservationRef>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeSource {
    pub locator: String,
    pub namespace: String,
    pub format: NativeSourceFormat,
    /// A known adjacent transcript, never a path-derived agent identity.
    pub companion_locator: Option<String>,
    pub owner: Option<NativeOwner>,
    /// Lexical JSONL ordinal of the record that contains the claimed native identity.
    pub identity_record_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum NativeOwner {
    Session {
        native_id: NativeId,
    },
    Embedded {
        anchor_native_id: NativeId,
        namespace: String,
        native_child_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSourceFormat {
    Jsonl,
    Snapshot,
    Bytes,
    CodexThreadMetadata,
    CodexGoalMetadata,
}

/// Observed source coverage at a committed acquisition cursor. Following describes
/// this reader, never whether a vendor process or execution is alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceStateEvent {
    pub source: crate::trace::SourceRef,
    pub generation: u64,
    pub complete_records: u64,
    pub complete_bytes: u64,
    pub pending_bytes: u64,
    pub following: bool,
    pub state: NativeSourceState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeSourceState {
    Present,
    Missing,
    Unreadable { reason: String },
}

/// A sidecar discovered after its parent's exact bytes were retained. The parent's
/// immutable capture is never rewritten; each observed sidecar revision is evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceAttachmentEvent {
    pub parent_observation: ObservationRef,
    pub parent_slot: String,
    pub attachment: crate::trace::CaptureAttachment,
}

/// A consistent vendor SQLite read preserves unknown columns and their storage classes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeDatabaseSnapshot {
    pub database_locator: String,
    pub table: String,
    pub columns: Vec<NativeDatabaseColumn>,
    pub status: NativeDatabaseStatus,
    pub row: std::collections::BTreeMap<String, NativeDatabaseValue>,
    pub selected_source: Option<crate::trace::SourceRef>,
    pub selected_generation: Option<u64>,
    pub selected_record_count: Option<u64>,
    pub selected_pending_bytes: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeDatabaseColumn {
    pub name: String,
    pub declared_type: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeDatabaseValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
    TextBytes(Vec<u8>),
    Blob(Vec<u8>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeDatabaseStatus {
    Available,
    MissingDatabase,
    MissingRow,
    Unreadable { reason: String },
}

/// Measured lifecycle of a process held by an adapter. `rpc_stream` is the exact
/// runtime stream key; the acquisition issuer determines its source reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceProcessEvent {
    pub rpc_stream: String,
    pub process_incarnation: String,
    pub event: ProcessEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProcessEvent {
    Spawned {
        pid: i32,
        program: String,
        args: Vec<String>,
        workdir: String,
    },
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    CaptureFailed {
        channel: String,
        reason: String,
    },
    WaitFailed {
        reason: String,
    },
}

/// Runtime control progress is independent of retained semantic activation outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlNotice {
    Identity {
        native_id: NativeId,
        evidence: ObservationRef,
    },
    Active {
        native_id: NativeId,
        native_turn_id: Option<String>,
    },
    Settled {
        native_id: NativeId,
        native_turn_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureAuthorization {
    pub scope: IngestionScope,
    pub epoch: IngestionEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PayloadDeclaration {
    Bytes {
        reference: PayloadRef,
        length: u64,
    },
    Manifest {
        reference: PayloadRef,
        manifest: PayloadManifest,
    },
}
impl PayloadDeclaration {
    pub fn reference(&self) -> &PayloadRef {
        match self {
            Self::Bytes { reference, .. } | Self::Manifest { reference, .. } => reference,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureUploadTicket {
    pub id: String,
    pub observation: ObservationRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CaptureDisposition {
    Retained { admission_seq: u64 },
    DiscardedByDeletion { tombstone_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureReceipt {
    pub observation: ObservationRef,
    pub disposition: CaptureDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CaptureReply {
    Pending {
        ticket: CaptureUploadTicket,
    },
    Admitted {
        receipts: Vec<CaptureReceipt>,
        accepted_through: u64,
        evidence_cut: u64,
    },
    ChunkStored,
    Rejected {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
// Values are bounded by the wire frame and queue limits; retain one owned serde envelope.
#[allow(clippy::large_enum_variant)]
pub enum CaptureUpload {
    Begin {
        capture: Capture,
        declarations: Vec<PayloadDeclaration>,
    },
    Chunk {
        ticket: CaptureUploadTicket,
        reference: PayloadRef,
        bytes: Vec<u8>,
    },
    Commit {
        ticket: CaptureUploadTicket,
    },
}
