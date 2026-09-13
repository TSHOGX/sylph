//! The warm tier: consolidated blocks, and the consolidation that fills it.
//!
//! Two rules govern this module, and both are ordering rules rather than data
//! rules.
//!
//! **History is durable before the journal forgets.** Consolidation inserts into
//! `history.db`, commits, and only then deletes the journal rows. A crash between
//! the two leaves the turn in both stores, which is harmless because the insert
//! keys on `(session_id, seq)` and re-running is idempotent. A crash in the other
//! order loses the turn outright.
//!
//! **Blobs are written before the block that references them.** A block pointing
//! at an absent hash is unreadable; a blob nothing points at is garbage the
//! refcount sweep collects. One of those is a bug and the other is housekeeping.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::Row;
use tracing::{debug, info, warn};

use crate::events::data_from_column;
use crate::models::now_ms;
use crate::models::Event;
use crate::storage::cas::{self, BlobStore, Payload};
use crate::storage::fold::{self, Delta, Folded};
use crate::supervisor::TrajectoryOutcome;
use sylph_protocol::env::{NativeBlobChunk, MAX_NATIVE_BLOB_BYTES, NATIVE_BLOB_CHUNK_BYTES};
use sylph_protocol::NativeRecord;

/// The result of reading a native sidecar, keeping absence distinct from a recorded error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeBlobState {
    Available(Vec<u8>),
    Unreadable { size: u64, error: String },
    Missing,
}

// One measurement definition serves both the live query and the recorded baseline.
// Unavailable references describe source bytes, not retained body bytes.
const SESSION_BYTES_SQL: &str = "SELECT
    COALESCE((SELECT SUM(text_len + data_len) FROM blocks WHERE session_id = ?1), 0)
    + COALESCE((SELECT SUM(length(payload_json)) FROM native_records WHERE session_id = ?1), 0)
    + COALESCE((SELECT SUM(size) FROM native_blobs WHERE session_id = ?1 AND hash IS NOT NULL), 0)
    + COALESCE((SELECT SUM(length(data)) FROM native_blob_chunks WHERE session_id = ?1), 0) AS bytes,
    (SELECT COUNT(*) FROM blocks WHERE session_id = ?1) AS blocks";

/// One block as stored: a completed event plus how it streamed.
#[derive(Debug, Clone)]
pub struct Block {
    pub session_id: String,
    pub seq: i32,
    pub turn_id: String,
    pub kind: String,
    pub phase: Option<String>,
    pub stream_id: Option<String>,
    /// The delegated trajectory this block ran in; `None` is the root turn.
    pub span: Option<String>,
    /// Latest typed interior reachability fact carried by a trajectory phase.
    pub interior_state: Option<String>,
    pub name: Option<String>,
    pub text: String,
    pub folded: Option<Folded>,
    pub data: Option<serde_json::Value>,
    pub is_error: bool,
    pub source: String,
    pub arrival_at_ms: i64,
    pub recorded_at_ms: Option<i64>,
    pub started_at_ms: i64,
    pub created_at_ms: i64,
}

/// Quote a user's text as a single FTS5 phrase.
///
/// **A user's query is not an FTS5 query.** FTS5 reads punctuation as *syntax*, so an
/// ordinary search string is parsed as an expression: `needle-one` becomes `needle NOT
/// one`, which answers `no such column: one` — an error naming a column, three layers from
/// the text box that typed it. Hyphens are the common case, because identifiers and flags
/// are exactly what someone searches a transcript for.
///
/// **Binding the value as a parameter is not sufficient**, which is the trap here: binding
/// protects against SQL injection, and the bound value still reaches FTS5's expression
/// parser. Quoting is what makes the text literal.
///
/// Inside a double-quoted phrase FTS5 treats everything but `"` as ordinary text, and an
/// embedded `"` is escaped by doubling it.
///
/// The cost is worth stating: a multi-word query is a phrase match rather than an implicit
/// AND, so `foo bar` finds "foo bar" and not a block mentioning both apart. That is the
/// safe direction — it never errors and never silently inverts a term — and supporting
/// operators means *parsing* the user's text rather than escaping it.
fn fts5_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

/// One session that matched a full-text query, with its best block.
///
/// Carries the session and sequence rather than a rowid, because `blocks_fts` stores
/// them `UNINDEXED` for exactly this: a hit resolves to a block without a join back
/// through a rowid mapping that would have to be maintained separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The encoded identity triple.
    pub session_id: String,
    /// The `seq` of the best-ranked matching block, which is what a client scrolls to.
    pub seq: i32,
    /// The matched text, elided around the hit rather than returned whole.
    pub excerpt: String,
    /// The turn that block belongs to. `None` when the block row is gone — pruning can
    /// remove it while the index entry is being rebuilt — and absent rather than
    /// fabricated, because a wrong turn id would scroll a client to the wrong place.
    pub turn_id: Option<String>,
    /// How many blocks matched **within this session**, so one strong match and thirty
    /// weak ones are distinguishable.
    pub hits: i64,
}

/// How much record one session holds, and how that changed.
///
/// `grewBy` is a signed delta, and negative is a real and useful reading: it means
/// pruning worked. A field reporting only increases would make a successful
/// retention pass indistinguishable from a stalled one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBytes {
    pub session_id: String,
    pub bytes: i64,
    pub blocks: i64,
    pub measured_at_ms: i64,
    /// Change since the previous *differing* measurement, or `None` when there has
    /// only ever been one. `None` is not zero: a session measured once has an
    /// unknown trend, and reporting that as "stable" would be a claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grew_by: Option<i64>,
    /// How long that change took, so a delta can be read as a rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grew_over_ms: Option<i64>,
}

fn session_bytes_from_row(row: sqlx::sqlite::SqliteRow) -> SessionBytes {
    let bytes: i64 = row.get("bytes");
    let measured_at_ms: i64 = row.get("measured_at_ms");
    let previous_bytes: Option<i64> = row.get("previous_bytes");
    let previous_measured_at_ms: Option<i64> = row.get("previous_measured_at_ms");

    SessionBytes {
        session_id: row.get("session_id"),
        bytes,
        blocks: row.get("blocks"),
        measured_at_ms,
        grew_by: previous_bytes.map(|previous| bytes - previous),
        grew_over_ms: previous_measured_at_ms.map(|before| measured_at_ms - before),
    }
}

/// The read-mostly history store.
#[derive(Clone)]
pub struct HistoryStore {
    pool: SqlitePool,
    blobs: BlobStore,
    native_record_failures: Arc<AtomicU64>,
}

impl HistoryStore {
    /// Open (creating if needed) the history database beside a blob store.
    pub async fn open(db_path: &str, blobs: BlobStore) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // FULL, unlike the journal's NORMAL: this is the record, and it is
            // written once per turn rather than once per token, so the fsync is
            // affordable exactly where the journal's would not be.
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(std::time::Duration::from_secs(30));

        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            // **The `jieba` tokenizer is registered per connection, and a failure here is
            // fatal rather than a fallback.**
            //
            // Registration is bound to one `sqlite3*`, so every connection the pool opens
            // needs it — including the one migrations run on, which is why this is set
            // before `connect_with` rather than after.
            //
            // Linked into the binary rather than loaded as a `.dylib`: there is no file to
            // locate, no per-platform artifact to install, and no "extension missing"
            // runtime state to degrade into. Degrading is what makes this worth stating —
            // an index built with one tokenizer and queried with another silently returns
            // nothing for CJK, so falling back to `unicode61` would answer *wrongly*
            // rather than answering worse. `docs/storage.md` § Tokenization.
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    let mut handle = conn.lock_handle().await?;
                    // SAFETY: the handle is a live `sqlite3*` held for this call, and
                    // registration lasts as long as that connection.
                    let rc = unsafe { fts5jieba::register_on_connection(handle.as_raw_handle().as_ptr().cast()) };
                    rc.map_err(|code| {
                        sqlx::Error::Configuration(
                            format!(
                                "could not register the `jieba` FTS5 tokenizer                                  (SQLite code {code}); the full-text index cannot be                                  read or written without it"
                            )
                            .into(),
                        )
                    })
                })
            })
            .connect_with(options)
            .await
            .with_context(|| format!("Could not open the history store at {}", db_path))?;

        sqlx::migrate!("./migrations/history")
            .run(&pool)
            .await
            .context("History migrations failed")?;

        Ok(Self {
            pool,
            blobs,
            native_record_failures: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    /// Number of native records that could not be durably retained since this store opened.
    ///
    /// Raw retention is a correctness contract. A warning in a log is not enough because
    /// the process may continue to serve a timeline whose re-projection source is gone.
    pub fn native_record_failures(&self) -> u64 {
        self.native_record_failures.load(Ordering::Relaxed)
    }

    /// Retain a bounded sidecar chunk before its producer receipt is acknowledged.
    pub async fn put_native_blob_chunk(
        &self,
        session_id: &str,
        chunk: &NativeBlobChunk,
    ) -> Result<()> {
        anyhow::ensure!(
            chunk.size <= MAX_NATIVE_BLOB_BYTES,
            "native blob exceeds size limit"
        );
        anyhow::ensure!(
            chunk.data.len() <= NATIVE_BLOB_CHUNK_BYTES.div_ceil(3) * 4,
            "encoded native blob chunk exceeds size limit"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&chunk.data)
            .context("native blob chunk is not valid base64")?;
        anyhow::ensure!(
            bytes.len() <= NATIVE_BLOB_CHUNK_BYTES,
            "native blob chunk exceeds size limit"
        );
        anyhow::ensure!(
            chunk.offset <= chunk.size
                && bytes.len() as u64 <= chunk.size - chunk.offset
                && (!bytes.is_empty() || chunk.size == 0)
                && chunk.final_chunk == (chunk.offset + bytes.len() as u64 == chunk.size),
            "native blob chunk has invalid bounds"
        );
        let mut tx = self.pool.begin().await?;
        // An insert acquires SQLite's writer lock before checking the current end.
        // A read-then-insert transaction can lose that upgrade to another writer.
        sqlx::query(
            "INSERT INTO native_blob_chunks (session_id, source, version, name, size, offset, data)
             SELECT ?, ?, ?, ?, ?, ?, ?
             WHERE ? = COALESCE((SELECT SUM(length(data)) FROM native_blob_chunks
               WHERE session_id = ? AND source = ? AND version = ? AND name = ?), 0)
             AND NOT EXISTS (SELECT 1 FROM native_blob_chunks
               WHERE session_id = ? AND source = ? AND version = ? AND name = ? AND size <> ?)
             ON CONFLICT(session_id, source, version, name, offset) DO NOTHING",
        )
        .bind(session_id)
        .bind(&chunk.source)
        .bind(chunk.version as i64)
        .bind(&chunk.name)
        .bind(chunk.size as i64)
        .bind(chunk.offset as i64)
        .bind(&bytes)
        .bind(chunk.offset as i64)
        .bind(session_id)
        .bind(&chunk.source)
        .bind(chunk.version as i64)
        .bind(&chunk.name)
        .bind(session_id)
        .bind(&chunk.source)
        .bind(chunk.version as i64)
        .bind(&chunk.name)
        .bind(chunk.size as i64)
        .execute(&mut *tx)
        .await?;
        let held: Option<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT size, data FROM native_blob_chunks
             WHERE session_id = ? AND source = ? AND version = ? AND name = ? AND offset = ?",
        )
        .bind(session_id)
        .bind(&chunk.source)
        .bind(chunk.version as i64)
        .bind(&chunk.name)
        .bind(chunk.offset as i64)
        .fetch_optional(&mut *tx)
        .await?;
        anyhow::ensure!(
            held == Some((chunk.size as i64, bytes)),
            "native blob chunk is out of order or conflicts with retained bytes"
        );
        tx.commit().await?;
        Ok(())
    }

    /// Store one complete vendor-native record and its sidecar references.
    ///
    /// Raw records have their own retention class and are deliberately outside the event
    /// watermark path.  Re-projecting an event may need a field the current adapter ignores,
    /// so replacing this row would be data loss; the `(session, source, version)` key is
    /// append-only for JSONL and content-versioned for rewritten manifests. Sidecar chunks
    /// are assembled into CAS before the record references them.
    pub async fn put_native_record(&self, session_id: &str, record: &NativeRecord) -> Result<()> {
        // A file may be observed while SQLite is checkpointing or while the vendor's
        // sidecar is being rotated. One bounded retry handles those transient failures
        // without turning a bad record into an unbounded ingest loop.
        let mut result = self.put_native_record_inner(session_id, record).await;
        if result.is_err() {
            result = self.put_native_record_inner(session_id, record).await;
        }
        if result.is_err() {
            self.native_record_failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn put_native_record_inner(&self, session_id: &str, record: &NativeRecord) -> Result<()> {
        let mut blobs = Vec::with_capacity(record.blobs.len());
        for blob in &record.blobs {
            anyhow::ensure!(
                blob.locator.is_none(),
                "native sidecar locators must be resolved by the owning envagent"
            );
            i64::try_from(blob.size).context("native blob size exceeds storage range")?;
            let hash = if blob.error.is_some() {
                anyhow::ensure!(
                    blob.bytes.is_none(),
                    "unreadable native blob contains bytes"
                );
                None
            } else {
                anyhow::ensure!(
                    blob.size <= MAX_NATIVE_BLOB_BYTES,
                    "native blob exceeds size limit"
                );
                let bytes = match &blob.bytes {
                    Some(bytes) => bytes.clone(),
                    None => {
                        let chunks: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
                            "SELECT size, offset, data FROM native_blob_chunks
                             WHERE session_id = ? AND source = ? AND version = ? AND name = ?
                             ORDER BY offset",
                        )
                        .bind(session_id)
                        .bind(&record.source)
                        .bind(record.version as i64)
                        .bind(&blob.name)
                        .fetch_all(&self.pool)
                        .await?;
                        if chunks.is_empty() {
                            // A repeated record may have already consumed its chunks.
                            let hash: Option<String> = sqlx::query_scalar(
                                "SELECT hash FROM native_blobs
                                 WHERE session_id = ? AND source = ? AND version = ? AND name = ? AND size = ?",
                            )
                            .bind(session_id).bind(&record.source).bind(record.version as i64)
                            .bind(&blob.name).bind(blob.size as i64).fetch_optional(&self.pool).await?.flatten();
                            let hash = hash.context("native blob has no retained chunks")?;
                            self.blobs
                                .get(&hash)
                                .await?
                                .context("native blob CAS object is missing")?
                        } else {
                            let mut bytes = Vec::with_capacity(blob.size as usize);
                            for (size, offset, data) in chunks {
                                anyhow::ensure!(
                                    size == blob.size as i64 && offset == bytes.len() as i64,
                                    "native blob chunk sequence is incomplete"
                                );
                                bytes.extend_from_slice(&data);
                            }
                            bytes
                        }
                    }
                };
                anyhow::ensure!(
                    bytes.len() as u64 == blob.size,
                    "native blob size does not match its body"
                );
                Some(self.blobs.put(&bytes).await?)
            };
            blobs.push((blob, hash));
        }

        let mut tx = self.pool.begin().await?;
        let payload = record
            .raw_json
            .clone()
            .unwrap_or_else(|| record.payload.to_string().into_bytes());
        sqlx::query(
            r#"INSERT INTO native_records
               (session_id, source, version, payload_json, created_at_ms, retention_class)
               VALUES (?, ?, ?, ?, ?, 'raw')
               ON CONFLICT(session_id, source, version) DO NOTHING"#,
        )
        .bind(session_id)
        .bind(&record.source)
        .bind(record.version as i64)
        .bind(&payload)
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;

        // A revision collision must be loud. `INSERT OR IGNORE` alone made a changed
        // manifest or a restarted reader look successful while silently retaining the old
        // payload.
        let stored: Vec<u8> = sqlx::query_scalar(
            "SELECT payload_json FROM native_records
             WHERE session_id = ? AND source = ? AND version = ?",
        )
        .bind(session_id)
        .bind(&record.source)
        .bind(record.version as i64)
        .fetch_one(&mut *tx)
        .await?;
        if stored != payload {
            return Err(anyhow::anyhow!(
                "native record revision collision for {}:{}:{}",
                session_id,
                record.source,
                record.version
            ));
        }

        for (blob, hash) in blobs {
            sqlx::query(
                r#"INSERT INTO native_blobs
                   (session_id, source, version, name, hash, size, error)
                   VALUES (?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(session_id, source, version, name) DO NOTHING"#,
            )
            .bind(session_id)
            .bind(&record.source)
            .bind(record.version as i64)
            .bind(&blob.name)
            .bind(&hash)
            .bind(blob.size as i64)
            .bind(&blob.error)
            .execute(&mut *tx)
            .await?;
            let held: (Option<String>, i64, Option<String>) = sqlx::query_as(
                "SELECT hash, size, error FROM native_blobs
                 WHERE session_id = ? AND source = ? AND version = ? AND name = ?",
            )
            .bind(session_id)
            .bind(&record.source)
            .bind(record.version as i64)
            .bind(&blob.name)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                held == (hash, blob.size as i64, blob.error.clone()),
                "native blob revision collision for {}:{}:{}",
                record.source,
                record.version,
                blob.name
            );
            sqlx::query("DELETE FROM native_blob_chunks WHERE session_id = ? AND source = ? AND version = ? AND name = ?")
                .bind(session_id).bind(&record.source).bind(record.version as i64).bind(&blob.name)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Read the original JSON payload of one native record.
    pub async fn native_record(
        &self,
        session_id: &str,
        source: &str,
        version: u64,
    ) -> Result<Option<serde_json::Value>> {
        let row = sqlx::query(
            "SELECT payload_json FROM native_records WHERE session_id = ? AND source = ? AND version = ?",
        )
        .bind(session_id)
        .bind(source)
        .bind(version as i64)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let bytes: Vec<u8> = row.try_get("payload_json")?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Read the lexical bytes retained for one native record.
    ///
    /// This is intentionally separate from [`Self::native_record`], whose parsed value is
    /// convenient for re-projection but cannot preserve whitespace, key order, duplicate
    /// keys, or escape spelling.
    pub async fn native_record_bytes(
        &self,
        session_id: &str,
        source: &str,
        version: u64,
    ) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query(
            "SELECT payload_json FROM native_records WHERE session_id = ? AND source = ? AND version = ?",
        )
        .bind(session_id)
        .bind(source)
        .bind(version as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row.try_get("payload_json"))
            .transpose()
            .map_err(Into::into)
    }

    /// Read a native sidecar body on demand.  The timeline never inlines these bytes.
    pub async fn native_blob(
        &self,
        session_id: &str,
        source: &str,
        version: u64,
        name: &str,
    ) -> Result<NativeBlobState> {
        let row = sqlx::query(
            "SELECT hash, size, error FROM native_blobs
             WHERE session_id = ? AND source = ? AND version = ? AND name = ?",
        )
        .bind(session_id)
        .bind(source)
        .bind(version as i64)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(NativeBlobState::Missing);
        };
        let hash: Option<String> = row.try_get("hash")?;
        let size: u64 = row.try_get::<i64, _>("size")?.max(0) as u64;
        let error: Option<String> = row.try_get("error")?;
        if let Some(error) = error {
            return Ok(NativeBlobState::Unreadable { size, error });
        }
        match hash {
            Some(hash) => match self.blobs.get(&hash).await? {
                Some(bytes) => Ok(NativeBlobState::Available(bytes)),
                None => Ok(NativeBlobState::Missing),
            },
            None => Ok(NativeBlobState::Missing),
        }
    }

    /// Write blocks, then their blob references.
    ///
    /// `INSERT OR REPLACE` because consolidation must be safe to re-run: a crash
    /// after the insert and before the journal delete leaves rows here that the
    /// next attempt writes again.
    pub async fn put_blocks(&self, blocks: &[Block]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }

        // Every blob lands before the transaction that references it, so an
        // interrupted consolidation can only ever leave unreferenced blobs.
        let mut payloads = Vec::with_capacity(blocks.len());
        for block in blocks {
            let text = cas::store_text(&self.blobs, &block.text).await?;
            let (data, data_len) = match &block.data {
                Some(value) if !value.is_null() => {
                    let rendered = value.to_string();
                    (
                        Some(cas::store_text(&self.blobs, &rendered).await?),
                        rendered.len(),
                    )
                }
                _ => (None, 0),
            };
            payloads.push((text, data, data_len));
        }

        let mut tx = self.pool.begin().await?;

        for (block, (text, data, data_len)) in blocks.iter().zip(&payloads) {
            // A late agent outcome may have reconciled this block while a
            // previously scheduled consolidation was still writing. Preserve
            // the reconciled record instead of allowing that stale snapshot to
            // resurrect the server's provisional `abandoned` close.
            let already_reconciled: Option<bool> = sqlx::query_scalar(
                "SELECT reconciled FROM blocks WHERE session_id = ? AND seq = ?",
            )
            .bind(&block.session_id)
            .bind(block.seq)
            .fetch_optional(&mut *tx)
            .await?;
            if already_reconciled == Some(true) {
                continue;
            }
            let (text_inline, text_hash) = match text {
                Payload::Raw(bytes) | Payload::Zstd(bytes) => (Some(bytes.clone()), None),
                Payload::External { hash } => (None, Some(hash.clone())),
            };
            let (data_inline, data_hash) = match data {
                Some(Payload::Raw(bytes)) | Some(Payload::Zstd(bytes)) => {
                    (Some(bytes.clone()), None)
                }
                Some(Payload::External { hash }) => (None, Some(hash.clone())),
                None => (None, None),
            };

            sqlx::query(
                r#"
                INSERT OR REPLACE INTO blocks (
                    session_id, seq, turn_id, kind, phase, stream_id, span, interior_state, name,
                    text_inline, text_hash, text_len, text_encoding,
                    delta_offsets, delta_timings,
                    data_json, data_hash, data_len, data_encoding, reconciled,
                    is_error, source, arrival_at_ms, recorded_at_ms,
                    started_at_ms, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&block.session_id)
            .bind(block.seq)
            .bind(&block.turn_id)
            .bind(&block.kind)
            .bind(&block.phase)
            .bind(&block.stream_id)
            .bind(&block.span)
            .bind(&block.interior_state)
            .bind(&block.name)
            .bind(text_inline)
            .bind(&text_hash)
            .bind(block.text.len() as i64)
            .bind(text.encoding())
            .bind(block.folded.as_ref().map(|f| fold::pack(&f.offsets)))
            .bind(block.folded.as_ref().map(|f| fold::pack(&f.timings)))
            .bind(data_inline)
            .bind(&data_hash)
            .bind(*data_len as i64)
            .bind(data.as_ref().map_or("raw", Payload::encoding))
            .bind(block.data.as_ref().and_then(|value| value.get("reconciled")).and_then(serde_json::Value::as_bool).unwrap_or(false))
            .bind(block.is_error)
            .bind(&block.source)
            .bind(block.arrival_at_ms)
            .bind(block.recorded_at_ms)
            .bind(block.started_at_ms)
            .bind(block.created_at_ms)
            .execute(&mut *tx)
            .await?;

            // Recorded per (hash, block) rather than as a count, so a blob shared
            // by a pruned block and a retained one survives the prune.
            for hash in [text_hash, data_hash].into_iter().flatten() {
                sqlx::query(
                    "INSERT OR IGNORE INTO cas_refs (hash, session_id, seq) VALUES (?, ?, ?)",
                )
                .bind(&hash)
                .bind(&block.session_id)
                .bind(block.seq)
                .execute(&mut *tx)
                .await?;
            }

            // The search index, fed the **decoded** text.
            //
            // This is why `blocks_fts` cannot be an external-content FTS5 table, which
            // is the shape the separate archive used and therefore the obvious thing to
            // copy. External content re-reads the column from the base table at query
            // time, and `text_inline` is not text: it may be zstd, and when the payload
            // is externalized to CAS the column is NULL and the bytes live under
            // `blobs/`. Such an index would hold compressed bytes for some rows and
            // nothing at all for the rows most worth finding — the large ones. So the
            // decoded copy is inserted here, which costs a second copy of the text and
            // is the price of the tier split (`docs/storage.md`).
            //
            // Deleted first, because `INSERT OR REPLACE` above makes re-consolidating a
            // turn idempotent and this table has no primary key to replace on — a
            // second pass would otherwise index the same block twice and return it
            // twice from one query.
            //
            // Empty text is skipped rather than indexed: a `tool_call` carries its
            // arguments in `data_json` and no text at all, and an empty row can never
            // match a query while still costing an index entry per block.
            sqlx::query("DELETE FROM blocks_fts WHERE session_id = ? AND seq = ?")
                .bind(&block.session_id)
                .bind(block.seq)
                .execute(&mut *tx)
                .await?;
            if !block.text.trim().is_empty() {
                sqlx::query("INSERT INTO blocks_fts (text, session_id, seq) VALUES (?, ?, ?)")
                    .bind(&block.text)
                    .bind(&block.session_id)
                    .bind(block.seq)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    /// A session's blocks in stream order, resolving external payloads.
    pub async fn blocks(&self, session_id: &str, after_seq: i32) -> Result<Vec<Block>> {
        let rows =
            sqlx::query("SELECT * FROM blocks WHERE session_id = ? AND seq > ? ORDER BY seq")
                .bind(session_id)
                .bind(after_seq)
                .fetch_all(&self.pool)
                .await?;

        let mut blocks = Vec::with_capacity(rows.len());
        for row in rows {
            blocks.push(self.row_to_block(row).await?);
        }
        Ok(blocks)
    }

    /// Return the next sequence number occupied by this session in history.
    ///
    /// The journal and history tiers share one session-wide sequence space, but
    /// consolidation removes journal rows. Remote ingest therefore needs this
    /// cursor as well as the journal's max, or a late event can reuse a sequence
    /// that a consolidated block still owns.
    pub async fn next_seq(&self, session_id: &str) -> Result<i32> {
        let max: Option<i32> =
            sqlx::query_scalar("SELECT MAX(seq) FROM blocks WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(max.unwrap_or(-1).saturating_add(1))
    }

    /// One turn's blocks, for a timeline that renders a single exchange.
    pub async fn turn_blocks(&self, turn_id: &str) -> Result<Vec<Block>> {
        let rows = sqlx::query("SELECT * FROM blocks WHERE turn_id = ? ORDER BY seq")
            .bind(turn_id)
            .fetch_all(&self.pool)
            .await?;

        let mut blocks = Vec::with_capacity(rows.len());
        for row in rows {
            blocks.push(self.row_to_block(row).await?);
        }
        Ok(blocks)
    }

    async fn row_to_block(&self, row: sqlx::sqlite::SqliteRow) -> Result<Block> {
        let encoding: String = row.try_get("text_encoding")?;
        let inline: Option<Vec<u8>> = row.try_get("text_inline")?;
        let hash: Option<String> = row.try_get("text_hash")?;

        let payload = match (encoding.as_str(), inline, hash) {
            ("external", None, Some(hash)) => Payload::External { hash },
            ("zstd", Some(bytes), None) => Payload::Zstd(bytes),
            ("raw", Some(bytes), None) => Payload::Raw(bytes),
            _ => anyhow::bail!("stored block text has an invalid encoding or body"),
        };
        let text = cas::load_text(&self.blobs, &payload).await?;
        anyhow::ensure!(
            text.len() as i64 == row.try_get::<i64, _>("text_len")?,
            "stored block text length mismatch"
        );

        let data_encoding: String = row.try_get("data_encoding")?;
        let data_inline: Option<Vec<u8>> = row.try_get("data_json")?;
        let data_hash: Option<String> = row.try_get("data_hash")?;
        let data_len: i64 = row.try_get("data_len")?;
        let data_payload = match (data_encoding.as_str(), data_inline, data_hash) {
            ("external", None, Some(hash)) => Some(Payload::External { hash }),
            ("zstd", Some(bytes), None) => Some(Payload::Zstd(bytes)),
            ("raw", Some(bytes), None) => Some(Payload::Raw(bytes)),
            ("raw", None, None) if data_len == 0 => None,
            _ => anyhow::bail!("stored block metadata has an invalid encoding or body"),
        };
        let data = match data_payload {
            Some(payload) => {
                let rendered = cas::load_text(&self.blobs, &payload).await?;
                anyhow::ensure!(
                    rendered.len() as i64 == data_len,
                    "stored block metadata length mismatch"
                );
                Some(
                    serde_json::from_str(&rendered)
                        .context("stored block metadata is not valid JSON")?,
                )
            }
            None => None,
        };

        let offsets: Option<Vec<u8>> = row.try_get("delta_offsets")?;
        let timings: Option<Vec<u8>> = row.try_get("delta_timings")?;
        let folded = match (offsets, timings) {
            (Some(offsets), Some(timings)) => Some(Folded {
                offsets: fold::unpack(&offsets),
                timings: fold::unpack(&timings),
            }),
            _ => None,
        };

        Ok(Block {
            session_id: row.try_get("session_id")?,
            seq: row.try_get("seq")?,
            turn_id: row.try_get("turn_id")?,
            kind: row.try_get("kind")?,
            phase: row.try_get("phase")?,
            stream_id: row.try_get("stream_id")?,
            span: row.try_get("span")?,
            interior_state: row.try_get("interior_state")?,
            name: row.try_get("name")?,
            text,
            folded,
            data,
            is_error: row.try_get::<i64, _>("is_error")? != 0,
            source: row.try_get("source")?,
            arrival_at_ms: row.try_get("arrival_at_ms")?,
            recorded_at_ms: row.try_get("recorded_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            created_at_ms: row.try_get("created_at_ms")?,
        })
    }

    /// Expand a block back into the events a client originally saw.
    ///
    /// Server-side on purpose (`docs/storage.md`): reconstruction is subtle enough
    /// — UTF-8 boundaries, timing accumulation — that a rule implemented once per
    /// frontend would differ per frontend, and Discord, a terminal, and the
    /// browser would disagree about what happened.
    pub fn replay_block(block: &Block) -> Vec<Delta> {
        match &block.folded {
            Some(folded) => fold::unfold(&block.text, folded, block.started_at_ms),
            // Never streamed: the block is its own single arrival.
            None => vec![Delta {
                text: block.text.clone(),
                at_ms: block.created_at_ms,
            }],
        }
    }

    /// Drop every block at or below a confirmed sequence number.
    ///
    /// Called only with a watermark from a [`crate::storage::retention`]
    /// authority. Returns how many blocks went.
    ///
    /// Blocks only — the `cas_refs` rows are deliberately left behind. They are
    /// what [`Self::sweep_unreferenced_blobs`] uses to find a blob no live block
    /// points at any more; deleting them here would erase the evidence and leak
    /// every pruned blob permanently. The ref outliving its block is a dangling
    /// row the sweep reconciles, which is the safe direction.
    pub async fn prune_below(&self, session_id: &str, confirmed_seq: i32) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("DELETE FROM blocks WHERE session_id = ? AND seq <= ?")
            .bind(session_id)
            .bind(confirmed_seq)
            .execute(&mut *tx)
            .await?;
        // The index goes with the rows, in the same transaction. `blocks_fts` is
        // standalone rather than external-content, so nothing removes its entries
        // automatically — and a hit pointing at a pruned block is worse than no hit,
        // because a client would follow it and find nothing.
        sqlx::query("DELETE FROM blocks_fts WHERE session_id = ? AND seq <= ?")
            .bind(session_id)
            .bind(confirmed_seq)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    /// Full-text search across every session's consolidated history, grouped by session.
    ///
    /// # Grouped rather than flat, and the grouping happens in SQL
    ///
    /// `docs/api/sessions.md` § Search defines a result as one **session** carrying its
    /// best match and a hit count, not one row per matching block. That shape is what
    /// lets a client render the same row component it uses for a listing, and it makes
    /// "one strong match" distinguishable from "thirty weak ones" — which a flat list
    /// cannot express, because thirty rows from one session would crowd out every other
    /// session's single good hit.
    ///
    /// `limit` therefore bounds **sessions**, not blocks. Grouping client-side after a
    /// row limit would silently do the opposite: a session with thirty hits would consume
    /// the whole page.
    ///
    /// `snippet()` rather than the stored text, so a two-word query against a 200 KB tool
    /// result returns a line instead of the file. FTS5 computes it from the indexed copy,
    /// which is the decoded text `put_blocks` inserted.
    ///
    /// Ordered by `MIN(rank)` — FTS5's own bm25 relevance for each session's best block.
    /// Ordering by recency was rejected: a search for a rare term should surface it
    /// wherever it is, and a client that wants recency can sort what it receives.
    ///
    /// **This is the one read that must stay off any polled path.** Measured, a full-text
    /// query is orders of magnitude slower than every other operation here, and owning the
    /// store makes FTS5 work no cheaper.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_page(query, limit, 0).await
    }

    /// Search one ranked page. `offset` is exposed so the API can skip hits from
    /// environments the caller cannot observe while still filling the requested
    /// page; applying authorization after a fixed FTS `LIMIT` silently loses rows.
    pub async fn search_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SearchHit>> {
        // **A user's query is not an FTS5 query**, so it is quoted into a phrase by
        // [`fts5_phrase`] rather than passed through. Binding it as a parameter is not
        // enough on its own: the value still reaches FTS5's *expression* parser, which
        // reads punctuation as syntax (`docs/storage.md` § A user's query is not an
        // FTS5 query).
        //
        // **`snippet()` cannot appear inside the subquery**, which is what forces this
        // into two statements rather than one. FTS5's auxiliary functions are only
        // callable in a query that reads the virtual table directly — used in a subquery
        // whose results are then joined, SQLite answers `unable to use function snippet
        // in the requested context`. So the ranking pass selects only plain columns, and
        // the excerpt is fetched per surviving row.
        let ranked = sqlx::query_as::<_, (String, i32, i64)>(
            "SELECT ranked.session_id, ranked.seq, ranked.hits
               FROM (
                 SELECT session_id,
                        seq,
                        rank AS score,
                        COUNT(*) OVER (PARTITION BY session_id) AS hits,
                        ROW_NUMBER() OVER (
                          PARTITION BY session_id ORDER BY rank
                        ) AS position
                   FROM blocks_fts
                  WHERE blocks_fts MATCH ?
               ) AS ranked
              WHERE ranked.position = 1
              ORDER BY ranked.score
              LIMIT ? OFFSET ?",
        )
        .bind(fts5_phrase(query))
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(ranked.len());
        for (session_id, seq, hits) in ranked {
            // One statement per result rather than per match, so this is bounded by
            // `limit` — the whole reason the ranking pass groups first.
            let row = sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT snippet(blocks_fts, 0, '', '', '…', 24), blocks.turn_id
                   FROM blocks_fts
                   LEFT JOIN blocks
                     ON blocks.session_id = blocks_fts.session_id
                    AND blocks.seq = blocks_fts.seq
                  WHERE blocks_fts MATCH ?
                    AND blocks_fts.session_id = ?
                    AND blocks_fts.seq = ?
                  LIMIT 1",
            )
            .bind(fts5_phrase(query))
            .bind(&session_id)
            .bind(seq)
            .fetch_optional(&self.pool)
            .await?;

            let (excerpt, turn_id) = row.unwrap_or_default();
            out.push(SearchHit {
                session_id,
                seq,
                excerpt,
                turn_id,
                hits,
            });
        }

        Ok(out)
    }

    /// Delete blobs nothing references any more, returning how many.
    ///
    /// Separate from pruning because reachability is not a property of one session:
    /// the same file read in two sessions is one blob, and it survives until both
    /// have let go.
    pub async fn sweep_unreferenced_blobs(&self) -> Result<u64> {
        let candidates: Vec<String> = sqlx::query_scalar(
            "SELECT hash FROM cas_refs
             UNION
             SELECT hash FROM native_blobs WHERE hash IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut removed = 0;
        for hash in candidates {
            let live_blocks: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM cas_refs
                 WHERE hash = ?
                   AND EXISTS (
                     SELECT 1 FROM blocks
                      WHERE blocks.session_id = cas_refs.session_id
                        AND blocks.seq = cas_refs.seq
                   )",
            )
            .bind(&hash)
            .fetch_one(&self.pool)
            .await?;
            let live_native: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM native_blobs WHERE hash = ?")
                    .bind(&hash)
                    .fetch_one(&self.pool)
                    .await?;

            sqlx::query(
                "DELETE FROM cas_refs WHERE hash = ? AND NOT EXISTS (
                SELECT 1 FROM blocks
                 WHERE blocks.session_id = cas_refs.session_id
                   AND blocks.seq = cas_refs.seq
              )",
            )
            .bind(&hash)
            .execute(&self.pool)
            .await?;

            if live_blocks == 0 && live_native == 0 {
                self.blobs.remove(&hash).await?;
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Record how far an authority has confirmed a session, and against what.
    ///
    /// Written on every pass, including one that confirmed nothing: the row is what
    /// distinguishes "checked, and nothing is safe to drop yet" from "never
    /// checked", and those need opposite responses. An empty `watermarks` table
    /// looks exactly like a correctly conservative one, which is how a retention
    /// path that confirmed nothing stayed invisible.
    pub async fn set_watermark(
        &self,
        session_id: &str,
        authority: &str,
        confirmation: &crate::storage::retention::Confirmation,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watermarks (
               session_id, authority, confirmed_seq, checked_at_ms, source, withheld
             )
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
               authority = excluded.authority,
               confirmed_seq = excluded.confirmed_seq,
               checked_at_ms = excluded.checked_at_ms,
               source = excluded.source,
               withheld = excluded.withheld",
        )
        .bind(session_id)
        .bind(authority)
        .bind(confirmation.safe_to_drop)
        .bind(now_ms())
        .bind(
            confirmation
                .source
                .map(crate::storage::retention::NativeSourceState::as_str),
        )
        .bind(
            confirmation
                .withheld
                .map(crate::storage::retention::WithheldReason::as_str),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Why a session's last pass withheld, if it did.
    ///
    /// This replaces `watermark_fingerprint`, and the two answer different questions
    /// rather than one being a rename of the other. The fingerprint was the *separate
    /// archive's* `file-v2:<sha256>:…` over a native file, and a pass counted how many
    /// were unchanged to detect a stalled ingestion daemon — a signal about a component
    /// that no longer exists. What is worth reporting now is the reason a bound was not
    /// established, because that is what separates "the environment answered
    /// conservatively" from "nobody was asked".
    pub async fn watermark_withheld(&self, session_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT withheld FROM watermarks WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map(Option::flatten)
        .map_err(Into::into)
    }

    pub async fn watermark(&self, session_id: &str) -> Result<Option<(String, Option<i32>)>> {
        let row =
            sqlx::query("SELECT authority, confirmed_seq FROM watermarks WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|row| {
            (
                row.get::<String, _>("authority"),
                row.get::<Option<i32>, _>("confirmed_seq"),
            )
        }))
    }

    /// The highest sequence number whose block completed at or before `at_ms`.
    ///
    /// This is how a downstream authority's timestamp becomes a sequence number. An
    /// environment reports how far its native store extends as a time, and retention needs
    /// the position in this session's stream that corresponds to it
    /// (`docs/storage.md` § Deriving a bound from a vendor's store).
    pub async fn max_seq_at_or_before(&self, session_id: &str, at_ms: i64) -> Result<Option<i32>> {
        sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(seq) FROM blocks
             WHERE session_id = ? AND recorded_at_ms IS NOT NULL AND recorded_at_ms <= ?",
        )
        .bind(session_id)
        .bind(at_ms)
        .fetch_one(&self.pool)
        .await
        .context("Could not resolve a watermark to a sequence number")
    }

    /// Record a trajectory's real outcome on the consolidated turn that owns it.
    ///
    /// The history-tier half of [`crate::db::JournalStore::reconcile_trajectory`]; see
    /// there for why the amendment exists and what it may claim. Both are attempted
    /// unconditionally by the caller, because consolidation can move the row between
    /// the two tiers at any moment and neither can be checked first without a race.
    ///
    /// Read-modify-write rather than an `UPDATE`, so the amendment goes through
    /// `put_blocks` and inherits its text encoding and CAS accounting. A block's text
    /// may be inline, compressed, or externalized, and an `UPDATE` writing a bare
    /// string would leave `text_encoding` and `text_len` describing the previous
    /// payload — a row that reads back as garbage for the one delegation that most
    /// needs explaining.
    pub async fn reconcile_trajectory(
        &self,
        session_id: &str,
        stream_id: &str,
        except_turn_id: &str,
        outcome: &TrajectoryOutcome<'_>,
    ) -> Result<bool> {
        let Some(row) = sqlx::query(
            "SELECT * FROM blocks
              WHERE session_id = ? AND stream_id = ? AND turn_id <> ?
                AND kind = 'subagent'
              ORDER BY seq DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(stream_id)
        .bind(except_turn_id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false);
        };

        let mut block = self.row_to_block(row).await?;
        if !outcome.supersedes(block.phase.as_deref(), block.data.as_ref()) {
            return Ok(false);
        }

        outcome.apply(
            &mut block.phase,
            &mut block.text,
            &mut block.is_error,
            &mut block.data,
            &mut block.source,
        );
        self.put_blocks(std::slice::from_ref(&block)).await?;
        Ok(true)
    }

    /// Close trajectories this tier still holds as running, returning how many.
    ///
    /// The counterpart to [`crate::db::JournalStore::close_stale_trajectories`], one
    /// tier down, and it exists because consolidation now *preserves* a trajectory the
    /// turn-settle sweep deliberately left open. That is right while the process lives:
    /// backgrounded work is running, and `started` says so. It is wrong across a
    /// restart — the adapter is gone, so the outcome can never arrive — and a block
    /// left `started` is then a card that spins forever, which is the exact failure the
    /// sweep exists to prevent, displaced from the journal into history.
    ///
    /// Run at startup only, which is what makes the reasoning sound: no link survives a
    /// restart, so every trajectory found here belongs to a process that cannot report.
    /// During normal operation the same block is a true statement and must be left
    /// alone.
    ///
    /// `abandoned` and not `failed`, for the same reason everywhere else: nobody said
    /// this failed, only that nobody said. And still not final — if a resumed session
    /// somehow reports the outcome later, `reconcile_trajectory` amends this row,
    /// because a server-authored close is exactly what it is entitled to overwrite.
    pub async fn close_running_trajectories(&self) -> Result<u64> {
        let rows = sqlx::query(
            "SELECT * FROM blocks
              WHERE kind = 'subagent' AND phase = 'started'",
        )
        .fetch_all(&self.pool)
        .await
        .context("Could not list trajectories left running in history")?;

        let mut closed = Vec::with_capacity(rows.len());
        for row in rows {
            let mut block = self.row_to_block(row).await?;
            block.phase = Some("completed".to_string());
            block.is_error = true;
            block.source = "server".to_string();
            block.data = Some(serde_json::json!({
                "closedBy": "server",
                "reason": "backend_restarted",
            }));
            closed.push(block);
        }

        let count = closed.len() as u64;
        if count > 0 {
            self.put_blocks(&closed).await?;
        }
        Ok(count)
    }

    pub async fn block_count(&self, session_id: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM blocks WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .context("Could not count blocks")
    }

    /// Bytes of record a session currently holds, and how many blocks hold them.
    ///
    /// `text_len` is the *uncompressed* length, always — including for an
    /// externalized payload, which is exactly why the column exists (a timeline
    /// lays out without touching the blob store). So this is the logical size of
    /// the record, not its footprint on disk, and that is the deliberate choice:
    ///
    /// * **It is comparable across rows.** Compression ratio varies 1–5× with
    ///   content, so ranking sessions by stored bytes ranks them partly by how
    ///   compressible their tool output happened to be. A session holding twice
    ///   the record should report twice the bytes.
    /// * **It needs no second bookkeeping.** Stored size for an external payload
    ///   is a file on disk, so measuring it means either a `stat` per blob per
    ///   pass or a size column written at consolidation — a new fact to keep
    ///   correct through pruning and sweeping, for a number that answers the same
    ///   question worse.
    /// * **Attribution stays sound.** A blob shared by two sessions is one file,
    ///   so *stored* bytes summed per session over-counts it, and each session's
    ///   share is not well-defined. Logical bytes are honestly per session: this
    ///   session's record does contain that text.
    ///
    /// Metadata uses its own uncompressed length. Native lexical records, retained
    /// sidecars, and staged chunks contribute their body bytes. An unreadable reference's
    /// source size contributes nothing because the server does not hold that body.
    ///
    /// The consequence to hold onto: the sum over sessions is the size of the
    /// *record*, and the store on disk is smaller. This measure answers "which
    /// session is growing", not "what is my disk doing" — `du` answers that, and
    /// answers it exactly.
    pub async fn measure_session_bytes(&self, session_id: &str) -> Result<(i64, i64)> {
        let row = sqlx::query(SESSION_BYTES_SQL)
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .context("Could not measure a session's held bytes")?;

        Ok((row.get::<i64, _>("bytes"), row.get::<i64, _>("blocks")))
    }

    /// Measure a session and record it, carrying the prior reading forward.
    ///
    /// The previous value moves only when this measurement is a *different*
    /// observation, so two passes in quick succession cannot flatten the baseline
    /// and hide growth: re-recording the same reading would set
    /// `previous_bytes = bytes` and report a growing session as stable. Held at
    /// the last differing observation instead, which is what makes the delta
    /// meaningful however often passes run.
    pub async fn record_session_bytes(&self, session_id: &str) -> Result<SessionBytes> {
        self.record_session_bytes_with_pass_delta(session_id)
            .await
            .map(|(measurement, _)| measurement)
    }

    /// Measure and record a session, returning the change since the immediately
    /// preceding observation as well as the long-term trend carried by the row.
    ///
    /// The two deltas answer different questions. `SessionBytes::grew_by` is held
    /// at the last *differing* observation so a stable pass does not erase a trend;
    /// a retention pass needs the change since its own previous reading, otherwise
    /// an unchanged third pass would report the original growth again.
    pub async fn record_session_bytes_with_pass_delta(
        &self,
        session_id: &str,
    ) -> Result<(SessionBytes, i64)> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(SESSION_BYTES_SQL)
            .bind(session_id)
            .fetch_one(&mut *tx)
            .await
            .context("Could not measure a session's held bytes")?;
        let bytes = row.get::<i64, _>("bytes");
        let blocks = row.get::<i64, _>("blocks");
        let now = now_ms();
        let before: Option<i64> =
            sqlx::query_scalar("SELECT bytes FROM session_bytes WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await
                .context("Could not read the previous session byte measurement")?;

        sqlx::query(
            "INSERT INTO session_bytes (
               session_id, bytes, blocks, measured_at_ms,
               previous_bytes, previous_measured_at_ms
             )
             VALUES (?, ?, ?, ?, NULL, NULL)
             ON CONFLICT(session_id) DO UPDATE SET
               previous_bytes = CASE
                 WHEN excluded.bytes = session_bytes.bytes THEN session_bytes.previous_bytes
                 ELSE session_bytes.bytes END,
               previous_measured_at_ms = CASE
                 WHEN excluded.bytes = session_bytes.bytes
                   THEN session_bytes.previous_measured_at_ms
                 ELSE session_bytes.measured_at_ms END,
               bytes = excluded.bytes,
               blocks = excluded.blocks,
               measured_at_ms = excluded.measured_at_ms",
        )
        .bind(session_id)
        .bind(bytes)
        .bind(blocks)
        .bind(now)
        .execute(&mut *tx)
        .await
        .context("Could not record a session's held bytes")?;

        tx.commit().await?;

        let measurement = self
            .session_bytes(session_id)
            .await?
            .context("a row just written must be readable")?;
        let pass_delta = before.map_or(0, |previous| bytes - previous);
        Ok((measurement, pass_delta))
    }

    /// One session's byte accounting, if it has ever been measured.
    pub async fn session_bytes(&self, session_id: &str) -> Result<Option<SessionBytes>> {
        let row = sqlx::query(
            "SELECT session_id, bytes, blocks, measured_at_ms,
                    previous_bytes, previous_measured_at_ms
               FROM session_bytes WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .context("Could not read a session's byte accounting")?;

        Ok(row.map(session_bytes_from_row))
    }

    /// The sessions holding the most record, largest first.
    ///
    /// Bounded, because the point is to name the few sessions worth acting on. A
    /// full listing would put the whole session table in a diagnostics response
    /// to answer a question about its head.
    pub async fn largest_sessions(&self, limit: i64) -> Result<Vec<SessionBytes>> {
        let rows = sqlx::query(
            "SELECT session_id, bytes, blocks, measured_at_ms,
                    previous_bytes, previous_measured_at_ms
               FROM session_bytes ORDER BY bytes DESC, session_id LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Could not list the largest sessions")?;

        Ok(rows.into_iter().map(session_bytes_from_row).collect())
    }

    /// Bytes of record held across every measured session, and how many that is.
    ///
    /// Read from the recorded measurements rather than summed live over `blocks`,
    /// so the total and the per-session rows beside it are the same observation. A
    /// live `SUM` would disagree with them by whatever changed since the last pass,
    /// which reads as an accounting bug.
    pub async fn total_bytes(&self) -> Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(bytes), 0) AS bytes, COUNT(*) AS sessions FROM session_bytes",
        )
        .fetch_one(&self.pool)
        .await
        .context("Could not total the held bytes")?;

        Ok((row.get::<i64, _>("bytes"), row.get::<i64, _>("sessions")))
    }

    /// Sessions holding history or raw bytes, least-recently-checked first.
    ///
    /// Ordered by the last pass rather than by id, so a bounded batch rotates
    /// through every session instead of re-examining the same alphabetical prefix
    /// forever. A session never checked has no watermark row and sorts first, which
    /// is what makes a fresh install converge on the whole set rather than starving
    /// whatever happens to sort late.
    ///
    /// Raw-only sessions participate because retaining every raw byte is normal and
    /// their growth must still be reported. Empty sessions need no accounting pass.
    pub async fn sessions_awaiting_retention(&self, limit: i64) -> Result<Vec<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT b.session_id FROM (
               SELECT session_id FROM blocks
               UNION SELECT session_id FROM native_records
               UNION SELECT session_id FROM native_blob_chunks
             ) b
             LEFT JOIN session_bytes sb ON sb.session_id = b.session_id
             ORDER BY sb.measured_at_ms IS NOT NULL, sb.measured_at_ms, b.session_id
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Could not list sessions awaiting retention")
    }
}

/// Fold a turn's journal rows into blocks.
///
/// Groups by `stream_id`, pairs each `completed` event with the deltas that built
/// it, and folds them. An event with no `stream_id` — the user's message, a status —
/// is its own block with no deltas.
pub fn consolidate(events: &[Event]) -> Vec<Block> {
    // Deltas arrive before their completion, so they are collected first and
    // matched as each `completed` event is reached.
    let mut pending: HashMap<String, Vec<Delta>> = HashMap::new();
    let mut first_arrival: HashMap<String, i64> = HashMap::new();
    // The span the *first* phase gave a trajectory.
    //
    // **A span is an identity, not a snapshot**, which is what separates it from the fields
    // beside it. A delegation is spawned by exactly one call, and a long-lived agent is then
    // messaged again — each message a fresh host call — so its later notifications name a
    // different `tool_use` than the one that started it. Measured, 29 delegates carry
    // notifications from between 2 and 9 distinct hosts, and the spawning call is the first in
    // 95% of them; keeping the latest reparented the consolidated block under whichever call
    // last spoke to the agent, so a card moved after its turn settled while the live tree
    // (which keeps the first) still showed it correctly. One fact, two derivations,
    // disagreeing at opposite ends.
    let mut first_span: HashMap<String, Option<String>> = HashMap::new();
    // The best name any phase gave a block, since not every phase carries one.
    let mut names: HashMap<String, String> = HashMap::new();
    // The strongest interior reachability claim observed for each trajectory. A lifecycle
    // notification can arrive after the bundle reader has already proved the source readable;
    // retaining the last event verbatim would regress `partial` back to `pending` solely due to
    // producer order. This fold is the one place that resolves phases into durable state.
    let mut interiors: HashMap<String, String> = HashMap::new();
    // Trajectories declared here and never closed, keyed by id, holding the latest
    // `subagent` row that spoke about each.
    //
    // Without this a declared-but-open trajectory folds into *no block at all*: its
    // `started` phase carries no text, so it never reaches `pending` either, and the
    // whole delegation disappears from history while its children survive and
    // reattach to the root. That used to be unreachable in practice, because the
    // turn-settle sweep closed everything before consolidation ran — and then
    // backgrounded work became a deliberate exception to the sweep, which made this
    // the *normal* path for exactly the delegations that outlive their turn.
    // Measured: a `Workflow`'s backgrounded delegate was absent from the dispatching
    // turn entirely, so the fix that stopped the sweep writing a wrong record had
    // traded it for no record.
    //
    // Kept at its `started` phase on purpose. A client reads a trajectory's status
    // off its phase, so `started` says *running*, which is what the work is. The
    // alternative — closing it here — would restate the contradiction consolidation
    // is not entitled to invent: nothing has reported an outcome yet.
    let mut open_trajectories: HashMap<String, Event> = HashMap::new();
    let mut blocks = Vec::new();

    for event in events {
        let stream_id = event.stream_id.clone();

        match event.phase.as_deref() {
            Some("started") | Some("delta") => {
                let Some(id) = stream_id else { continue };
                if event.kind == "subagent" {
                    // Latest wins for what a later phase legitimately revises — a name, a
                    // flag, a progress snapshot. **Not for `span`**, which `first_span`
                    // holds instead, because a trajectory has one parent however many
                    // times it is spoken to afterwards.
                    open_trajectories.insert(id.clone(), event.clone());
                    first_span
                        .entry(id.clone())
                        .or_insert_with(|| event.span.clone());
                    if let Some(state) = event.interior_state.as_deref() {
                        remember_interior_state(&mut interiors, id.clone(), state);
                    }
                }
                // `started_at_ms` is the arrival-clock anchor published to clients.
                // A vendor timestamp belongs to a different domain and cannot be
                // mixed into a duration that ends on the server clock.
                first_arrival
                    .entry(id.clone())
                    .or_insert(event.arrival_at_ms);
                // A name given by an earlier phase is kept, because for some blocks
                // that is the *only* phase that has it. A delegated trajectory is
                // named on `started` — or renamed by the `delta` that carries its
                // parsed brief — and its `completed` phase reports an outcome, not
                // an identity. Folding only the completed event's fields therefore
                // erased every subagent's name, and every card in the timeline read
                // "Subagent" after the turn consolidated. The symptom looked like a
                // frontend fallback firing; the name had been dropped two layers
                // below it.
                if let Some(name) = event.name.clone() {
                    names.insert(id.clone(), name);
                }
                if !event.text.is_empty() {
                    pending.entry(id).or_default().push(Delta {
                        text: event.text.clone(),
                        at_ms: event.arrival_at_ms,
                    });
                }
            }
            // `completed`, or no phase at all: server-authored events are stable
            // and unstreamed.
            _ => {
                if event.kind == "subagent" {
                    if let (Some(id), Some(state)) =
                        (stream_id.as_ref(), event.interior_state.as_deref())
                    {
                        remember_interior_state(&mut interiors, id.clone(), state);
                    }
                }
                // This trajectory has an outcome, so it is no longer open. A
                // `tool_result` counts as well as a `subagent` close: a promoted
                // host's lifecycle is its result (§ Promotion).
                if let Some(id) = stream_id.as_ref() {
                    if event.kind == "subagent" || event.kind == "tool_result" {
                        open_trajectories.remove(id);
                    }
                }

                let deltas = stream_id
                    .as_ref()
                    .and_then(|id| pending.remove(id))
                    .unwrap_or_default();
                let started_at_ms = stream_id
                    .as_ref()
                    .and_then(|id| first_arrival.remove(id))
                    .unwrap_or(event.arrival_at_ms);

                // Resolved before the id is moved into the block below.
                let name = event
                    .name
                    .clone()
                    .or_else(|| stream_id.as_ref().and_then(|id| names.remove(id)));
                let interior_state = stream_id
                    .as_ref()
                    .and_then(|id| interiors.remove(id))
                    .or_else(|| event.interior_state.clone());

                let folded = fold::fold(&deltas, &event.text, started_at_ms);
                if folded.is_none() && !deltas.is_empty() {
                    // The deltas did not rebuild the completed text. The stable
                    // text is kept and only delta-level replay is lost for this
                    // block, which is the safe direction — but it means an
                    // adapter tidied its final string, so it is worth knowing.
                    debug!(
                        "Deltas for {:?} did not rebuild their block; storing without replay",
                        stream_id
                    );
                }

                blocks.push(Block {
                    session_id: event.session_id.encode(),
                    seq: event.seq,
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    kind: event.kind.clone(),
                    phase: event.phase.clone(),
                    stream_id,
                    span: event.span.clone(),
                    interior_state,
                    // The completed event's own name wins when it has one; a name
                    // from an earlier phase fills in when it does not.
                    name,
                    text: event.text.clone(),
                    folded,
                    data: data_from_column(&event.data_json),
                    is_error: event.is_error,
                    source: event.source.clone(),
                    arrival_at_ms: event.arrival_at_ms,
                    recorded_at_ms: event.recorded_at_ms,
                    started_at_ms,
                    created_at_ms: event.created_at_ms,
                });
            }
        }
    }

    // Trajectories still open when the turn ended. Preserved rather than dropped, and
    // preserved *as open* — see `open_trajectories` above for why both halves matter.
    //
    // Emitted before the leftover-delta pass so a trajectory that also streamed text
    // is claimed here, with its identity intact, rather than there as an anonymous
    // `incomplete` block.
    for (stream_id, declaring) in open_trajectories {
        let deltas = pending.remove(&stream_id).unwrap_or_default();
        let started_at_ms = first_arrival
            .get(&stream_id)
            .copied()
            .unwrap_or(declaring.arrival_at_ms);
        let text: String = deltas.iter().map(|d| d.text.as_str()).collect();
        let folded = fold::fold(&deltas, &text, started_at_ms);

        blocks.push(Block {
            session_id: declaring.session_id.encode(),
            seq: declaring.seq,
            turn_id: declaring.turn_id.clone().unwrap_or_default(),
            kind: declaring.kind.clone(),
            // The phase *is* the claim: this delegation has not reported an outcome.
            phase: Some("started".to_string()),
            // The spawning call, not whichever one last spoke to this agent.
            span: first_span
                .remove(&stream_id)
                .flatten()
                .or_else(|| declaring.span.clone()),
            interior_state: interiors.remove(&stream_id).or(declaring.interior_state),
            name: declaring.name.clone().or_else(|| names.remove(&stream_id)),
            stream_id: Some(stream_id.clone()),
            text,
            folded,
            // Carried forward because it is what says this was expected to outlive
            // its turn — the one durable record of that, once the journal rows are
            // gone.
            data: data_from_column(&declaring.data_json),
            is_error: false,
            source: declaring.source.clone(),
            arrival_at_ms: declaring.arrival_at_ms,
            recorded_at_ms: declaring.recorded_at_ms,
            started_at_ms,
            created_at_ms: declaring.created_at_ms,
        });
    }

    // Deltas whose `completed` never arrived — the link died mid-block. Kept as a
    // block of what did arrive, because losing a half-streamed answer is exactly
    // the case a user most wants to see.
    for (stream_id, deltas) in pending {
        if deltas.is_empty() {
            continue;
        }
        let text: String = deltas.iter().map(|d| d.text.as_str()).collect();
        let started_at_ms = first_arrival
            .get(&stream_id)
            .copied()
            .unwrap_or_else(|| deltas[0].at_ms);
        let last = deltas.last().map(|d| d.at_ms).unwrap_or(started_at_ms);
        let folded = fold::fold(&deltas, &text, started_at_ms);

        warn!(
            "Block {} was never completed; storing what arrived",
            stream_id
        );

        blocks.push(Block {
            // Every event in this fold belongs to one session, so the first row's id is
            // the block's. An empty run cannot reach here — the caller only folds a
            // stream that has deltas — and `String::new()` rather than a panic keeps a
            // malformed fold from taking the process down over a block it could still
            // write.
            session_id: events
                .first()
                .map(|e| e.session_id.encode())
                .unwrap_or_default(),
            // Sequence numbers order the stream, and an incomplete block has none
            // of its own. The last delta's is the closest true position.
            seq: events
                .iter()
                .filter(|e| e.stream_id.as_deref() == Some(stream_id.as_str()))
                .map(|e| e.seq)
                .max()
                .unwrap_or(0),
            turn_id: events
                .first()
                .and_then(|e| e.turn_id.clone())
                .unwrap_or_default(),
            kind: events
                .iter()
                .find(|e| e.stream_id.as_deref() == Some(stream_id.as_str()))
                .map(|e| e.kind.clone())
                .unwrap_or_else(|| "assistant".to_string()),
            phase: Some("incomplete".to_string()),
            // Read from the event that opened it, like `kind` above: an interrupted
            // block still belongs to the trajectory it ran in, and dropping that
            // would reparent the tail of an abandoned delegation onto the root.
            span: events
                .iter()
                .find(|e| e.stream_id.as_deref() == Some(stream_id.as_str()))
                .and_then(|e| e.span.clone()),
            interior_state: interiors.remove(&stream_id),
            // Same reason as the folded path: for a block whose only phases were
            // `started` and `delta`, that is the only place a name ever appeared.
            // A trajectory the agent abandoned is exactly this case, so leaving it
            // None would make the one delegation that needs explaining anonymous.
            name: names.remove(&stream_id),
            stream_id: Some(stream_id.clone()),
            text,
            folded,
            data: None,
            is_error: false,
            source: "agent".to_string(),
            arrival_at_ms: deltas.last().map(|_| last).unwrap_or(last),
            recorded_at_ms: events
                .iter()
                .filter(|event| event.stream_id.as_deref() == Some(stream_id.as_str()))
                .rev()
                .find_map(|event| event.recorded_at_ms),
            started_at_ms,
            created_at_ms: last,
        });
    }

    blocks.sort_by_key(|block| block.seq);
    blocks
}

/// Keep the most informative reachability state for one trajectory.
///
/// `pending` is a temporary observation and must not overwrite evidence that readable content
/// already arrived. `unreadable` is terminal and wins over non-terminal states; `full` is the
/// strongest positive claim, while `partial` still says the source is reachable but incomplete.
fn remember_interior_state(states: &mut HashMap<String, String>, id: String, state: &str) {
    let Some(incoming) = sylph_protocol::InteriorState::parse(state) else {
        return;
    };
    let merged = states
        .get(&id)
        .and_then(|existing| sylph_protocol::InteriorState::parse(existing))
        .map_or(incoming, |current| current.merge(incoming));
    states.insert(id, merged.to_string());
}

/// Move a turn from the journal into history.
///
/// The ordering rule this module exists to enforce: insert, commit, *then* delete.
pub async fn consolidate_turn(
    journal: &crate::db::JournalStore,
    history: &HistoryStore,
    turn_id: &str,
) -> Result<usize> {
    let events = journal.turn_events_for_consolidation(turn_id).await?;
    if events.is_empty() {
        return Ok(0);
    }

    let blocks = consolidate(&events);
    let count = blocks.len();

    history.put_blocks(&blocks).await?;

    // Only now. A crash before this leaves the turn in both stores, and the next
    // attempt overwrites identical rows.
    journal.delete_turn_events(turn_id).await?;

    info!("Consolidated turn {} into {} blocks", turn_id, count);
    Ok(count)
}
