//! Durable lexical capture. Source cursors and the queue share the spool transaction.
//!
//! Native files are read without a vendor API or semantic interpretation. Complete
//! newline-terminated records survive malformed JSON, CRLF, duplicate keys and binary
//! bytes. The server's mapper decides what they mean after durable admission.

use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use sylph_protocol::trace::{
    AgentRef, Capture, IngestionEpoch, Issuer, ObservationRef, PayloadChunk, PayloadCompleteness,
    PayloadEncoding, PayloadManifest, PayloadRef, SourceLocator, SourcePosition, SourceRef,
    TimeEvidence, TransportKind,
};
use tokio::sync::Mutex;

use super::{Spool, SpoolError};
use crate::path::PathPolicy;

#[derive(Debug, thiserror::Error)]
pub enum CaptureSpoolError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error("invalid capture acquisition: {0}")]
    Invalid(String),
    #[error("the source changed while it was being captured; retry the pass")]
    ChangedDuringRead,
    #[error("another capture reservation committed; retry the pass")]
    ReservationChanged,
    #[error("source record exceeds the configured {0}-byte capture limit")]
    RecordLimit(usize),
    #[error("capture receipt does not name the queued contiguous producer prefix")]
    ReceiptMismatch,
}

type Result<T> = std::result::Result<T, CaptureSpoolError>;

#[derive(Debug, Clone, Copy)]
pub struct CaptureLimits {
    pub chunk_bytes: usize,
    pub read_bytes: usize,
    pub max_record_bytes: usize,
    pub max_records: usize,
    pub max_pending_read_bytes: usize,
    pub max_manifest_chunks: usize,
    pub max_metadata_bytes: usize,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            chunk_bytes: 64 * 1024,
            read_bytes: 1024 * 1024,
            max_record_bytes: 64 * 1024 * 1024,
            max_records: 128,
            max_pending_read_bytes: 4 * 1024 * 1024,
            max_manifest_chunks: 4096,
            max_metadata_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAuthorization {
    pub epoch: IngestionEpoch,
    pub subjects: Vec<AgentRef>,
    pub captured_at: TimeEvidence,
    pub vendor_build: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RegisteredSource {
    pub reference: SourceRef,
    pub locator: PathBuf,
    pub identity_record_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePass {
    pub observations: Vec<ObservationRef>,
    pub generation: u64,
    pub complete_offset: u64,
    pub pending_tail_bytes: usize,
    pub source_missing: bool,
    pub generation_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedCapture {
    pub capture: Capture,
    pub manifest: Option<PayloadManifest>,
    /// Ordered exact byte chunks. The manifest maps each chunk into the record.
    pub chunks: Vec<(PayloadRef, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedCaptureMetadata {
    pub capture: Capture,
    pub manifest: Option<PayloadManifest>,
    pub chunk_count: usize,
}

/// A terminal receipt may acknowledge retained evidence or an authorized deletion.
/// It never claims that deletion-discarded bytes were admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CaptureReceipt {
    Retained {
        observation: ObservationRef,
        admission_seq: u64,
    },
    DiscardedByDeletion {
        observation: ObservationRef,
        tombstone: String,
    },
}

impl CaptureReceipt {
    fn observation(&self) -> &ObservationRef {
        match self {
            Self::Retained { observation, .. } | Self::DiscardedByDeletion { observation, .. } => {
                observation
            }
        }
    }
}

#[derive(Clone)]
pub struct CaptureRegistry {
    spool: Spool,
    issuer: Issuer,
    producer_id: String,
    producer_epoch: String,
    policy: PathPolicy,
    limits: CaptureLimits,
    reservations: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    #[cfg(test)]
    crash_point: Option<&'static str>,
}

#[derive(Clone)]
struct Cursor {
    revision: i64,
    generation: u64,
    absent: bool,
    identity: Option<String>,
    global_offset: u64,
    global_ordinal: u64,
    prefix_digest: String,
    offset: u64,
    ordinal: u64,
    tail: Vec<u8>,
}

struct Acquired {
    records: Vec<Vec<u8>>,
    tail: Vec<u8>,
    offset: u64,
    ordinal: u64,
    global_offset: u64,
    global_ordinal: u64,
    prefix_digest: String,
    identity: String,
    new_generation: Option<&'static str>,
    missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    created: Option<u128>,
}

impl FileIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            created: metadata
                .created()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_nanos()),
        }
    }
}

/// A snapshot is bounded before allocation and checked twice. Vendors may rewrite a
/// file in place without a lock; a torn read remains a retry, never a settled revision.
fn read_snapshot(path: &Path, maximum: usize) -> Result<Option<(Vec<u8>, String)>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(invalid("snapshot source is not a regular file"));
    }
    if before.len() > maximum as u64 {
        return Err(CaptureSpoolError::RecordLimit(maximum));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(CaptureSpoolError::RecordLimit(maximum));
    }
    let digest = blake3::hash(&bytes);
    file.seek(SeekFrom::Start(0))?;
    let mut verifier = blake3::Hasher::new();
    let copied = std::io::copy(&mut (&mut file).take(maximum as u64 + 1), &mut verifier)?;
    let after = file.metadata()?;
    let current = std::fs::metadata(path).map_err(|_| CaptureSpoolError::ChangedDuringRead)?;
    let identity = FileIdentity::of(&before);
    if copied != bytes.len() as u64
        || before.len() != copied
        || verifier.finalize() != digest
        || FileIdentity::of(&after) != identity
        || FileIdentity::of(&current) != identity
        || after.len() != copied
        || current.len() != copied
        || before.modified().ok() != after.modified().ok()
        || after.modified().ok() != current.modified().ok()
    {
        return Err(CaptureSpoolError::ChangedDuringRead);
    }
    Ok(Some((bytes, serde_json::to_string(&identity)?)))
}

impl CaptureRegistry {
    /// One acquisition runtime gets one execution epoch; reconnect keeps this object.
    /// Old epochs remain in the durable queue and are drained independently on restart.
    pub async fn open(
        spool: Spool,
        issuer: Issuer,
        policy: PathPolicy,
        limits: CaptureLimits,
    ) -> Result<Self> {
        issuer.validate().map_err(invalid)?;
        for bound in [
            limits.chunk_bytes,
            limits.read_bytes,
            limits.max_record_bytes,
            limits.max_records,
            limits.max_pending_read_bytes,
            limits.max_manifest_chunks,
            limits.max_metadata_bytes,
        ] {
            i64::try_from(bound).map_err(invalid)?;
        }
        if limits.chunk_bytes == 0
            || limits.read_bytes == 0
            || limits.max_record_bytes == 0
            || limits.max_records == 0
            || limits.max_pending_read_bytes == 0
            || limits.max_manifest_chunks == 0
            || limits.max_metadata_bytes == 0
            || limits.max_metadata_bytes > limits.max_pending_read_bytes
            || limits.chunk_bytes > limits.max_record_bytes
            || limits.read_bytes > limits.max_record_bytes
        {
            return Err(invalid("inconsistent capture limits"));
        }
        let producer_id = "lexical-capture".to_owned();
        let producer_epoch = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO capture_producers (issuer_json,producer_id,producer_epoch) VALUES (?,?,?)",
        )
        .bind(serde_json::to_string(&issuer)?)
        .bind(&producer_id)
        .bind(&producer_epoch)
        .execute(&spool.pool)
        .await?;
        Ok(Self {
            spool,
            issuer,
            producer_id,
            producer_epoch,
            policy,
            limits,
            reservations: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            crash_point: None,
        })
    }

    /// The durable registry identifies the locator stream; replacement at that path is
    /// a new generation. A copied path gets a different source even for identical bytes.
    pub async fn register_jsonl(
        &self,
        namespace: &str,
        path: impl AsRef<Path>,
    ) -> Result<RegisteredSource> {
        if namespace.is_empty() {
            return Err(invalid("source namespace is empty"));
        }
        let path = self.policy.contain_write_target(path).map_err(invalid)?;
        let locator = path
            .to_str()
            .ok_or_else(|| invalid("native source path is not UTF-8"))?;
        let issuer = serde_json::to_string(&self.issuer)?;
        let source = SourceRef {
            issuer: self.issuer.clone(),
            instance: uuid::Uuid::new_v4().to_string(),
        };
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO capture_sources (source_ref,issuer_json,namespace,locator) VALUES (?,?,?,?) ON CONFLICT(issuer_json,namespace,locator) DO NOTHING")
            .bind(source.encode()).bind(&issuer).bind(namespace).bind(locator).execute(&mut *tx).await?;
        let reference: String = sqlx::query_scalar("SELECT source_ref FROM capture_sources WHERE issuer_json=? AND namespace=? AND locator=?")
            .bind(issuer).bind(namespace).bind(locator).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO capture_generations (source_ref,generation,reason,prefix_digest) VALUES (?,0,'initial',?) ON CONFLICT DO NOTHING")
            .bind(&reference).bind(blake3::hash(&[]).to_hex().as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(RegisteredSource {
            reference: SourceRef::decode(&reference).map_err(invalid)?,
            locator: path,
            identity_record_ordinal: None,
        })
    }

    pub async fn source_locator(&self,source:&SourceRef)->Result<Option<PathBuf>> {
        let path:Option<String>=sqlx::query_scalar("SELECT locator FROM capture_sources WHERE source_ref=?").bind(source.encode()).fetch_optional(&self.spool.readers).await?;Ok(path.map(PathBuf::from))
    }

    /// Keep a small part of the configured spool available for process/capture-loss
    /// evidence after ordinary capture reaches backpressure. The ceiling is still the
    /// configured total, and no observed-only native process is stopped by this policy.
    fn capacity(&self, transport: TransportKind) -> u64 {
        if matches!(
            transport,
            TransportKind::Supervisor | TransportKind::SourceState
        ) {
            self.spool.max_bytes
        } else {
            self.spool.max_bytes - self.spool.max_bytes.saturating_div(8).min(64 * 1024)
        }
    }

    /// Retain one exact runtime frame before vendor parsing or reply routing.
    /// Stream keys include the process incarnation and are stable across reconnect.
    pub async fn capture_frame(
        &self,
        stream: &str,
        transport: TransportKind,
        bytes: Vec<u8>,
        authorization: CaptureAuthorization,
    ) -> Result<ObservationRef> {
        self.capture_frame_with_attachments(stream, transport, bytes, vec![], authorization)
            .await
    }

    pub async fn capture_frame_with_attachments(
        &self,
        stream: &str,
        transport: TransportKind,
        bytes: Vec<u8>,
        attachments: Vec<sylph_protocol::trace::CaptureAttachment>,
        authorization: CaptureAuthorization,
    ) -> Result<ObservationRef> {
        if stream.is_empty() || bytes.len() > self.limits.max_record_bytes {
            return Err(invalid("invalid or oversized runtime frame"));
        }
        let source = SourceRef {
            issuer: self.issuer.clone(),
            instance: format!("runtime:{stream}"),
        };
        let source_key = source.encode();
        let issuer = serde_json::to_string(&self.issuer)?;
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO capture_sources(source_ref,issuer_json,namespace,locator) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
            .bind(&source_key).bind(&issuer).bind("runtime").bind(stream).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO capture_generations(source_ref,generation,reason,prefix_digest) VALUES(?,0,'runtime',?) ON CONFLICT DO NOTHING")
            .bind(&source_key).bind(blake3::hash(&[]).to_hex().as_str()).execute(&mut *tx).await?;
        let ordinal:i64=sqlx::query_scalar("UPDATE capture_generations SET next_ordinal=next_ordinal+1 WHERE source_ref=? AND generation=0 RETURNING next_ordinal-1")
            .bind(&source_key).fetch_one(&mut *tx).await?;
        let mut item = self.prepare(
            &authorization,
            SourcePosition {
                source,
                generation: 0,
                locator: SourceLocator::Frame {
                    ordinal: unsigned(ordinal)?,
                },
            },
            bytes,
        )?;
        item.capture.transport_kind = transport;
        item.capture.attachments = attachments;
        item.capture.validate().map_err(invalid)?;
        let sequence:i64=sqlx::query_scalar("UPDATE capture_producers SET next_seq=next_seq+1 WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND next_seq<9223372036854775807 RETURNING next_seq-1")
            .bind(&issuer).bind(&self.producer_id).bind(&self.producer_epoch).fetch_one(&mut *tx).await?;
        item.capture.observation_ref.producer_seq = unsigned(sequence)?;
        let incoming = item_size(&item)?;
        let used = unsigned(
            sqlx::query_scalar("SELECT bytes FROM spool_usage WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?,
        )?;
        if used.saturating_add(incoming) > self.capacity(transport) {
            return Err(SpoolError::Overflow {
                used,
                incoming,
                maximum: self.capacity(transport),
            }
            .into());
        }
        self.insert(&mut tx, &item).await?;
        tx.commit().await?;
        Ok(item.capture.observation_ref)
    }

    pub async fn capture_snapshot(
        &self,
        source: &RegisteredSource,
        authorization: CaptureAuthorization,
    ) -> Result<CapturePass> {
        self.capture_snapshot_as(source, authorization, TransportKind::NativeSnapshot)
            .await
    }
    pub async fn capture_snapshot_as(
        &self,
        source: &RegisteredSource,
        authorization: CaptureAuthorization,
        transport: TransportKind,
    ) -> Result<CapturePass> {
        let reservation = self.reserve(&source.reference).await;
        let _held = reservation.lock().await;
        let path = self
            .policy
            .contain_write_target(&source.locator)
            .map_err(invalid)?;
        let maximum = self.limits.max_record_bytes;
        let snapshot = tokio::task::spawn_blocking(move || read_snapshot(&path, maximum))
            .await
            .map_err(invalid)??;
        let cursor = self.cursor(source, &authorization.epoch).await?;
        let Some((bytes, identity)) = snapshot else {
            sqlx::query(
                "UPDATE capture_sources SET absent=1,revision=revision+1 WHERE source_ref=?",
            )
            .bind(source.reference.encode())
            .execute(&self.spool.pool)
            .await?;
            return Ok(CapturePass {
                observations: vec![],
                generation: cursor.generation,
                complete_offset: cursor.offset,
                pending_tail_bytes: 0,
                source_missing: true,
                generation_changed: false,
            });
        };
        self.commit_snapshot(source, authorization, cursor, bytes, identity, transport)
            .await
    }

    /// Serialize a vendor-supported read transaction as a typed acquisition snapshot.
    /// The caller retains every column and supplies its observed database file identity.
    pub async fn capture_database_snapshot(
        &self,
        source: &RegisteredSource,
        authorization: CaptureAuthorization,
        bytes: Vec<u8>,
        identity: String,
    ) -> Result<CapturePass> {
        if bytes.len() > self.limits.max_record_bytes {
            return Err(CaptureSpoolError::RecordLimit(self.limits.max_record_bytes));
        }
        let reservation = self.reserve(&source.reference).await;
        let _held = reservation.lock().await;
        let cursor = self.cursor(source, &authorization.epoch).await?;
        self.commit_snapshot(
            source,
            authorization,
            cursor,
            bytes,
            identity,
            TransportKind::NativeDatabase,
        )
        .await
    }

    async fn commit_snapshot(
        &self,
        source: &RegisteredSource,
        authorization: CaptureAuthorization,
        cursor: Cursor,
        bytes: Vec<u8>,
        identity: String,
        transport: TransportKind,
    ) -> Result<CapturePass> {
        let digest = blake3::hash(&bytes).to_hex().to_string();
        if !cursor.absent
            && cursor.identity.as_deref() == Some(&identity)
            && cursor.prefix_digest == digest
            && cursor.offset == bytes.len() as u64
        {
            return Ok(CapturePass {
                observations: vec![],
                generation: cursor.generation,
                complete_offset: cursor.offset,
                pending_tail_bytes: 0,
                source_missing: false,
                generation_changed: false,
            });
        }
        let generation = if cursor.absent
            || cursor
                .identity
                .as_ref()
                .is_some_and(|previous| previous != &identity || cursor.prefix_digest != digest)
        {
            cursor
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid("source generation exhausted"))?
        } else {
            cursor.generation
        };
        let mut item = self.prepare(
            &authorization,
            SourcePosition {
                source: source.reference.clone(),
                generation,
                locator: SourceLocator::Snapshot {
                    revision: generation.to_string(),
                },
            },
            bytes.clone(),
        )?;
        item.capture.transport_kind = transport;
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source_key = source.reference.encode();
        let issuer = serde_json::to_string(&self.issuer)?;
        let changed=sqlx::query("UPDATE capture_sources SET revision=revision+1,generation=?,absent=0 WHERE source_ref=? AND revision=?").bind(signed(generation)?).bind(&source_key).bind(cursor.revision).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(CaptureSpoolError::ReservationChanged);
        }
        sqlx::query("INSERT INTO capture_generations(source_ref,generation,predecessor,reason,prefix_digest) VALUES(?,?,?,'snapshot',?) ON CONFLICT DO NOTHING").bind(&source_key).bind(signed(generation)?).bind(signed(cursor.generation)?).bind(&digest).execute(&mut *tx).await?;
        let sequence:i64=sqlx::query_scalar("UPDATE capture_producers SET next_seq=next_seq+1 WHERE issuer_json=? AND producer_id=? AND producer_epoch=? RETURNING next_seq-1").bind(&issuer).bind(&self.producer_id).bind(&self.producer_epoch).fetch_one(&mut *tx).await?;
        item.capture.observation_ref.producer_seq = unsigned(sequence)?;
        let incoming = item_size(&item)?;
        let used = unsigned(
            sqlx::query_scalar("SELECT bytes FROM spool_usage WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?,
        )?;
        if used.saturating_add(incoming) > self.capacity(transport) {
            return Err(SpoolError::Overflow {
                used,
                incoming,
                maximum: self.capacity(transport),
            }
            .into());
        }
        self.insert(&mut tx, &item).await?;
        sqlx::query("INSERT INTO capture_snapshot_revisions(source_ref,generation,ingestion_epoch,capture_json) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
            .bind(&source_key).bind(signed(generation)?).bind(authorization.epoch.as_str()).bind(serde_json::to_string(&item.capture)?).execute(&mut *tx).await?;
        sqlx::query("UPDATE capture_generations SET file_identity=?,complete_offset=?,next_ordinal=1,prefix_digest=? WHERE source_ref=? AND generation=?").bind(&identity).bind(bytes.len()as i64).bind(digest).bind(&source_key).bind(signed(generation)?).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO capture_epoch_progress(source_ref,generation,ingestion_epoch,complete_offset,next_ordinal) VALUES(?,?,?,?,1) ON CONFLICT(source_ref,generation,ingestion_epoch) DO UPDATE SET complete_offset=excluded.complete_offset,next_ordinal=1")
            .bind(&source_key).bind(signed(generation)?).bind(authorization.epoch.as_str()).bind(bytes.len()as i64).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(CapturePass {
            observations: vec![item.capture.observation_ref],
            generation,
            complete_offset: bytes.len() as u64,
            pending_tail_bytes: 0,
            source_missing: false,
            generation_changed: generation != cursor.generation,
        })
    }

    /// The source cursor and generation come from the same committed spool row, including
    /// complete records whose vendor kind has no semantic mapper.
    pub async fn source_extent(
        &self,
        source: &RegisteredSource,
        epoch: &IngestionEpoch,
    ) -> Result<(u64, u64, u64, u64, bool)> {
        let cursor = self.cursor(source, epoch).await?;
        Ok((
            cursor.generation,
            cursor.ordinal,
            cursor.offset,
            cursor.tail.len() as u64,
            cursor.absent,
        ))
    }

    pub async fn snapshot_capture(
        &self,
        source: &SourceRef,
        generation: u64,
        epoch: &IngestionEpoch,
    ) -> Result<Option<Capture>> {
        let value:Option<String>=sqlx::query_scalar("SELECT capture_json FROM capture_snapshot_revisions WHERE source_ref=? AND generation=? AND ingestion_epoch=?")
            .bind(source.encode()).bind(signed(generation)?).bind(epoch.as_str()).fetch_optional(&self.spool.readers).await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(CaptureSpoolError::from))
            .transpose()
    }

    pub async fn source_proof(
        &self,
        source: &SourceRef,
        generation: u64,
    ) -> Result<Option<ObservationRef>> {
        let value:Option<String>=sqlx::query_scalar("SELECT observation_json FROM capture_source_proofs WHERE source_ref=? AND generation=?").bind(source.encode()).bind(signed(generation)?).fetch_optional(&self.spool.readers).await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(CaptureSpoolError::from))
            .transpose()
    }
    async fn reserve(&self, source: &SourceRef) -> Arc<Mutex<()>> {
        let key = source.encode();
        let mut reservations = self.reservations.lock().await;
        reservations.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = reservations.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        reservations.insert(key, Arc::downgrade(&lock));
        lock
    }

    /// Reads and hashes outside the writer transaction. The revision compare in commit
    /// covers independent registry handles as well as this process's shared reservation.
    pub async fn capture_jsonl(
        &self,
        source: &RegisteredSource,
        authorization: CaptureAuthorization,
    ) -> Result<CapturePass> {
        if source.reference.issuer != self.issuer {
            return Err(invalid("source belongs to another issuer"));
        }
        authorization.captured_at.validate().map_err(invalid)?;
        for subject in &authorization.subjects {
            subject.validate().map_err(invalid)?;
            if let Issuer::Environment(environment) = &self.issuer {
                if subject.environment() != environment {
                    return Err(invalid("subject belongs to another environment"));
                }
            }
        }
        let reservation = self.reserve(&source.reference).await;
        let _held = reservation.lock().await;
        for _ in 0..4 {
            let cursor = self.cursor(source, &authorization.epoch).await?;
            let path = self
                .policy
                .contain_write_target(&source.locator)
                .map_err(invalid)?;
            let read_cursor = cursor.clone();
            let limits = self.limits;
            let acquired =
                tokio::task::spawn_blocking(move || acquire(&path, &read_cursor, limits))
                    .await
                    .map_err(invalid)??;
            match self.commit(source, &authorization, &cursor, acquired).await {
                Err(CaptureSpoolError::ReservationChanged) => continue,
                other => return other,
            }
        }
        Err(CaptureSpoolError::ReservationChanged)
    }

    async fn cursor(&self, source: &RegisteredSource, epoch: &IngestionEpoch) -> Result<Cursor> {
        let row = sqlx::query("SELECT s.revision,s.generation,s.absent,s.locator,g.file_identity,g.complete_offset AS global_offset,g.next_ordinal AS global_ordinal,g.prefix_digest,p.complete_offset,p.next_ordinal,p.tail FROM capture_sources s JOIN capture_generations g ON g.source_ref=s.source_ref AND g.generation=s.generation LEFT JOIN capture_epoch_progress p ON p.source_ref=s.source_ref AND p.generation=s.generation AND p.ingestion_epoch=? WHERE s.source_ref=?")
            .bind(epoch.as_str()).bind(source.reference.encode()).fetch_one(&self.spool.readers).await?;
        if row.try_get::<String, _>("locator")?
            != source
                .locator
                .to_str()
                .ok_or_else(|| invalid("invalid source locator"))?
        {
            return Err(invalid("source locator was changed"));
        }
        Ok(Cursor {
            revision: row.try_get("revision")?,
            generation: unsigned(row.try_get("generation")?)?,
            absent: row.try_get::<i64, _>("absent")? != 0,
            identity: row.try_get("file_identity")?,
            global_offset: unsigned(row.try_get("global_offset")?)?,
            global_ordinal: unsigned(row.try_get("global_ordinal")?)?,
            prefix_digest: row.try_get("prefix_digest")?,
            offset: unsigned(
                row.try_get::<Option<i64>, _>("complete_offset")?
                    .unwrap_or(0),
            )?,
            ordinal: unsigned(row.try_get::<Option<i64>, _>("next_ordinal")?.unwrap_or(0))?,
            tail: row
                .try_get::<Option<Vec<u8>>, _>("tail")?
                .unwrap_or_default(),
        })
    }

    async fn commit(
        &self,
        source: &RegisteredSource,
        authorization: &CaptureAuthorization,
        cursor: &Cursor,
        acquired: Acquired,
    ) -> Result<CapturePass> {
        let generation = if acquired.new_generation.is_some() {
            cursor
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid("source generation exhausted"))?
        } else {
            cursor.generation
        };
        let mut prepared = Vec::new();
        let mut offset = if acquired.new_generation.is_some() {
            0
        } else {
            cursor.offset
        };
        let mut ordinal = if acquired.new_generation.is_some() {
            0
        } else {
            cursor.ordinal
        };
        for record in acquired.records {
            let position = SourcePosition {
                source: source.reference.clone(),
                generation,
                locator: SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + record.len() as u64,
                    record_ordinal: ordinal,
                },
            };
            prepared.push(self.prepare(authorization, position, record)?);
            offset += prepared
                .last()
                .unwrap()
                .chunks
                .iter()
                .map(|(_, bytes)| bytes.len() as u64)
                .sum::<u64>();
            ordinal += 1;
        }
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source_key = source.reference.encode();
        let changed = sqlx::query(
            "UPDATE capture_sources SET revision=revision+1 WHERE source_ref=? AND revision=?",
        )
        .bind(&source_key)
        .bind(cursor.revision)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(CaptureSpoolError::ReservationChanged);
        }
        if acquired.missing {
            sqlx::query("UPDATE capture_sources SET absent=1 WHERE source_ref=?")
                .bind(&source_key)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(CapturePass {
                observations: vec![],
                generation: cursor.generation,
                complete_offset: cursor.offset,
                pending_tail_bytes: cursor.tail.len(),
                source_missing: true,
                generation_changed: false,
            });
        }
        if let Some(reason) = acquired.new_generation {
            sqlx::query("INSERT INTO capture_generations(source_ref,generation,predecessor,reason,prefix_digest) VALUES (?,?,?,?,?)")
                .bind(&source_key).bind(signed(generation)?).bind(signed(cursor.generation)?).bind(reason).bind(blake3::hash(&[]).to_hex().as_str()).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE capture_sources SET generation=?,absent=0 WHERE source_ref=?")
            .bind(signed(generation)?)
            .bind(&source_key)
            .execute(&mut *tx)
            .await?;
        for item in &mut prepared {
            let seq:i64=sqlx::query_scalar("UPDATE capture_producers SET next_seq=next_seq+1 WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND next_seq<9223372036854775807 RETURNING next_seq-1")
                .bind(serde_json::to_string(&self.issuer)?).bind(&self.producer_id).bind(&self.producer_epoch).fetch_one(&mut *tx).await?;
            item.capture.observation_ref.producer_seq = unsigned(seq)?;
        }
        let old_tail_length = if acquired.new_generation.is_some() {
            0
        } else {
            cursor.tail.len()
        };
        let incoming = prepared
            .iter()
            .try_fold(acquired.tail.len() as u64, |sum, item| {
                sum.checked_add(item_size(item)?)
                    .ok_or_else(|| invalid("capture size overflow"))
            })?;
        let used: i64 = sqlx::query_scalar("SELECT bytes FROM spool_usage WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
        let used = unsigned(used)?.saturating_sub(old_tail_length as u64);
        if used
            .checked_add(incoming)
            .is_none_or(|sum| sum > self.capacity(TransportKind::NativeJsonl))
        {
            return Err(SpoolError::Overflow {
                used,
                incoming,
                maximum: self.capacity(TransportKind::NativeJsonl),
            }
            .into());
        }
        for item in &prepared {
            self.insert(&mut tx, item).await?;
        }
        if let Some(first) = prepared.iter().find(|item| matches!(item.capture.source_position.locator,
            SourceLocator::Jsonl { record_ordinal, .. } if Some(record_ordinal) == source.identity_record_ordinal)) {
            sqlx::query("INSERT INTO capture_source_proofs(source_ref,generation,observation_json) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(&source_key).bind(signed(generation)?).bind(serde_json::to_string(&first.capture.observation_ref)?).execute(&mut *tx).await?;
        }

        sqlx::query("INSERT INTO capture_epoch_progress (source_ref,generation,ingestion_epoch,complete_offset,next_ordinal,tail) VALUES (?,?,?,?,?,?) ON CONFLICT(source_ref,generation,ingestion_epoch) DO UPDATE SET complete_offset=excluded.complete_offset,next_ordinal=excluded.next_ordinal,tail=excluded.tail")
            .bind(&source_key).bind(signed(generation)?).bind(authorization.epoch.as_str()).bind(signed(acquired.offset)?).bind(signed(acquired.ordinal)?).bind(&acquired.tail).execute(&mut *tx).await?;
        sqlx::query("UPDATE capture_generations SET file_identity=?,complete_offset=?,next_ordinal=?,prefix_digest=? WHERE source_ref=? AND generation=?")
            .bind(acquired.identity).bind(signed(acquired.global_offset)?).bind(signed(acquired.global_ordinal)?).bind(acquired.prefix_digest).bind(&source_key).bind(signed(generation)?).execute(&mut *tx).await?;
        #[cfg(test)]
        self.crash_at("before_commit");
        tx.commit().await?;
        #[cfg(test)]
        self.crash_at("after_commit");
        Ok(CapturePass {
            observations: prepared
                .into_iter()
                .map(|item| item.capture.observation_ref)
                .collect(),
            generation,
            complete_offset: acquired.offset,
            pending_tail_bytes: acquired.tail.len(),
            source_missing: false,
            generation_changed: acquired.new_generation.is_some(),
        })
    }

    #[cfg(test)]
    fn crash_at(&self, point: &str) {
        if self.crash_point != Some(point) {
            return;
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: only the isolated crash probe enables this field. SIGKILL
            // skips Rust and SQLite cleanup so the parent tests actual WAL recovery.
            unsafe { kill(std::process::id() as i32, 9) };
        }
        unreachable!("the isolated capture crash probe must terminate");
    }

    fn prepare(
        &self,
        authorization: &CaptureAuthorization,
        position: SourcePosition,
        bytes: Vec<u8>,
    ) -> Result<QueuedCapture> {
        if bytes.len().div_ceil(self.limits.chunk_bytes) > self.limits.max_manifest_chunks {
            return Err(invalid("record exceeds configured manifest chunk count"));
        }
        let chunks: Vec<_> = bytes
            .chunks(self.limits.chunk_bytes)
            .map(|bytes| {
                Ok((
                    PayloadRef::bytes(blake3::hash(bytes).to_hex()).map_err(invalid)?,
                    bytes.to_vec(),
                ))
            })
            .collect::<Result<_>>()?;
        let manifest = if chunks.len() > 1 {
            Some(PayloadManifest {
                chunks: chunks
                    .iter()
                    .enumerate()
                    .map(|(index, (digest, bytes))| PayloadChunk {
                        digest: digest.clone(),
                        chunk_offset: 0,
                        length: bytes.len() as u64,
                        payload_offset: (index * self.limits.chunk_bytes) as u64,
                    })
                    .collect(),
                media_type: "application/x-ndjson".into(),
                encoding: PayloadEncoding::Binary,
                decoded_length: Some(bytes.len() as u64),
                completeness: PayloadCompleteness::Complete,
            })
        } else {
            None
        };
        let payload_ref = match &manifest {
            Some(manifest) => PayloadRef::manifest(
                blake3::hash(&manifest.canonical_bytes().map_err(invalid)?).to_hex(),
            )
            .map_err(invalid)?,
            None => chunks
                .first()
                .ok_or_else(|| invalid("empty record"))?
                .0
                .clone(),
        };
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: self.issuer.clone(),
                producer_id: self.producer_id.clone(),
                producer_epoch: self.producer_epoch.clone(),
                // Assigned from the durable producer allocator before this value leaves commit.
                producer_seq: 1,
            },
            source_position: position,
            ingestion_authorization_epoch: authorization.epoch.clone(),
            declared_subject_refs: authorization.subjects.clone(),
            captured_at: authorization.captured_at.clone(),
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: authorization.vendor_build.clone(),
            schema_fingerprint: None,
            payload_ref,
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        capture.validate().map_err(invalid)?;
        let item = QueuedCapture {
            capture,
            manifest,
            chunks,
        };
        if item_metadata_size(&item)? > self.limits.max_metadata_bytes as u64 {
            return Err(invalid("capture metadata exceeds configured byte limit"));
        }
        Ok(item)
    }

    async fn insert(&self, tx: &mut Transaction<'_, Sqlite>, item: &QueuedCapture) -> Result<()> {
        let observation = &item.capture.observation_ref;
        let issuer = serde_json::to_string(&observation.issuer)?;
        let manifest = item.manifest.as_ref().map(serde_json::to_vec).transpose()?;
        sqlx::query("INSERT INTO capture_queue (issuer_json,producer_id,producer_epoch,producer_seq,source_ref,generation,ingestion_epoch,position_key,capture_json,manifest_json,encoded_bytes,metadata_bytes,chunk_count) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&issuer).bind(&observation.producer_id).bind(&observation.producer_epoch).bind(signed(observation.producer_seq)?).bind(item.capture.source_position.source.encode()).bind(signed(item.capture.source_position.generation)?).bind(item.capture.ingestion_authorization_epoch.as_str()).bind(item.capture.source_position.encode()).bind(serde_json::to_vec(&item.capture)?).bind(manifest).bind(signed(item_size(item)?)?).bind(signed(item_metadata_size(item)?)?).bind(item.chunks.len()as i64).execute(&mut **tx).await?;
        for (ordinal, (digest, bytes)) in item.chunks.iter().enumerate() {
            sqlx::query("INSERT INTO capture_payload_chunks (issuer_json,producer_id,producer_epoch,producer_seq,ordinal,digest,bytes) VALUES (?,?,?,?,?,?,?)")
                .bind(&issuer).bind(&observation.producer_id).bind(&observation.producer_epoch).bind(signed(observation.producer_seq)?).bind(ordinal as i64).bind(digest.as_str()).bind(bytes).execute(&mut **tx).await?;
        }
        Ok(())
    }

    /// Large records can be exported without loading their entire body. Each
    /// returned manifest is immutable until its terminal receipt releases the queue.
    pub async fn pending_metadata(&self, maximum: usize) -> Result<Vec<QueuedCaptureMetadata>> {
        self.pending_metadata_through(maximum, None).await
    }
    pub async fn pending_frontiers(&self) -> Result<Vec<ObservationRef>> {
        let rows=sqlx::query("SELECT producer_id,producer_epoch,max(producer_seq) AS through FROM capture_queue WHERE issuer_json=? GROUP BY producer_id,producer_epoch")
            .bind(serde_json::to_string(&self.issuer)?).fetch_all(&self.spool.readers).await?;
        rows.into_iter()
            .map(|row| {
                Ok(ObservationRef {
                    issuer: self.issuer.clone(),
                    producer_id: row.try_get("producer_id")?,
                    producer_epoch: row.try_get("producer_epoch")?,
                    producer_seq: unsigned(row.try_get("through")?)?,
                })
            })
            .collect()
    }
    pub async fn pending_metadata_through(
        &self,
        maximum: usize,
        through: Option<&[ObservationRef]>,
    ) -> Result<Vec<QueuedCaptureMetadata>> {
        if maximum == 0 || maximum > self.limits.max_records {
            return Err(invalid("invalid capture read item limit"));
        }
        let mut tx = self.spool.readers.begin().await?;
        let limits = through.map(serde_json::to_string).transpose()?;
        let rows=sqlx::query("SELECT producer_id,producer_epoch,producer_seq,metadata_bytes FROM capture_queue q WHERE issuer_json=? AND (? IS NULL OR EXISTS(SELECT 1 FROM json_each(?) p WHERE json_extract(p.value,'$.producerId')=q.producer_id AND json_extract(p.value,'$.producerEpoch')=q.producer_epoch AND q.producer_seq<=json_extract(p.value,'$.producerSeq'))) ORDER BY producer_id,producer_epoch,producer_seq LIMIT ?")
            .bind(serde_json::to_string(&self.issuer)?).bind(&limits).bind(&limits).bind(maximum as i64).fetch_all(&mut *tx).await?;
        let mut result = Vec::new();
        let mut used = 0usize;
        for row in rows {
            let size =
                usize::try_from(row.try_get::<i64, _>("metadata_bytes")?).map_err(invalid)?;
            if used
                .checked_add(size)
                .is_none_or(|next| next > self.limits.max_pending_read_bytes)
            {
                if result.is_empty() {
                    return Err(invalid("capture metadata exceeds read budget"));
                }
                break;
            }
            let body=sqlx::query("SELECT capture_json,manifest_json,chunk_count FROM capture_queue WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=?")
                .bind(serde_json::to_string(&self.issuer)?).bind(row.try_get::<String,_>("producer_id")?).bind(row.try_get::<String,_>("producer_epoch")?).bind(row.try_get::<i64,_>("producer_seq")?).fetch_one(&mut *tx).await?;
            result.push(QueuedCaptureMetadata {
                capture: serde_json::from_slice(body.try_get("capture_json")?)?,
                manifest: body
                    .try_get::<Option<Vec<u8>>, _>("manifest_json")?
                    .map(|bytes| serde_json::from_slice(&bytes))
                    .transpose()?,
                chunk_count: usize::try_from(body.try_get::<i64, _>("chunk_count")?)
                    .map_err(invalid)?,
            });
            used += size;
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn queued_chunk(
        &self,
        observation: &ObservationRef,
        ordinal: usize,
    ) -> Result<Option<(PayloadRef, Vec<u8>)>> {
        observation.validate().map_err(invalid)?;
        if observation.issuer != self.issuer {
            return Err(invalid("capture belongs to another issuer"));
        }
        let row=sqlx::query("SELECT digest,bytes FROM capture_payload_chunks WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=? AND ordinal=?")
            .bind(serde_json::to_string(&observation.issuer)?).bind(&observation.producer_id).bind(&observation.producer_epoch).bind(signed(observation.producer_seq)?).bind(i64::try_from(ordinal).map_err(invalid)?).fetch_optional(&self.spool.readers).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let bytes: Vec<u8> = row.try_get("bytes")?;
        if bytes.len() > self.limits.chunk_bytes {
            return Err(invalid("queued chunk exceeds capture limit"));
        }
        let digest = PayloadRef::parse(row.try_get::<String, _>("digest")?).map_err(invalid)?;
        if blake3::hash(&bytes).to_hex().as_str() != digest.digest() {
            return Err(invalid("queued chunk digest mismatch"));
        }
        Ok(Some((digest, bytes)))
    }

    /// Reads are bounded by metadata before any potentially large payload is loaded.
    pub async fn pending(&self, maximum: usize) -> Result<Vec<QueuedCapture>> {
        if maximum == 0 || maximum > self.limits.max_records {
            return Err(invalid("invalid capture read item limit"));
        }
        let mut tx = self.spool.readers.begin().await?;
        let rows=sqlx::query("SELECT issuer_json,producer_id,producer_epoch,producer_seq,encoded_bytes FROM capture_queue WHERE issuer_json=? ORDER BY issuer_json,producer_id,producer_epoch,producer_seq LIMIT ?")
            .bind(serde_json::to_string(&self.issuer)?).bind(maximum as i64).fetch_all(&mut *tx).await?;
        let mut used = 0;
        let mut result = Vec::new();
        for row in rows {
            let size = usize::try_from(row.try_get::<i64, _>("encoded_bytes")?).map_err(invalid)?;
            if used + size > self.limits.max_pending_read_bytes {
                if result.is_empty() {
                    return Err(invalid(
                        "queued capture requires a larger bounded read budget",
                    ));
                }
                break;
            }
            let issuer: String = row.try_get("issuer_json")?;
            let producer: String = row.try_get("producer_id")?;
            let epoch: String = row.try_get("producer_epoch")?;
            let sequence: i64 = row.try_get("producer_seq")?;
            let body=sqlx::query("SELECT capture_json,manifest_json FROM capture_queue WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=?")
                .bind(&issuer).bind(&producer).bind(&epoch).bind(sequence).fetch_optional(&mut *tx).await?;
            let Some(body) = body else {
                continue;
            };
            let chunks=sqlx::query("SELECT digest,bytes FROM capture_payload_chunks WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=? ORDER BY ordinal")
                .bind(&issuer).bind(&producer).bind(&epoch).bind(sequence).fetch_all(&mut *tx).await?;
            let chunks = chunks
                .into_iter()
                .map(|row| {
                    let digest =
                        PayloadRef::parse(row.try_get::<String, _>("digest")?).map_err(invalid)?;
                    let bytes: Vec<u8> = row.try_get("bytes")?;
                    if blake3::hash(&bytes).to_hex().as_str() != digest.digest() {
                        return Err(invalid("queued chunk digest mismatch"));
                    }
                    Ok((digest, bytes))
                })
                .collect::<Result<_>>()?;
            result.push(QueuedCapture {
                capture: serde_json::from_slice(body.try_get("capture_json")?)?,
                manifest: body
                    .try_get::<Option<Vec<u8>>, _>("manifest_json")?
                    .map(|bytes| serde_json::from_slice(&bytes))
                    .transpose()?,
                chunks,
            });
            used += size;
        }
        tx.commit().await?;
        Ok(result)
    }

    pub async fn acknowledge(&self, receipts: &[CaptureReceipt]) -> Result<usize> {
        if receipts.is_empty() {
            return Ok(0);
        }
        let first = receipts[0].observation();
        if first.issuer != self.issuer {
            return Err(CaptureSpoolError::ReceiptMismatch);
        }
        let issuer = serde_json::to_string(&first.issuer)?;
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut through:i64=sqlx::query_scalar("SELECT acked_through FROM capture_producers WHERE issuer_json=? AND producer_id=? AND producer_epoch=?")
            .bind(&issuer).bind(&first.producer_id).bind(&first.producer_epoch).fetch_one(&mut *tx).await?;
        let mut deleted = 0;
        for receipt in receipts {
            let observation = receipt.observation();
            if observation.issuer != first.issuer
                || observation.producer_id != first.producer_id
                || observation.producer_epoch != first.producer_epoch
            {
                return Err(CaptureSpoolError::ReceiptMismatch);
            }
            match receipt {
                CaptureReceipt::Retained { admission_seq, .. } if *admission_seq == 0 => {
                    return Err(CaptureSpoolError::ReceiptMismatch)
                }
                CaptureReceipt::DiscardedByDeletion { tombstone, .. } if tombstone.is_empty() => {
                    return Err(CaptureSpoolError::ReceiptMismatch)
                }
                _ => {}
            }
            let sequence = signed(observation.producer_seq)?;
            let receipt_json = serde_json::to_string(receipt)?;
            if sequence <= through {
                let prior:Option<String>=sqlx::query_scalar("SELECT receipt_json FROM capture_receipts WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=?")
                    .bind(&issuer).bind(&first.producer_id).bind(&first.producer_epoch).bind(sequence).fetch_optional(&mut *tx).await?;
                if prior.as_deref() != Some(receipt_json.as_str()) {
                    return Err(CaptureSpoolError::ReceiptMismatch);
                }
                continue;
            }
            if sequence != through + 1 {
                return Err(CaptureSpoolError::ReceiptMismatch);
            }
            sqlx::query("INSERT INTO capture_receipts (issuer_json,producer_id,producer_epoch,producer_seq,receipt_json) VALUES (?,?,?,?,?)")
                .bind(&issuer).bind(&first.producer_id).bind(&first.producer_epoch).bind(sequence).bind(receipt_json).execute(&mut *tx).await?;
            let removed=sqlx::query("DELETE FROM capture_queue WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND producer_seq=?")
                .bind(&issuer).bind(&first.producer_id).bind(&first.producer_epoch).bind(sequence).execute(&mut *tx).await?.rows_affected();
            if removed != 1 {
                return Err(CaptureSpoolError::ReceiptMismatch);
            }
            through = sequence;
            deleted += 1;
        }
        sqlx::query("UPDATE capture_producers SET acked_through=? WHERE issuer_json=? AND producer_id=? AND producer_epoch=?")
            .bind(through).bind(&issuer).bind(&first.producer_id).bind(&first.producer_epoch).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(deleted)
    }
}

fn acquire(path: &Path, cursor: &Cursor, limits: CaptureLimits) -> Result<Acquired> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Acquired {
                records: vec![],
                tail: vec![],
                offset: cursor.offset,
                ordinal: cursor.ordinal,
                global_offset: cursor.global_offset,
                global_ordinal: cursor.global_ordinal,
                prefix_digest: cursor.prefix_digest.clone(),
                identity: String::new(),
                new_generation: None,
                missing: true,
            })
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("capture source is not a regular file"));
    }
    let identity = serde_json::to_string(&FileIdentity::of(&metadata))?;
    let mut reason = if cursor.absent && cursor.identity.is_some() {
        Some("reappeared")
    } else if cursor
        .identity
        .as_ref()
        .is_some_and(|previous| previous != &identity)
    {
        Some("replaced")
    } else {
        None
    };
    if reason.is_none()
        && hash_prefix(&mut file, cursor.global_offset, limits.chunk_bytes)? != cursor.prefix_digest
    {
        reason = Some("prefix_changed");
    }
    let mut offset = if reason.is_some() { 0 } else { cursor.offset };
    let mut ordinal = if reason.is_some() { 0 } else { cursor.ordinal };
    let mut tail = if reason.is_some() {
        vec![]
    } else {
        cursor.tail.clone()
    };
    if !tail.is_empty() {
        file.seek(SeekFrom::Start(offset))?;
        let mut observed = vec![0; tail.len()];
        if file.read_exact(&mut observed).is_err() || observed != tail {
            reason = Some("staged_prefix_changed");
            offset = 0;
            ordinal = 0;
            tail.clear();
        }
    }
    let initial_offset = offset;
    file.seek(SeekFrom::Start(offset + tail.len() as u64))?;
    let mut read = vec![0; limits.read_bytes];
    let count = file.read(&mut read)?;
    read.truncate(count);
    tail.extend_from_slice(&read);
    let mut records = Vec::new();
    let mut consumed = 0;
    for line in tail.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") || records.len() == limits.max_records {
            break;
        }
        if line.len() > limits.max_record_bytes {
            return Err(CaptureSpoolError::RecordLimit(limits.max_record_bytes));
        }
        records.push(line.to_vec());
        consumed += line.len();
        offset += line.len() as u64;
        ordinal += 1;
    }
    let staged = tail[consumed..].to_vec();
    if staged.len() > limits.max_record_bytes {
        return Err(CaptureSpoolError::RecordLimit(limits.max_record_bytes));
    }
    // Verify the exact read interval again. A concurrent rewrite cannot install
    // a cursor whose prefix digest describes different bytes from the queue.
    file.seek(SeekFrom::Start(initial_offset))?;
    let mut verify = vec![0; tail.len()];
    if file.read_exact(&mut verify).is_err() || verify != tail {
        return Err(CaptureSpoolError::ChangedDuringRead);
    }
    let global_offset = if reason.is_some() {
        offset
    } else {
        cursor.global_offset.max(offset)
    };
    let global_ordinal = if reason.is_some() || offset > cursor.global_offset {
        ordinal
    } else {
        cursor.global_ordinal
    };
    let prefix_digest = hash_prefix(&mut file, global_offset, limits.chunk_bytes)?;
    if reason.is_none()
        && hash_prefix(&mut file, cursor.global_offset, limits.chunk_bytes)? != cursor.prefix_digest
    {
        return Err(CaptureSpoolError::ChangedDuringRead);
    }
    let final_metadata = file.metadata()?;
    if FileIdentity::of(&final_metadata) != FileIdentity::of(&metadata)
        || final_metadata.len() != metadata.len()
        || final_metadata.modified().ok() != metadata.modified().ok()
    {
        return Err(CaptureSpoolError::ChangedDuringRead);
    }
    Ok(Acquired {
        records,
        tail: staged,
        offset,
        ordinal,
        global_offset,
        global_ordinal,
        prefix_digest,
        identity,
        new_generation: reason,
        missing: false,
    })
}

fn hash_prefix(file: &mut std::fs::File, length: u64, buffer_size: usize) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; buffer_size];
    while remaining > 0 {
        let requested = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Ok("truncated".into());
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn item_metadata_size(item: &QueuedCapture) -> Result<u64> {
    Ok((serde_json::to_vec(&item.capture)?.len()
        + item
            .manifest
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .map_or(0, |bytes| bytes.len())) as u64)
}
fn item_size(item: &QueuedCapture) -> Result<u64> {
    item.chunks
        .iter()
        .try_fold(item_metadata_size(item)?, |sum, (_, bytes)| {
            sum.checked_add(bytes.len() as u64)
                .ok_or_else(|| invalid("capture byte count overflow"))
        })
}
fn invalid(error: impl std::fmt::Display) -> CaptureSpoolError {
    CaptureSpoolError::Invalid(error.to_string())
}
fn signed(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(invalid)
}
fn unsigned(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(invalid)
}

#[cfg(test)]
mod tests;
