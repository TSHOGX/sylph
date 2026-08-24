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

use crate::models::Event;
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
    pub name: Option<String>,
    pub text: String,
    pub folded: Option<Folded>,
    pub data: Option<serde_json::Value>,
    pub is_error: bool,
    pub source: String,
    pub started_at_ms: i64,
    pub created_at_ms: i64,
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
                    session_id, seq, turn_id, kind, phase, stream_id, name,
                    text_inline, text_hash, text_len, text_encoding,
                    delta_offsets, delta_timings,
                    data_json, data_hash, is_error, source,
                    started_at_ms, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&block.session_id)
            .bind(block.seq)
            .bind(&block.turn_id)
            .bind(&block.kind)
            .bind(&block.phase)
            .bind(&block.stream_id)
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

    /// Record how far an authority has confirmed a session.
    pub async fn set_watermark(
        &self,
        session_id: &str,
        authority: &str,
        confirmed_seq: Option<i32>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watermarks (session_id, authority, confirmed_seq, checked_at_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
               authority = excluded.authority,
               confirmed_seq = excluded.confirmed_seq,
               checked_at_ms = excluded.checked_at_ms",
        )
        .bind(session_id)
        .bind(authority)
        .bind(confirmed_seq)
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
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

    pub async fn block_count(&self, session_id: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*) FROM blocks WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .context("Could not count blocks")
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
    let mut blocks = Vec::new();

    for event in events {
        let stream_id = event.stream_id.clone();

        match event.phase.as_deref() {
            Some("started") | Some("delta") => {
                let Some(id) = stream_id else { continue };
                first_arrival.entry(id.clone()).or_insert(event.created_at_ms);
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
                let deltas = stream_id
                    .as_ref()
                    .and_then(|id| pending.remove(id))
                    .unwrap_or_default();
                let started_at_ms = stream_id
                    .as_ref()
                    .and_then(|id| first_arrival.remove(id))
                    .unwrap_or(event.created_at_ms);

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
                    name: event.name.clone(),
                    text: event.text.clone(),
                    folded,
                    data: match event.data_json.as_str() {
                        "" | "{}" => None,
                        raw => serde_json::from_str(raw).ok(),
                    },
                    is_error: event.is_error,
                    source: event.source.clone(),
                    started_at_ms,
                    created_at_ms: event.created_at_ms,
                });
            }
        }
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
            stream_id: Some(stream_id),
            name: None,
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
    journal: &crate::db::LiveStore,
    history: &HistoryStore,
    turn_id: &str,
) -> Result<usize> {
    let events = journal.turn_events_after(turn_id, -1).await?;
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
