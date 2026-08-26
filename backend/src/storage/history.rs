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

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous};
use sqlx::Row;
use tracing::{debug, info, warn};

use crate::events::data_from_column;
use crate::models::Event;
use crate::supervisor::TrajectoryOutcome;
use crate::storage::cas::{self, BlobStore, Payload};
use crate::storage::fold::{self, Delta, Folded};

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
    pub name: Option<String>,
    pub text: String,
    pub folded: Option<Folded>,
    pub data: Option<serde_json::Value>,
    pub is_error: bool,
    pub source: String,
    pub started_at_ms: i64,
    pub created_at_ms: i64,
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
            .synchronous(SqliteSynchronous::Full);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .with_context(|| format!("Could not open the history store at {}", db_path))?;

        sqlx::migrate!("./migrations/history")
            .run(&pool)
            .await
            .context("History migrations failed")?;

        Ok(Self { pool, blobs })
    }

    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
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
            let data = match &block.data {
                Some(value) if !value.is_null() => {
                    let rendered = value.to_string();
                    Some(cas::store_text(&self.blobs, &rendered).await?)
                }
                _ => None,
            };
            payloads.push((text, data));
        }

        let mut tx = self.pool.begin().await?;

        for (block, (text, data)) in blocks.iter().zip(&payloads) {
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
                    session_id, seq, turn_id, kind, phase, stream_id, span, name,
                    text_inline, text_hash, text_len, text_encoding,
                    delta_offsets, delta_timings,
                    data_json, data_hash, is_error, source,
                    started_at_ms, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&block.session_id)
            .bind(block.seq)
            .bind(&block.turn_id)
            .bind(&block.kind)
            .bind(&block.phase)
            .bind(&block.stream_id)
            .bind(&block.span)
            .bind(&block.name)
            .bind(text_inline)
            .bind(&text_hash)
            .bind(block.text.len() as i64)
            .bind(text.encoding())
            .bind(block.folded.as_ref().map(|f| fold::pack(&f.offsets)))
            .bind(block.folded.as_ref().map(|f| fold::pack(&f.timings)))
            .bind(data_inline)
            .bind(&data_hash)
            .bind(block.is_error)
            .bind(&block.source)
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
        }

        tx.commit().await?;
        Ok(())
    }

    /// A session's blocks in stream order, resolving external payloads.
    pub async fn blocks(&self, session_id: &str, after_seq: i32) -> Result<Vec<Block>> {
        let rows = sqlx::query(
            "SELECT * FROM blocks WHERE session_id = ? AND seq > ? ORDER BY seq",
        )
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
            ("external", _, Some(hash)) => Payload::External { hash },
            ("zstd", Some(bytes), _) => Payload::Zstd(bytes),
            (_, Some(bytes), _) => Payload::Raw(bytes),
            // A row with neither is a block whose text was legitimately empty.
            _ => Payload::Raw(Vec::new()),
        };
        let text = cas::load_text(&self.blobs, &payload).await?;

        let data_encoding: Option<String> = row.try_get("text_encoding").ok();
        let data_inline: Option<Vec<u8>> = row.try_get("data_json")?;
        let data_hash: Option<String> = row.try_get("data_hash")?;
        let data = match (data_inline, data_hash) {
            (_, Some(hash)) => {
                let rendered =
                    cas::load_text(&self.blobs, &Payload::External { hash }).await?;
                serde_json::from_str(&rendered).ok()
            }
            (Some(bytes), None) => {
                // Data uses the same encoding decision as text, so a compressed
                // payload has to be tried both ways: a short JSON object stays
                // raw while a large one is compressed.
                // Data follows the same encoding decision as text, so a
                // compressed payload has to be tried both ways: a short JSON
                // object stays raw while a large one is compressed. A failed
                // decompress falls back to raw rather than failing the read — a
                // block whose metadata will not decode is still worth showing.
                let rendered = match data_encoding.as_deref() {
                    Some("zstd") => {
                        match cas::load_text(&self.blobs, &Payload::Zstd(bytes.clone())).await {
                            Ok(text) => text,
                            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
                        }
                    }
                    _ => String::from_utf8_lossy(&bytes).into_owned(),
                };
                serde_json::from_str(&rendered).ok()
            }
            (None, None) => None,
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
            name: row.try_get("name")?,
            text,
            folded,
            data,
            is_error: row.try_get::<i64, _>("is_error")? != 0,
            source: row.try_get("source")?,
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
        let result = sqlx::query("DELETE FROM blocks WHERE session_id = ? AND seq <= ?")
            .bind(session_id)
            .bind(confirmed_seq)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Delete blobs nothing references any more, returning how many.
    ///
    /// Separate from pruning because reachability is not a property of one session:
    /// the same file read in two sessions is one blob, and it survives until both
    /// have let go.
    pub async fn sweep_unreferenced_blobs(&self) -> Result<u64> {
        let orphans: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT hash FROM cas_refs
              WHERE NOT EXISTS (
                SELECT 1 FROM blocks
                 WHERE blocks.session_id = cas_refs.session_id
                   AND blocks.seq = cas_refs.seq
              )",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut removed = 0;
        for hash in orphans {
            // Still referenced by a *live* block elsewhere? Then only this row is
            // stale, not the object.
            let live: i64 = sqlx::query_scalar(
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

            sqlx::query("DELETE FROM cas_refs WHERE hash = ? AND NOT EXISTS (
                SELECT 1 FROM blocks
                 WHERE blocks.session_id = cas_refs.session_id
                   AND blocks.seq = cas_refs.seq
              )")
                .bind(&hash)
                .execute(&self.pool)
                .await?;

            if live == 0 {
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
               session_id, authority, confirmed_seq, checked_at_ms,
               archive_id, source_fingerprint
             )
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
               authority = excluded.authority,
               confirmed_seq = excluded.confirmed_seq,
               checked_at_ms = excluded.checked_at_ms,
               archive_id = excluded.archive_id,
               source_fingerprint = excluded.source_fingerprint",
        )
        .bind(session_id)
        .bind(authority)
        .bind(confirmation.safe_to_drop)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(confirmation.archive_id.as_deref())
        .bind(confirmation.source_fingerprint.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The fingerprint recorded for a session at its last pass, if any.
    ///
    /// A pass compares this against what the archive reports now. It cannot gate
    /// the `show` call — the current fingerprint arrives in that same response —
    /// so this is not an optimization: it is how "the archive re-ingested this
    /// session" becomes observable. A pass where every session is unchanged means
    /// the copy is not advancing, which is what a stopped ingestion daemon looks
    /// like, and which is otherwise indistinguishable from a healthy steady state.
    pub async fn watermark_fingerprint(&self, session_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT source_fingerprint FROM watermarks WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map(Option::flatten)
        .map_err(Into::into)
    }

    pub async fn watermark(&self, session_id: &str) -> Result<Option<(String, Option<i32>)>> {
        let row = sqlx::query("SELECT authority, confirmed_seq FROM watermarks WHERE session_id = ?")
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
    /// This is how a TraceDB watermark becomes a sequence number: the archive
    /// reports a timestamp for what it holds, and retention needs the position in
    /// this session's stream that corresponds to it.
    pub async fn max_seq_at_or_before(
        &self,
        session_id: &str,
        at_ms: i64,
    ) -> Result<Option<i32>> {
        sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(seq) FROM blocks WHERE session_id = ? AND created_at_ms <= ?",
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
    /// `data_json` is measured as stored, because it has no `_len` companion and
    /// is small by construction — anything over 4 KB was externalized.
    ///
    /// The consequence to hold onto: the sum over sessions is the size of the
    /// *record*, and the store on disk is smaller. This measure answers "which
    /// session is growing", not "what is my disk doing" — `du` answers that, and
    /// answers it exactly.
    pub async fn measure_session_bytes(&self, session_id: &str) -> Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(text_len), 0) + COALESCE(SUM(LENGTH(data_json)), 0) AS bytes,
                    COUNT(*) AS blocks
               FROM blocks WHERE session_id = ?",
        )
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
        let (bytes, blocks) = self.measure_session_bytes(session_id).await?;
        let now = chrono::Utc::now().timestamp_millis();

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
        .execute(&self.pool)
        .await
        .context("Could not record a session's held bytes")?;

        self.session_bytes(session_id)
            .await?
            .context("a row just written must be readable")
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

    /// Sessions holding blocks, least-recently-checked first.
    ///
    /// Ordered by the last pass rather than by id, so a bounded batch rotates
    /// through every session instead of re-examining the same alphabetical prefix
    /// forever. A session never checked has no watermark row and sorts first, which
    /// is what makes a fresh install converge on the whole set rather than starving
    /// whatever happens to sort late.
    ///
    /// Only sessions with blocks appear: a session whose history is already empty
    /// has nothing to prune, so spending a `show` round trip on it would be pure
    /// cost. This is also why the batch is drawn from `blocks` rather than from the
    /// journal's session list — the work is proportional to what history holds, not
    /// to how many sessions exist.
    pub async fn sessions_awaiting_retention(&self, limit: i64) -> Result<Vec<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT b.session_id FROM (
               SELECT DISTINCT session_id FROM blocks
             ) b
             LEFT JOIN watermarks w ON w.session_id = b.session_id
             ORDER BY w.checked_at_ms IS NOT NULL, w.checked_at_ms, b.session_id
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
    // The best name any phase gave a block, since not every phase carries one.
    let mut names: HashMap<String, String> = HashMap::new();
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
                    // Latest wins: progress is a snapshot, and a later phase may carry
                    // a name or a flag the first did not.
                    open_trajectories.insert(id.clone(), event.clone());
                }
                first_arrival.entry(id.clone()).or_insert(event.created_at_ms);
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
                        at_ms: event.created_at_ms,
                    });
                }
            }
            // `completed`, or no phase at all: server-authored events are stable
            // and unstreamed.
            _ => {
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
                    .unwrap_or(event.created_at_ms);

                // Resolved before the id is moved into the block below.
                let name = event.name.clone().or_else(|| {
                    stream_id.as_ref().and_then(|id| names.remove(id))
                });

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
                    session_id: event.session_id.clone(),
                    seq: event.seq,
                    turn_id: event.turn_id.clone().unwrap_or_default(),
                    kind: event.kind.clone(),
                    phase: event.phase.clone(),
                    stream_id,
                    span: event.span.clone(),
                    // The completed event's own name wins when it has one; a name
                    // from an earlier phase fills in when it does not.
                    name,
                    text: event.text.clone(),
                    folded,
                    data: data_from_column(&event.data_json),
                    is_error: event.is_error,
                    source: event.source.clone(),
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
            .unwrap_or(declaring.created_at_ms);
        let text: String = deltas.iter().map(|d| d.text.as_str()).collect();
        let folded = fold::fold(&deltas, &text, started_at_ms);

        blocks.push(Block {
            session_id: declaring.session_id.clone(),
            seq: declaring.seq,
            turn_id: declaring.turn_id.clone().unwrap_or_default(),
            kind: declaring.kind.clone(),
            // The phase *is* the claim: this delegation has not reported an outcome.
            phase: Some("started".to_string()),
            span: declaring.span.clone(),
            name: declaring
                .name
                .clone()
                .or_else(|| names.remove(&stream_id)),
            stream_id: Some(stream_id),
            text,
            folded,
            // Carried forward because it is what says this was expected to outlive
            // its turn — the one durable record of that, once the journal rows are
            // gone.
            data: data_from_column(&declaring.data_json),
            is_error: false,
            source: declaring.source.clone(),
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

        warn!("Block {} was never completed; storing what arrived", stream_id);

        blocks.push(Block {
            session_id: events
                .first()
                .map(|e| e.session_id.clone())
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
            // Same reason as the folded path: for a block whose only phases were
            // `started` and `delta`, that is the only place a name ever appeared.
            // A trajectory the agent abandoned is exactly this case, so leaving it
            // None would make the one delegation that needs explaining anonymous.
            name: names.remove(&stream_id),
            stream_id: Some(stream_id),
            text,
            folded,
            data: None,
            is_error: false,
            source: "agent".to_string(),
            started_at_ms,
            created_at_ms: last,
        });
    }

    blocks.sort_by_key(|block| block.seq);
    blocks
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
