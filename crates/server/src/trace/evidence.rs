//! Immutable capture admission, before and independently of semantic mapping.
//!
//! `docs/unified-trace-storage.md` § Admission and acknowledgments owns this
//! boundary. Payload publication is outside the SQLite write transaction; the
//! observation, both deduplication indexes and producer prefix commit together.

use std::{collections::BTreeSet, path::Path};

use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqliteConnection, SqlitePool,
};
use sylph_protocol::trace::{
    capture::{
        AttachmentAvailability, Capture, IngestionEpoch, PayloadCompleteness, PayloadManifest,
        PayloadRef,
    },
    identity::{AgentRef, Issuer, ObservationRef},
    IngestionScope,
};

use crate::storage::cas::BlobStore;

pub mod catalogue;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionLimits {
    pub max_batch_items: usize,
    pub max_batch_envelope_bytes: usize,
    pub max_chunk_bytes: usize,
    pub max_manifest_chunks: usize,
    pub max_inventory_subjects: usize,
    pub max_read_items: usize,
    pub max_read_bytes: usize,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            max_batch_items: 128,
            max_batch_envelope_bytes: 1024 * 1024,
            max_chunk_bytes: 1024 * 1024,
            max_manifest_chunks: 4096,
            max_inventory_subjects: 4096,
            max_read_items: 128,
            max_read_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("invalid capture: {0}")]
    Invalid(String),
    #[error("capture exceeds {0}")]
    Limit(&'static str),
    #[error("capture authority does not match the authenticated issuer or inventory")]
    Authority,
    #[error("ingestion epoch is unknown or no longer authorized")]
    Epoch,
    #[error("scope is suppressed; explicit re-adoption is required")]
    Suppressed,
    #[error("producer prefix requires sequence {expected}, received {received}")]
    Gap { expected: u64, received: u64 },
    #[error("receipt identity was reused with a different envelope")]
    ReceiptCollision,
    #[error("source position was reused with a different payload")]
    SourceCollision,
    #[error("payload is unavailable or corrupt: {0}")]
    Payload(String),
    #[error("the requested payload range includes bytes not retained by the source")]
    UnavailableRange,
    #[error("evidence database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("evidence filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("evidence encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[cfg(test)]
    #[error("injected admission crash at {0:?}")]
    Crash(CrashPoint),
}

pub type Result<T> = std::result::Result<T, EvidenceError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReceiptDisposition {
    Retained { admission_seq: u64 },
    DiscardedByDeletion { tombstone_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionReceipt {
    pub observation_ref: ObservationRef,
    pub disposition: ReceiptDisposition,
    /// A replay confirms the original receipt; it does not recapture erased bytes.
    pub newly_recorded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionBatch {
    pub receipts: Vec<AdmissionReceipt>,
    pub accepted_through: u64,
    pub evidence_cut: u64,
}

#[derive(Debug, Clone)]
pub enum CapturePreflight {
    Recorded(AdmissionReceipt),
    DiscardedByDeletion { tombstone_id: String },
    RequiresPayload,
}

/// The manifest's fixed representation is shared by command acceptance and
/// payload publication, so an outbox does not derive a second address scheme.
pub fn manifest_bytes(manifest: &PayloadManifest) -> Result<Vec<u8>> {
    manifest.canonical_bytes().map_err(invalid)
}

pub fn manifest_ref(manifest: &PayloadManifest) -> Result<PayloadRef> {
    PayloadRef::manifest(blake3::hash(&manifest_bytes(manifest)?).to_hex()).map_err(invalid)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmittedCapture {
    pub admission_seq: u64,
    pub occurrence_id: u64,
    pub canonical_observation_ref: ObservationRef,
    pub capture: Capture,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionPage {
    pub captures: Vec<AdmittedCapture>,
    pub next_after: u64,
    pub has_more: bool,
    pub evidence_cut: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadSlice {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub total_length: Option<u64>,
    pub coverage: PayloadCompleteness,
}

#[derive(Clone)]
pub struct EvidenceStore {
    writer: SqlitePool,
    readers: SqlitePool,
    blobs: BlobStore,
    limits: AdmissionLimits,
}

impl EvidenceStore {
    pub async fn open(
        path: impl AsRef<Path>,
        blobs: BlobStore,
        limits: AdmissionLimits,
    ) -> Result<Self> {
        if [
            limits.max_batch_items,
            limits.max_batch_envelope_bytes,
            limits.max_chunk_bytes,
            limits.max_manifest_chunks,
            limits.max_inventory_subjects,
            limits.max_read_items,
            limits.max_read_bytes,
        ]
        .contains(&0)
        {
            return Err(EvidenceError::Invalid(
                "all admission limits must be positive".into(),
            ));
        }
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let mut tx = writer.begin_with("BEGIN IMMEDIATE").await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *tx)
            .await?;
        match version {
            0 => {
                sqlx::raw_sql(include_str!("evidence/schema.sql"))
                    .execute(&mut *tx)
                    .await?;
                sqlx::raw_sql(include_str!("evidence/catalogue/schema.sql"))
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("PRAGMA user_version = 1")
                    .execute(&mut *tx)
                    .await?;
            }
            1 => {}
            _ => return Err(EvidenceError::Invalid("unsupported evidence schema".into())),
        }
        tx.commit().await?;
        let readers = SqlitePoolOptions::new()
            .max_connections(4)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA query_only = ON").execute(conn).await?;
                    Ok(())
                })
            })
            .connect_with(options.read_only(true))
            .await?;
        Ok(Self {
            writer,
            readers,
            blobs,
            limits,
        })
    }

    pub fn limits(&self) -> &AdmissionLimits {
        &self.limits
    }

    /// Issue/persist an inventory before capture. Reconnect reuses its epoch.
    /// The caller has already checked current environment control authority.
    pub async fn authorize_scope(
        &self,
        scope: &IngestionScope,
        subjects: &[AgentRef],
    ) -> Result<IngestionEpoch> {
        self.inventory(scope, subjects, false).await
    }

    /// The explicit re-adoption action creates a fresh inventory and epoch.
    /// Old producer receipts and tombstones remain permanent.
    pub async fn readopt_scope(
        &self,
        scope: &IngestionScope,
        subjects: &[AgentRef],
    ) -> Result<IngestionEpoch> {
        self.inventory(scope, subjects, true).await
    }

    async fn inventory(
        &self,
        scope: &IngestionScope,
        subjects: &[AgentRef],
        readopt: bool,
    ) -> Result<IngestionEpoch> {
        if (subjects.is_empty() && matches!(scope, IngestionScope::Agent(_)))
            || subjects.len() > self.limits.max_inventory_subjects
        {
            return Err(EvidenceError::Limit("inventory subject limit"));
        }
        scope.validate().map_err(invalid)?;
        let environment = scope.environment().as_str();
        for subject in subjects {
            subject.validate().map_err(invalid)?;
            if !scope.permits_subject(subject) {
                return Err(EvidenceError::Authority);
            }
        }
        let scope_key = scope.encode();
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let existing = sqlx::query(
            "SELECT current_epoch, suppressed FROM ingestion_scopes WHERE scope_ref = ?",
        )
        .bind(&scope_key)
        .fetch_optional(&mut *tx)
        .await?;
        let epoch = if let Some(row) = existing {
            let held_epoch: String = row.try_get("current_epoch")?;
            let suppressed = row.try_get::<i64, _>("suppressed")? != 0
                || epoch_tombstone(&mut tx, &held_epoch).await?.is_some();
            if suppressed && !readopt {
                return Err(EvidenceError::Suppressed);
            }
            if readopt && !suppressed {
                return Err(EvidenceError::Invalid(
                    "re-adoption requires a deleted scope".into(),
                ));
            }
            if suppressed {
                if let IngestionScope::Agent(agent) = scope {
                    // Closing suppression fences every inventory authorized during
                    // it, including identities whose vendor binding arrives later.
                    // Append a cutoff fact; the original deletion stays immutable.
                    let tombstone = epoch_tombstone(&mut tx, &held_epoch)
                        .await?
                        .ok_or(EvidenceError::Epoch)?;
                    let through: i64 = sqlx::query_scalar(
                        "SELECT epoch_order FROM ingestion_clock WHERE singleton=1",
                    )
                    .fetch_one(&mut *tx)
                    .await?;
                    sqlx::query("INSERT INTO agent_deletions(agent_ref,deleted_through_order,tombstone_id,deleted_at_ms) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
                        .bind(agent.encode()).bind(through).bind(tombstone).bind(now_ms()).execute(&mut *tx).await?;
                }
                let epoch = uuid::Uuid::new_v4().to_string();
                sqlx::query("UPDATE ingestion_scopes SET current_epoch = ?, suppressed = 0 WHERE scope_ref = ?")
                    .bind(&epoch).bind(&scope_key).execute(&mut *tx).await?;
                epoch
            } else {
                row.try_get("current_epoch")?
            }
        } else {
            if readopt {
                return Err(EvidenceError::Epoch);
            }
            let epoch = uuid::Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO ingestion_scopes (scope_ref, scope_json, issuer_ref, vendor, current_epoch, suppressed) VALUES (?, ?, ?, ?, ?, 0)")
                .bind(&scope_key).bind(serde_json::to_string(scope)?).bind(environment).bind(scope.vendor().as_str()).bind(&epoch).execute(&mut *tx).await?;
            epoch
        };
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ingestion_epochs WHERE epoch=?)")
                .bind(&epoch)
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            let epoch_order:i64=sqlx::query_scalar("UPDATE ingestion_clock SET epoch_order=epoch_order+1 WHERE singleton=1 RETURNING epoch_order").fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO ingestion_epochs (epoch, scope_ref, opened_at_ms,epoch_order) VALUES (?, ?, ?,?)")
                .bind(&epoch).bind(&scope_key).bind(now_ms()).bind(epoch_order).execute(&mut *tx).await?;
        }
        for subject in subjects {
            sqlx::query("INSERT INTO inventory_subjects (epoch, subject_ref) VALUES (?, ?) ON CONFLICT DO NOTHING")
                .bind(&epoch).bind(subject.encode()).execute(&mut *tx).await?;
        }
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM inventory_subjects WHERE epoch = ?")
                .bind(&epoch)
                .fetch_one(&mut *tx)
                .await?;
        if total > self.limits.max_inventory_subjects as i64 {
            return Err(EvidenceError::Limit("inventory subject limit"));
        }
        tx.commit().await?;
        IngestionEpoch::new(epoch).map_err(invalid)
    }

    /// Commit the permanent visibility boundary before any eventual byte GC.
    pub async fn delete_scope(&self, scope: &IngestionScope) -> Result<String> {
        scope.validate().map_err(invalid)?;
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let scope_key = scope.encode();
        let epoch: Option<String> =
            sqlx::query_scalar("SELECT current_epoch FROM ingestion_scopes WHERE scope_ref = ?")
                .bind(&scope_key)
                .fetch_optional(&mut *tx)
                .await?;
        let epoch = match epoch {
            Some(epoch) => epoch,
            None if matches!(scope, IngestionScope::Agent(_)) => {
                let epoch = uuid::Uuid::new_v4().to_string();
                let epoch_order:i64=sqlx::query_scalar("UPDATE ingestion_clock SET epoch_order=epoch_order+1 WHERE singleton=1 RETURNING epoch_order").fetch_one(&mut *tx).await?;
                sqlx::query("INSERT INTO ingestion_scopes(scope_ref,scope_json,issuer_ref,vendor,current_epoch,suppressed) VALUES(?,?,?,?,?,0)")
                    .bind(&scope_key).bind(serde_json::to_string(scope)?).bind(scope.environment().as_str()).bind(scope.vendor().as_str()).bind(&epoch).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO ingestion_epochs(epoch,scope_ref,opened_at_ms,epoch_order) VALUES(?,?,?,?)")
                    .bind(&epoch).bind(&scope_key).bind(now_ms()).bind(epoch_order).execute(&mut *tx).await?;
                epoch
            }
            None => return Err(EvidenceError::Epoch),
        };
        if let Some(id) = sqlx::query_scalar::<_, String>(
            "SELECT tombstone_id FROM deletion_tombstones WHERE epoch = ?",
        )
        .bind(&epoch)
        .fetch_optional(&mut *tx)
        .await?
        {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO deletion_tombstones (tombstone_id, epoch, scope_ref, deleted_at_ms) VALUES (?, ?, ?, ?)")
            .bind(&id).bind(&epoch).bind(&scope_key).bind(now_ms()).execute(&mut *tx).await?;
        sqlx::query("UPDATE ingestion_scopes SET suppressed = 1 WHERE scope_ref = ?")
            .bind(&scope_key)
            .execute(&mut *tx)
            .await?;
        if let IngestionScope::Agent(agent) = scope {
            let through: i64 =
                sqlx::query_scalar("SELECT epoch_order FROM ingestion_clock WHERE singleton=1")
                    .fetch_one(&mut *tx)
                    .await?;
            sqlx::query("INSERT INTO agent_deletions(agent_ref,deleted_through_order,tombstone_id,deleted_at_ms) VALUES(?,?,?,?)")
                .bind(agent.encode()).bind(through).bind(&id).bind(now_ms()).execute(&mut *tx).await?;
        }
        catalogue::record_scope_deletion(&mut tx, &scope_key, &epoch, &id, now_ms()).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn current_scope_epoch(
        &self,
        scope: &IngestionScope,
    ) -> Result<Option<IngestionEpoch>> {
        scope.validate().map_err(invalid)?;
        let epoch:Option<String>=sqlx::query_scalar("SELECT s.current_epoch FROM ingestion_scopes s JOIN ingestion_epoch_tombstones d ON d.epoch=s.current_epoch WHERE s.scope_ref=? AND s.suppressed=0 AND d.tombstone_id IS NULL")
            .bind(scope.encode()).fetch_optional(&self.readers).await?;
        epoch
            .map(|value| IngestionEpoch::new(value).map_err(invalid))
            .transpose()
    }

    /// Publish one bounded exact-byte chunk, including lexical JSON spelling.
    /// Larger records use a manifest; the chunk bound never truncates a record.
    pub(crate) async fn stage_payload(&self, bytes: &[u8]) -> Result<PayloadRef> {
        self.stage_payload_inner(bytes, None).await
    }

    async fn stage_payload_inner(
        &self,
        bytes: &[u8],
        crash: Option<CrashPoint>,
    ) -> Result<PayloadRef> {
        if bytes.len() > self.limits.max_chunk_bytes {
            return Err(EvidenceError::Limit("payload chunk bytes"));
        }
        fail_at(crash, CrashPoint::BeforePayloadSync)?;
        let digest = blake3::hash(bytes).to_hex().to_string();
        let write_id = self.begin_blob_write(&digest).await?;
        let written = self.blobs.put(bytes).await.map_err(payload_error);
        if let Err(error) = written {
            self.end_blob_write(&write_id).await?;
            return Err(error);
        }
        fail_at(crash, CrashPoint::AfterPayloadSync)?;
        let reference = PayloadRef::bytes(digest.clone()).map_err(invalid)?;
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO payloads (payload_ref, physical_digest, availability, kind, stored_length, decoded_length, decoded_digest, complete, staged_at_ms) VALUES (?, ?, 'available', 'bytes', ?, ?, ?, 1, ?) ON CONFLICT(payload_ref) DO UPDATE SET availability='available'")
            .bind(reference.as_str()).bind(&digest).bind(bytes.len() as i64).bind(bytes.len() as i64).bind(&digest).bind(now_ms())
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM trace_blob_writes WHERE write_id=?")
            .bind(write_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(reference)
    }

    /// Manifest identity covers representation metadata and ordered byte ranges.
    pub(crate) async fn stage_manifest(&self, manifest: &PayloadManifest) -> Result<PayloadRef> {
        manifest.validate().map_err(invalid)?;
        if manifest.chunks.len() > self.limits.max_manifest_chunks {
            return Err(EvidenceError::Limit("manifest chunk count"));
        }
        let complete = manifest.completeness == PayloadCompleteness::Complete;
        let total = manifest.decoded_length.map(signed).transpose()?;
        let serialized = serde_json::to_vec(manifest)?;
        if serialized.len() > self.limits.max_batch_envelope_bytes {
            return Err(EvidenceError::Limit("manifest bytes"));
        }
        let mut decoded_hasher = blake3::Hasher::new();
        for chunk in &manifest.chunks {
            let row =
                sqlx::query("SELECT kind, decoded_length FROM payloads WHERE payload_ref = ?")
                    .bind(chunk.digest.as_str())
                    .fetch_optional(&self.readers)
                    .await?
                    .ok_or_else(|| EvidenceError::Payload(chunk.digest.as_str().into()))?;
            let length = unsigned(row.try_get("decoded_length")?)?;
            if row.try_get::<String, _>("kind")? != "bytes"
                || chunk
                    .chunk_offset
                    .checked_add(chunk.length)
                    .is_none_or(|end| end > length)
            {
                return Err(EvidenceError::Invalid(
                    "manifest range exceeds its exact-byte chunk".into(),
                ));
            }
            let chunk_bytes = self.verify_bytes(&chunk.digest, length).await?;
            decoded_hasher.update(
                &chunk_bytes
                    [chunk.chunk_offset as usize..(chunk.chunk_offset + chunk.length) as usize],
            );
        }
        let decoded_digest = complete.then(|| decoded_hasher.finalize().to_hex().to_string());
        // The encoding marker pins manifest serialization; the typed payload key
        // distinguishes this interpretation from the same bytes captured as raw data.
        let encoded = manifest_bytes(manifest)?;
        let digest = blake3::hash(&encoded).to_hex().to_string();
        let write_id = self.begin_blob_write(&digest).await?;
        let written = self.blobs.put(&encoded).await.map_err(payload_error);
        if let Err(error) = written {
            self.end_blob_write(&write_id).await?;
            return Err(error);
        }
        let reference = PayloadRef::manifest(&digest).map_err(invalid)?;
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO payloads (payload_ref, physical_digest, availability, kind, stored_length, decoded_length, decoded_digest, complete, manifest_json, staged_at_ms) VALUES (?, ?, 'available', 'manifest', ?, ?, ?, ?, ?, ?) ON CONFLICT(payload_ref) DO UPDATE SET availability='available'")
            .bind(reference.as_str()).bind(&digest).bind(encoded.len() as i64).bind(total).bind(&decoded_digest).bind(i64::from(complete))
            .bind(std::str::from_utf8(&serialized).expect("JSON is UTF-8")).bind(now_ms()).execute(&mut *tx).await?;
        for (ordinal, chunk) in manifest.chunks.iter().enumerate() {
            sqlx::query("INSERT INTO payload_chunks (manifest_ref, ordinal, chunk_ref, chunk_offset, length, payload_offset) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING")
                .bind(reference.as_str()).bind(ordinal as i64).bind(chunk.digest.as_str()).bind(signed(chunk.chunk_offset)?)
                .bind(signed(chunk.length)?).bind(signed(chunk.payload_offset)?).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM trace_blob_writes WHERE write_id=?")
            .bind(write_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(reference)
    }

    /// Admit a contiguous batch belonging to one authenticated producer epoch.
    /// Every returned prefix already survived a FULL-durable evidence commit.
    pub(crate) async fn admit_batch(
        &self,
        issuer: &Issuer,
        captures: &[Capture],
    ) -> Result<AdmissionBatch> {
        self.admit_inner(issuer, captures, None).await
    }

    pub async fn needs_payloads(&self, capture: &Capture) -> Result<bool> {
        Ok(matches!(
            self.admission_preflight(&capture.observation_ref.issuer, capture)
                .await?,
            CapturePreflight::RequiresPayload
        ))
    }

    /// Advisory upload eligibility only. Admission repeats these checks inside
    /// its transaction, so deletion racing this read cannot authorize evidence.
    pub async fn admission_preflight(
        &self,
        issuer: &Issuer,
        capture: &Capture,
    ) -> Result<CapturePreflight> {
        capture.validate().map_err(invalid)?;
        if &capture.observation_ref.issuer != issuer
            || &capture.source_position.source.issuer != issuer
        {
            return Err(EvidenceError::Authority);
        }
        let envelope = serde_json::to_vec(capture)?;
        if envelope.len() > self.limits.max_batch_envelope_bytes {
            return Err(EvidenceError::Limit("capture envelope bytes"));
        }
        if let Some(row) = sqlx::query("SELECT envelope_digest, disposition, admission_seq, tombstone_id FROM producer_receipts WHERE observation_ref = ?")
            .bind(capture.observation_ref.encode()).fetch_optional(&self.readers).await? {
            if row.try_get::<String, _>("envelope_digest")? != blake3::hash(&envelope).to_hex().as_str() {
                return Err(EvidenceError::ReceiptCollision);
            }
            return Ok(CapturePreflight::Recorded(AdmissionReceipt { observation_ref: capture.observation_ref.clone(), disposition: disposition(&row)?, newly_recorded: false }));
        }
        let mut reader = self.readers.acquire().await?;
        if let Some(tombstone_id) = check_inventory(&mut reader, issuer, capture).await? {
            return Ok(CapturePreflight::DiscardedByDeletion { tombstone_id });
        }
        Ok(CapturePreflight::RequiresPayload)
    }

    async fn admit_inner(
        &self,
        issuer: &Issuer,
        captures: &[Capture],
        crash: Option<CrashPoint>,
    ) -> Result<AdmissionBatch> {
        self.admit_guarded_inner(issuer, captures, crash, None)
            .await
    }

    pub(crate) async fn admit_owned(
        &self,
        issuer: &Issuer,
        capture: &Capture,
        guard: &crate::trace::upload::AdmissionGuard,
    ) -> Result<AdmissionBatch> {
        self.admit_guarded_inner(issuer, std::slice::from_ref(capture), None, Some(guard))
            .await
    }

    async fn admit_guarded_inner(
        &self,
        issuer: &Issuer,
        captures: &[Capture],
        crash: Option<CrashPoint>,
        guard: Option<&crate::trace::upload::AdmissionGuard>,
    ) -> Result<AdmissionBatch> {
        if captures.is_empty() || captures.len() > self.limits.max_batch_items {
            return Err(EvidenceError::Limit("capture batch items"));
        }
        let first = &captures[0].observation_ref;
        let issuer_key = serde_json::to_string(issuer)?;
        let mut prepared = Vec::with_capacity(captures.len());
        let mut bytes = 0usize;
        for (index, capture) in captures.iter().enumerate() {
            capture.validate().map_err(invalid)?;
            let observation = &capture.observation_ref;
            if &observation.issuer != issuer || &capture.source_position.source.issuer != issuer {
                return Err(EvidenceError::Authority);
            }
            if observation.producer_id != first.producer_id
                || observation.producer_epoch != first.producer_epoch
            {
                return Err(EvidenceError::Invalid(
                    "a batch must use one producer epoch".into(),
                ));
            }
            if index > 0
                && captures[index - 1]
                    .observation_ref
                    .producer_seq
                    .checked_add(1)
                    != Some(observation.producer_seq)
            {
                return Err(EvidenceError::Invalid(
                    "batch sequences must be contiguous".into(),
                ));
            }
            signed(observation.producer_seq)?;
            signed(capture.source_position.generation)?;
            let envelope = serde_json::to_string(capture)?;
            bytes = bytes
                .checked_add(envelope.len())
                .ok_or(EvidenceError::Limit("capture envelope bytes"))?;
            if bytes > self.limits.max_batch_envelope_bytes {
                return Err(EvidenceError::Limit("capture envelope bytes"));
            }
            let digest = blake3::hash(envelope.as_bytes()).to_hex().to_string();
            // Receipt replay and stale epochs require no payload reads. In
            // particular, a never-admitted old spool item can be discarded after
            // deletion without uploading its erased bytes again.
            if matches!(
                self.admission_preflight(issuer, capture).await?,
                CapturePreflight::RequiresPayload
            ) {
                for payload in capture_payload_refs(capture) {
                    self.verify_payload(payload).await?;
                }
                for payload in std::iter::once(&capture.payload_ref).chain(
                    capture
                        .attachments
                        .iter()
                        .filter(|attachment| {
                            attachment.availability == AttachmentAvailability::Captured
                        })
                        .filter_map(|attachment| attachment.payload_ref.as_ref()),
                ) {
                    let complete: bool =
                        sqlx::query_scalar("SELECT complete FROM payloads WHERE payload_ref = ?")
                            .bind(payload.as_str())
                            .fetch_one(&self.readers)
                            .await?;
                    if !complete {
                        return Err(EvidenceError::Invalid(
                            "lexical captures and captured sidecars must be completely retained"
                                .into(),
                        ));
                    }
                }
            }
            prepared.push((envelope, digest));
        }
        fail_at(crash, CrashPoint::BeforeTransaction)?;
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let mut prefix = producer_prefix(
            &mut tx,
            &issuer_key,
            &first.producer_id,
            &first.producer_epoch,
        )
        .await?;
        let mut receipts = Vec::with_capacity(captures.len());
        for (capture, (envelope, digest)) in captures.iter().zip(prepared) {
            let observation = &capture.observation_ref;
            let observation_key = observation.encode();
            if let Some(row) = sqlx::query("SELECT envelope_digest, disposition, admission_seq, tombstone_id FROM producer_receipts WHERE observation_ref = ?")
                .bind(&observation_key).fetch_optional(&mut *tx).await? {
                if row.try_get::<String, _>("envelope_digest")? != digest {
                    return Err(EvidenceError::ReceiptCollision);
                }
                if let Some(guard)=guard{catalogue::upload_store::finish_guard(&mut tx,guard,capture,&disposition(&row)?,false).await?;}
                receipts.push(AdmissionReceipt { observation_ref: observation.clone(), disposition: disposition(&row)?, newly_recorded: false });
                continue;
            }
            if observation.producer_seq != prefix + 1 {
                return Err(EvidenceError::Gap {
                    expected: prefix + 1,
                    received: observation.producer_seq,
                });
            }
            let epoch = capture.ingestion_authorization_epoch.as_str();
            let tombstone = check_inventory(&mut tx, issuer, capture).await?;
            if tombstone.is_none() {
                if let Some(guard) = guard {
                    catalogue::upload_store::verify_guard(&mut tx, guard, capture, true).await?;
                }
            }
            let receipt_disposition = if let Some(tombstone_id) = tombstone {
                ReceiptDisposition::DiscardedByDeletion { tombstone_id }
            } else {
                let source = &capture.source_position.source;
                let source_key = source.encode();
                sqlx::query("INSERT INTO sources (source_ref, issuer_ref, source_instance) VALUES (?, ?, ?) ON CONFLICT DO NOTHING")
                    .bind(&source_key).bind(&issuer_key).bind(&source.instance).execute(&mut *tx).await?;
                let generation = signed(capture.source_position.generation)?;
                sqlx::query("INSERT INTO source_generations (source_ref, generation) VALUES (?, ?) ON CONFLICT DO NOTHING")
                    .bind(&source_key).bind(generation).execute(&mut *tx).await?;
                let locator = serde_json::to_string(&capture.source_position.locator)?;
                let decoded_digest: String =
                    sqlx::query_scalar("SELECT decoded_digest FROM payloads WHERE payload_ref = ?")
                        .bind(capture.payload_ref.as_str())
                        .fetch_one(&mut *tx)
                        .await?;
                let occurrence_id = if let Some(row) = sqlx::query("SELECT occurrence_id, decoded_digest FROM occurrences WHERE ingestion_epoch = ? AND source_ref = ? AND generation = ? AND locator_json = ?")
                    .bind(epoch).bind(&source_key).bind(generation).bind(&locator).fetch_optional(&mut *tx).await? {
                    if row.try_get::<String, _>("decoded_digest")? != decoded_digest {
                        return Err(EvidenceError::SourceCollision);
                    }
                    row.try_get::<i64, _>("occurrence_id")?
                } else {
                    sqlx::query("INSERT INTO occurrences (ingestion_epoch, source_ref, generation, locator_json, payload_ref, decoded_digest, canonical_observation_ref) VALUES (?, ?, ?, ?, ?, ?, ?)")
                        .bind(epoch).bind(&source_key).bind(generation).bind(&locator).bind(capture.payload_ref.as_str()).bind(&decoded_digest).bind(&observation_key).execute(&mut *tx).await?.last_insert_rowid()
                };
                let envelope_bytes = envelope.len() as i64;
                let admission_seq = sqlx::query("INSERT INTO observations (observation_ref, occurrence_id, envelope_json, envelope_bytes, admitted_at_ms) VALUES (?, ?, ?, ?, ?)")
                    .bind(&observation_key).bind(occurrence_id).bind(envelope).bind(envelope_bytes).bind(now_ms()).execute(&mut *tx).await?.last_insert_rowid();
                for payload in capture_payload_refs(capture) {
                    sqlx::query("INSERT INTO observation_payload_roots (admission_seq, payload_ref) VALUES (?, ?)")
                        .bind(admission_seq).bind(payload.as_str()).execute(&mut *tx).await?;
                    catalogue::gc_store::add_payload_roots(
                        &mut tx,
                        "observation",
                        &observation_key,
                        epoch,
                        payload,
                    )
                    .await
                    .map_err(|e| EvidenceError::Invalid(e.to_string()))?;
                }
                ReceiptDisposition::Retained {
                    admission_seq: unsigned(admission_seq)?,
                }
            };
            let (kind, admitted, deleted) = match &receipt_disposition {
                ReceiptDisposition::Retained { admission_seq } => {
                    ("retained", Some(signed(*admission_seq)?), None)
                }
                ReceiptDisposition::DiscardedByDeletion { tombstone_id } => {
                    ("discarded_by_deletion", None, Some(tombstone_id.as_str()))
                }
            };
            sqlx::query("INSERT INTO producer_receipts (observation_ref, issuer_ref, producer_id, producer_epoch, producer_seq, envelope_digest, disposition, admission_seq, tombstone_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(observation_key).bind(&issuer_key).bind(&observation.producer_id).bind(&observation.producer_epoch)
                .bind(signed(observation.producer_seq)?).bind(digest).bind(kind).bind(admitted).bind(deleted).execute(&mut *tx).await?;
            if let Some(guard) = guard {
                catalogue::upload_store::finish_guard(
                    &mut tx,
                    guard,
                    capture,
                    &receipt_disposition,
                    false,
                )
                .await?;
            }
            prefix = observation.producer_seq;
            receipts.push(AdmissionReceipt {
                observation_ref: observation.clone(),
                disposition: receipt_disposition,
                newly_recorded: true,
            });
        }
        sqlx::query("INSERT INTO producer_prefixes (issuer_ref, producer_id, producer_epoch, accepted_through) VALUES (?, ?, ?, ?) ON CONFLICT (issuer_ref, producer_id, producer_epoch) DO UPDATE SET accepted_through = excluded.accepted_through")
            .bind(&issuer_key).bind(&first.producer_id).bind(&first.producer_epoch).bind(signed(prefix)?).execute(&mut *tx).await?;
        let cut: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq), 0) FROM observations")
                .fetch_one(&mut *tx)
                .await?;
        fail_at(crash, CrashPoint::BeforeCommit)?;
        tx.commit().await?;
        fail_at(crash, CrashPoint::AfterCommit)?;
        Ok(AdmissionBatch {
            receipts,
            accepted_through: prefix,
            evidence_cut: unsigned(cut)?,
        })
    }

    pub async fn receipt(&self, observation: &ObservationRef) -> Result<Option<AdmissionReceipt>> {
        let row = sqlx::query("SELECT disposition, admission_seq, tombstone_id FROM producer_receipts WHERE observation_ref = ?")
            .bind(observation.encode()).fetch_optional(&self.readers).await?;
        row.map(|row| {
            Ok(AdmissionReceipt {
                observation_ref: observation.clone(),
                disposition: disposition(&row)?,
                newly_recorded: false,
            })
        })
        .transpose()
    }

    /// A source proof is read from admitted occurrence metadata, never from a
    /// routing hint or a reconstructed filename. Deleted epochs remain invisible.
    pub async fn observation_source(
        &self,
        observation: &ObservationRef,
    ) -> Result<Option<sylph_protocol::trace::SourcePosition>> {
        let row=sqlx::query("SELECT c.source_ref,c.generation,c.locator_json FROM observations o JOIN occurrences c ON c.occurrence_id=o.occurrence_id JOIN ingestion_epoch_tombstones t ON t.epoch=c.ingestion_epoch WHERE o.observation_ref=? AND t.tombstone_id IS NULL")
            .bind(observation.encode()).fetch_optional(&self.readers).await?;
        row.map(|row| {
            Ok(sylph_protocol::trace::SourcePosition {
                source: sylph_protocol::trace::SourceRef::decode(
                    &row.try_get::<String, _>("source_ref")?,
                )
                .map_err(invalid)?,
                generation: unsigned(row.try_get("generation")?)?,
                locator: serde_json::from_str(&row.try_get::<String, _>("locator_json")?)?,
            })
        })
        .transpose()
    }

    pub async fn accepted_through(&self, observation: &ObservationRef) -> Result<u64> {
        let value: Option<i64> = sqlx::query_scalar("SELECT accepted_through FROM producer_prefixes WHERE issuer_ref = ? AND producer_id = ? AND producer_epoch = ?")
            .bind(serde_json::to_string(&observation.issuer)?).bind(&observation.producer_id).bind(&observation.producer_epoch)
            .fetch_optional(&self.readers).await?;
        unsigned(value.unwrap_or(0))
    }

    pub async fn evidence_cut(&self) -> Result<u64> {
        unsigned(
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq), 0) FROM observations")
                .fetch_one(&self.readers)
                .await?,
        )
    }

    /// Bounded log work, including tombstone holes. The worker advances over
    /// deleted rows without mistaking them for an empty completed archive.
    pub async fn admissions_after(&self, after: u64, limit: usize) -> Result<AdmissionPage> {
        self.admissions_page(after, limit, None).await
    }

    async fn admissions_page(
        &self,
        after: u64,
        limit: usize,
        interleave: Option<(&tokio::sync::Notify, &tokio::sync::Notify)>,
    ) -> Result<AdmissionPage> {
        if limit == 0 || limit > self.limits.max_read_items {
            return Err(EvidenceError::Limit("admission read items"));
        }
        // A page and its cut come from one SQLite read snapshot. Sampling the cut
        // after releasing the rows would report an unseen concurrent admission as
        // already covered by a page whose has_more is false.
        let mut tx = self.readers.begin().await?;
        let cut: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq), 0) FROM observations")
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query("SELECT o.admission_seq, o.occurrence_id, o.envelope_bytes, c.canonical_observation_ref, d.tombstone_id FROM observations o JOIN occurrences c ON c.occurrence_id = o.occurrence_id LEFT JOIN ingestion_epoch_tombstones d ON d.epoch = c.ingestion_epoch WHERE o.admission_seq > ? ORDER BY o.admission_seq LIMIT ?")
            .bind(signed(after)?).bind((limit + 1) as i64).fetch_all(&mut *tx).await?;
        if let Some((read, resume)) = interleave {
            read.notify_one();
            resume.notified().await;
        }
        let cut = unsigned(cut)?;
        let mut result = AdmissionPage {
            captures: Vec::new(),
            next_after: after,
            has_more: rows.len() > limit,
            evidence_cut: cut,
        };
        let mut bytes = 0usize;
        let mut loaded = Vec::new();
        for row in rows.into_iter().take(limit) {
            let seq = unsigned(row.try_get("admission_seq")?)?;
            if row.try_get::<Option<String>, _>("tombstone_id")?.is_none() {
                let envelope_bytes = unsigned(row.try_get("envelope_bytes")?)? as usize;
                if bytes + envelope_bytes > self.limits.max_read_bytes {
                    if result.next_after == after {
                        return Err(EvidenceError::Limit("admission read bytes"));
                    }
                    result.has_more = true;
                    break;
                }
                // Read a body only after its stored byte extent fits this page.
                let envelope: String = sqlx::query_scalar(
                    "SELECT envelope_json FROM observations WHERE admission_seq = ?",
                )
                .bind(signed(seq)?)
                .fetch_one(&mut *tx)
                .await?;
                bytes += envelope_bytes;
                loaded.push((
                    seq,
                    unsigned(row.try_get("occurrence_id")?)?,
                    row.try_get::<String, _>("canonical_observation_ref")?,
                    envelope,
                ));
            }
            result.next_after = seq;
        }
        tx.commit().await?;
        for (admission_seq, occurrence_id, canonical_ref, envelope) in loaded {
            result.captures.push(AdmittedCapture {
                admission_seq,
                occurrence_id,
                canonical_observation_ref: ObservationRef::decode(&canonical_ref)
                    .map_err(invalid)?,
                capture: serde_json::from_str(&envelope)?,
            });
        }
        Ok(result)
    }

    /// Internal archive read. HTTP readers additionally require a reachable,
    /// authorized object/snapshot anchor before invoking this storage operation.
    pub async fn payload_bytes(&self, payload: &PayloadRef, max_bytes: usize) -> Result<Vec<u8>> {
        let row = sqlx::query("SELECT kind, stored_length, decoded_length, decoded_digest, complete FROM payloads WHERE payload_ref = ?")
            .bind(payload.as_str()).fetch_optional(&self.readers).await?
            .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        if !row.try_get::<bool, _>("complete")? {
            return Err(EvidenceError::UnavailableRange);
        }
        let length = unsigned(row.try_get("decoded_length")?)?;
        if length > max_bytes as u64 {
            return Err(EvidenceError::Limit("payload read bytes"));
        }
        if row.try_get::<String, _>("kind")? == "bytes" {
            return self.verify_bytes(payload, length).await;
        }
        self.verify_bytes(payload, unsigned(row.try_get("stored_length")?)?)
            .await?;
        let chunks = sqlx::query("SELECT chunk_ref, chunk_offset, length, payload_offset FROM payload_chunks WHERE manifest_ref = ? ORDER BY ordinal")
            .bind(payload.as_str()).fetch_all(&self.readers).await?;
        let mut bytes = Vec::with_capacity(length as usize);
        for chunk in chunks {
            let digest: String = chunk.try_get("chunk_ref")?;
            let chunk_ref = PayloadRef::parse(digest).map_err(invalid)?;
            let chunk_bytes = self
                .blobs
                .get(chunk_ref.digest())
                .await
                .map_err(payload_error)?
                .ok_or_else(|| EvidenceError::Payload(chunk_ref.as_str().into()))?;
            let start = unsigned(chunk.try_get("chunk_offset")?)? as usize;
            let count = unsigned(chunk.try_get("length")?)? as usize;
            if unsigned(chunk.try_get("payload_offset")?)? != bytes.len() as u64 {
                return Err(EvidenceError::Payload("non-contiguous manifest".into()));
            }
            let segment = chunk_bytes
                .get(
                    start
                        ..start
                            .checked_add(count)
                            .ok_or_else(|| EvidenceError::Payload("range overflow".into()))?,
                )
                .ok_or_else(|| {
                    EvidenceError::Payload("manifest range exceeds retained bytes".into())
                })?;
            bytes.extend_from_slice(segment);
        }
        if bytes.len() as u64 != length {
            return Err(EvidenceError::Payload(
                "manifest total length mismatch".into(),
            ));
        }
        if blake3::hash(&bytes).to_hex().as_str() != row.try_get::<String, _>("decoded_digest")? {
            return Err(EvidenceError::Payload(
                "decoded manifest digest mismatch".into(),
            ));
        }
        Ok(bytes)
    }

    /// Return immutable representation metadata, including unknown total length
    /// and unavailable ranges. Raw byte objects have no manifest wrapper.
    pub async fn payload_manifest(&self, payload: &PayloadRef) -> Result<Option<PayloadManifest>> {
        let row =
            sqlx::query("SELECT manifest_json, stored_length FROM payloads WHERE payload_ref = ?")
                .bind(payload.as_str())
                .fetch_optional(&self.readers)
                .await?
                .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        self.verify_bytes(payload, unsigned(row.try_get("stored_length")?)?)
            .await?;
        row.try_get::<Option<String>, _>("manifest_json")?
            .map(|json| serde_json::from_str(&json).map_err(EvidenceError::from))
            .transpose()
    }

    /// Indexed decoded length; reading a preview does not materialize the full body.
    pub async fn payload_length(&self, payload: &PayloadRef) -> Result<Option<u64>> {
        let length: Option<i64> =
            sqlx::query_scalar("SELECT decoded_length FROM payloads WHERE payload_ref=?")
                .bind(payload.as_str())
                .fetch_optional(&self.readers)
                .await?
                .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        length.map(unsigned).transpose()
    }

    /// Read one bounded retained range. A gap is an explicit error; it never
    /// becomes a shorter body that appears complete to an interpretation worker.
    pub async fn payload_range(
        &self,
        payload: &PayloadRef,
        offset: u64,
        length: usize,
    ) -> Result<PayloadSlice> {
        if length == 0 || length > self.limits.max_read_bytes {
            return Err(EvidenceError::Limit("payload range bytes"));
        }
        let end = offset
            .checked_add(length as u64)
            .ok_or(EvidenceError::UnavailableRange)?;
        let row = sqlx::query("SELECT kind, stored_length, decoded_length, manifest_json FROM payloads WHERE payload_ref = ?")
            .bind(payload.as_str()).fetch_optional(&self.readers).await?
            .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        let total = row
            .try_get::<Option<i64>, _>("decoded_length")?
            .map(unsigned)
            .transpose()?;
        if total.is_some_and(|total| end > total) {
            return Err(EvidenceError::UnavailableRange);
        }
        if row.try_get::<String, _>("kind")? == "bytes" {
            let bytes = self
                .verify_bytes(payload, unsigned(row.try_get("stored_length")?)?)
                .await?;
            return Ok(PayloadSlice {
                bytes: bytes
                    .get(offset as usize..end as usize)
                    .ok_or(EvidenceError::UnavailableRange)?
                    .to_vec(),
                offset,
                total_length: total,
                coverage: PayloadCompleteness::Complete,
            });
        }
        self.verify_bytes(payload, unsigned(row.try_get("stored_length")?)?)
            .await?;
        let manifest: PayloadManifest =
            serde_json::from_str(&row.try_get::<String, _>("manifest_json")?)?;
        let chunks = sqlx::query("SELECT c.chunk_ref, c.chunk_offset, c.length, c.payload_offset, p.stored_length FROM payload_chunks c JOIN payloads p ON p.payload_ref = c.chunk_ref WHERE c.manifest_ref = ? AND c.payload_offset < ? AND c.payload_offset + c.length > ? ORDER BY c.payload_offset LIMIT ?")
            .bind(payload.as_str()).bind(signed(end)?).bind(signed(offset)?).bind((self.limits.max_manifest_chunks + 1) as i64).fetch_all(&self.readers).await?;
        if chunks.len() > self.limits.max_manifest_chunks {
            return Err(EvidenceError::Limit("payload range chunks"));
        }
        let mut bytes = Vec::with_capacity(length);
        for chunk in chunks {
            let position = unsigned(chunk.try_get("payload_offset")?)?;
            let chunk_length = unsigned(chunk.try_get("length")?)?;
            let start = position.max(offset);
            let stop = (position + chunk_length).min(end);
            if start != offset + bytes.len() as u64 {
                return Err(EvidenceError::UnavailableRange);
            }
            let digest =
                PayloadRef::parse(chunk.try_get::<String, _>("chunk_ref")?).map_err(invalid)?;
            let chunk_bytes = self
                .verify_bytes(&digest, unsigned(chunk.try_get("stored_length")?)?)
                .await?;
            let source_start = unsigned(chunk.try_get("chunk_offset")?)? + start - position;
            let source_end = source_start + stop - start;
            bytes.extend_from_slice(
                chunk_bytes
                    .get(source_start as usize..source_end as usize)
                    .ok_or(EvidenceError::UnavailableRange)?,
            );
        }
        if bytes.len() != length {
            return Err(EvidenceError::UnavailableRange);
        }
        Ok(PayloadSlice {
            bytes,
            offset,
            total_length: total,
            coverage: manifest.completeness,
        })
    }

    pub async fn capture_payload(
        &self,
        observation: &ObservationRef,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>> {
        let payload: Option<String> = sqlx::query_scalar("SELECT c.payload_ref FROM observations o JOIN occurrences c ON c.occurrence_id = o.occurrence_id WHERE o.observation_ref = ? AND NOT EXISTS (SELECT 1 FROM ingestion_epoch_tombstones d WHERE d.epoch = c.ingestion_epoch AND d.tombstone_id IS NOT NULL)")
            .bind(observation.encode()).fetch_optional(&self.readers).await?;
        match payload {
            Some(payload) => Ok(Some(
                self.payload_bytes(&PayloadRef::parse(payload).map_err(invalid)?, max_bytes)
                    .await?,
            )),
            None => Ok(None),
        }
    }

    async fn verify_payload(&self, payload: &PayloadRef) -> Result<()> {
        let row = sqlx::query("SELECT kind, stored_length FROM payloads WHERE payload_ref = ?")
            .bind(payload.as_str())
            .fetch_optional(&self.readers)
            .await?
            .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        self.verify_bytes(payload, unsigned(row.try_get("stored_length")?)?)
            .await?;
        if row.try_get::<String, _>("kind")? == "manifest" {
            let chunks = sqlx::query("SELECT DISTINCT p.payload_ref, p.stored_length FROM payload_chunks c JOIN payloads p ON p.payload_ref = c.chunk_ref WHERE c.manifest_ref = ?")
                .bind(payload.as_str()).fetch_all(&self.readers).await?;
            for chunk in chunks {
                let chunk_ref = PayloadRef::parse(chunk.try_get::<String, _>("payload_ref")?)
                    .map_err(invalid)?;
                self.verify_bytes(&chunk_ref, unsigned(chunk.try_get("stored_length")?)?)
                    .await?;
            }
        }
        Ok(())
    }

    async fn verify_bytes(&self, payload: &PayloadRef, expected_length: u64) -> Result<Vec<u8>> {
        let bytes = self
            .blobs
            .get(payload.digest())
            .await
            .map_err(payload_error)?
            .ok_or_else(|| EvidenceError::Payload(payload.as_str().into()))?;
        if bytes.len() as u64 != expected_length {
            return Err(EvidenceError::Payload("decoded length mismatch".into()));
        }
        Ok(bytes)
    }

    pub async fn close(&self) {
        self.readers.close().await;
        self.writer.close().await;
    }

    async fn begin_blob_write(&self, digest: &str) -> Result<String> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let deleting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_blob_gc WHERE physical_digest=? AND state='deleting')").bind(digest).fetch_one(&mut *tx).await?;
        if deleting {
            return Err(EvidenceError::Payload(
                "physical payload is being collected; retry the upload".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO trace_blob_inventory VALUES(?,0) ON CONFLICT(physical_digest) DO UPDATE SET collected=0").bind(digest).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO trace_blob_writes VALUES(?,?)")
            .bind(&id)
            .bind(digest)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn end_blob_write(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM trace_blob_writes WHERE write_id=?")
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }
}

fn capture_payload_refs(capture: &Capture) -> BTreeSet<&PayloadRef> {
    std::iter::once(&capture.payload_ref)
        .chain(
            capture
                .attachments
                .iter()
                .filter_map(|attachment| attachment.payload_ref.as_ref()),
        )
        .collect()
}

async fn epoch_tombstone(conn: &mut SqliteConnection, epoch: &str) -> Result<Option<String>> {
    let tombstone: Option<Option<String>> =
        sqlx::query_scalar("SELECT tombstone_id FROM ingestion_epoch_tombstones WHERE epoch=?")
            .bind(epoch)
            .fetch_optional(conn)
            .await?;
    Ok(tombstone.flatten())
}

// Preflight and the transaction use the same policy against different read
// snapshots. Only the latter authorizes admission; duplicating this policy would
// eventually make upload eligibility disagree with the durable receipt boundary.
async fn check_inventory(
    conn: &mut SqliteConnection,
    issuer: &Issuer,
    capture: &Capture,
) -> Result<Option<String>> {
    let epoch = capture.ingestion_authorization_epoch.as_str();
    let row = sqlx::query("SELECT s.scope_json, s.issuer_ref, s.current_epoch, s.suppressed, d.tombstone_id FROM ingestion_epochs e JOIN ingestion_scopes s ON s.scope_ref = e.scope_ref LEFT JOIN ingestion_epoch_tombstones d ON d.epoch = e.epoch WHERE e.epoch = ?")
        .bind(epoch).fetch_optional(&mut *conn).await?.ok_or(EvidenceError::Epoch)?;
    let scope: IngestionScope = serde_json::from_str(&row.try_get::<String, _>("scope_json")?)?;
    if let Issuer::Environment(environment) = issuer {
        if environment != scope.environment() {
            return Err(EvidenceError::Authority);
        }
    }
    if (matches!(scope, IngestionScope::Agent(_)) && capture.declared_subject_refs.is_empty())
        || capture
            .declared_subject_refs
            .iter()
            .any(|subject| !scope.permits_subject(subject))
    {
        return Err(EvidenceError::Authority);
    }
    for subject in &capture.declared_subject_refs {
        let included: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM inventory_subjects WHERE epoch = ? AND subject_ref = ?)",
        )
        .bind(epoch)
        .bind(subject.encode())
        .fetch_one(&mut *conn)
        .await?;
        if !included {
            return Err(EvidenceError::Authority);
        }
    }
    let tombstone: Option<String> = row.try_get("tombstone_id")?;
    if tombstone.is_some() {
        return Ok(tombstone);
    }
    if row.try_get::<String, _>("current_epoch")? != epoch
        || row.try_get::<i64, _>("suppressed")? != 0
    {
        return Err(EvidenceError::Epoch);
    }
    Ok(None)
}

async fn producer_prefix(
    conn: &mut SqliteConnection,
    issuer: &str,
    producer: &str,
    epoch: &str,
) -> Result<u64> {
    let value: Option<i64> = sqlx::query_scalar("SELECT accepted_through FROM producer_prefixes WHERE issuer_ref = ? AND producer_id = ? AND producer_epoch = ?")
        .bind(issuer).bind(producer).bind(epoch).fetch_optional(conn).await?;
    unsigned(value.unwrap_or(0))
}

fn disposition(row: &sqlx::sqlite::SqliteRow) -> Result<ReceiptDisposition> {
    match row.try_get::<String, _>("disposition")?.as_str() {
        "retained" => Ok(ReceiptDisposition::Retained {
            admission_seq: unsigned(row.try_get("admission_seq")?)?,
        }),
        "discarded_by_deletion" => Ok(ReceiptDisposition::DiscardedByDeletion {
            tombstone_id: row.try_get("tombstone_id")?,
        }),
        _ => Err(EvidenceError::Invalid(
            "unknown durable receipt disposition".into(),
        )),
    }
}

fn signed(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| EvidenceError::Invalid("integer exceeds SQLite's signed range".into()))
}

fn unsigned(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| EvidenceError::Invalid("negative stored sequence or length".into()))
}

fn invalid(error: impl std::fmt::Display) -> EvidenceError {
    EvidenceError::Invalid(error.to_string())
}

fn payload_error(error: impl std::fmt::Display) -> EvidenceError {
    EvidenceError::Payload(error.to_string())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    BeforePayloadSync,
    AfterPayloadSync,
    BeforeTransaction,
    BeforeCommit,
    AfterCommit,
}

fn fail_at(selected: Option<CrashPoint>, point: CrashPoint) -> Result<()> {
    #[cfg(test)]
    if selected == Some(point) {
        #[cfg(unix)]
        if std::env::var_os("SYLPH_EVIDENCE_CRASH_PROCESS").is_some() {
            // The isolated test process dies inside the production transaction,
            // bypassing Rust/SQLite cleanup. The parent verifies WAL recovery.
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: this targets only this deliberately spawned test process.
            unsafe { kill(std::process::id() as i32, 9) };
            unreachable!("SIGKILL must terminate the crash probe");
        }
        return Err(EvidenceError::Crash(point));
    }
    let _ = (selected, point);
    Ok(())
}

#[cfg(test)]
mod tests;
