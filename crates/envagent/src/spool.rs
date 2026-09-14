//! Durable ack-gated event spool.
//!
//! The spool is a bounded buffer, not a second history authority. An event is
//! inserted before it is sent; rows are removed only after the server confirms a
//! contiguous producer prefix. Reconnect therefore always resumes at the first
//! unacknowledged sequence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::Row;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SpoolConfig {
    pub path: PathBuf,
    pub max_bytes: u64,
}

impl SpoolConfig {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            path: path.into(),
            max_bytes: max_bytes.max(1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpoolEvent {
    pub producer_id: String,
    pub producer_seq: u64,
    pub kind: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_turn_id: Option<String>,
    pub payload: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at_ms: Option<i64>,
}

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("spool capacity exceeded: {used} + {incoming} > {maximum} bytes")]
    Overflow {
        used: u64,
        incoming: u64,
        maximum: u64,
    },
    #[error("producer sequence {producer_id}:{producer_seq} already exists with different data")]
    ConflictingDuplicate {
        producer_id: String,
        producer_seq: u64,
    },
    #[error("producer sequence is above SQLite's signed integer limit: {0}")]
    SequenceOutOfRange(u64),
    #[error("acknowledgement for {producer_id} skips a spool gap before {through}")]
    AckGap { producer_id: String, through: u64 },
    #[error("spool database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("spool I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("spool setup error: {0}")]
    Setup(#[from] anyhow::Error),
}

#[derive(Clone)]
pub struct Spool {
    path: PathBuf,
    max_bytes: u64,
    pool: SqlitePool,
}

impl Spool {
    pub async fn open(config: SpoolConfig) -> Result<Self, SpoolError> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(&config.path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS spool_events (
              producer_id TEXT NOT NULL,
              producer_seq INTEGER NOT NULL,
              kind TEXT NOT NULL DEFAULT 'event',
              session_id TEXT NOT NULL,
              arrival_turn_id TEXT,
              payload BLOB NOT NULL,
              source_cursor TEXT,
              recorded_at_ms INTEGER,
              PRIMARY KEY (producer_id, producer_seq)
            );
            CREATE INDEX IF NOT EXISTS spool_events_order
              ON spool_events (producer_id, producer_seq);
            CREATE TABLE IF NOT EXISTS spool_acks (
              producer_id TEXT PRIMARY KEY,
              acked_through INTEGER NOT NULL DEFAULT -1
            );
            CREATE TABLE IF NOT EXISTS spool_sequences (
              producer_id TEXT PRIMARY KEY,
              next_seq INTEGER NOT NULL DEFAULT 1
            );
            "#,
        )
        .execute(&pool)
        .await?;
        Ok(Self {
            path: config.path,
            max_bytes: config.max_bytes,
            pool,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Allocate and persist the next producer sequence before an event is sent.
    pub async fn next_sequence(&self, producer_id: &str) -> Result<u64, SpoolError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO spool_sequences(producer_id, next_seq) VALUES (?, 1) \
             ON CONFLICT(producer_id) DO NOTHING",
        )
        .bind(producer_id)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            "UPDATE spool_sequences SET next_seq = next_seq + 1 \
             WHERE producer_id = ? RETURNING next_seq - 1 AS allocated",
        )
        .bind(producer_id)
        .fetch_one(&mut *tx)
        .await?;
        let allocated: i64 = row.try_get("allocated")?;
        tx.commit().await?;
        u64::try_from(allocated).map_err(|_| SpoolError::SequenceOutOfRange(u64::MAX))
    }

    /// Append an event, refusing overflow and accepting an exact duplicate.
    pub async fn append(&self, event: &SpoolEvent) -> Result<(), SpoolError> {
        let producer_seq = to_i64(event.producer_seq)?;
        let mut tx = self.pool.begin().await?;
        if let Some(existing) = sqlx::query(
            "SELECT kind, session_id, arrival_turn_id, payload, source_cursor, recorded_at_ms \
             FROM spool_events WHERE producer_id = ? AND producer_seq = ?",
        )
        .bind(&event.producer_id)
        .bind(producer_seq)
        .fetch_optional(&mut *tx)
        .await?
        {
            let same = existing.try_get::<String, _>("kind")? == event.kind
                && existing.try_get::<String, _>("session_id")? == event.session_id
                && existing.try_get::<Option<String>, _>("arrival_turn_id")?
                    == event.arrival_turn_id
                && existing.try_get::<Vec<u8>, _>("payload")? == event.payload
                && existing.try_get::<Option<String>, _>("source_cursor")? == event.source_cursor
                && existing.try_get::<Option<i64>, _>("recorded_at_ms")? == event.recorded_at_ms;
            tx.rollback().await?;
            if same {
                return Ok(());
            }
            return Err(SpoolError::ConflictingDuplicate {
                producer_id: event.producer_id.clone(),
                producer_seq: event.producer_seq,
            });
        }

        let used: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(payload)), 0) FROM spool_events")
                .fetch_one(&mut *tx)
                .await?;
        let used = u64::try_from(used).unwrap_or(u64::MAX);
        let incoming = event.payload.len() as u64;
        if used.saturating_add(incoming) > self.max_bytes {
            tx.rollback().await?;
            return Err(SpoolError::Overflow {
                used,
                incoming,
                maximum: self.max_bytes,
            });
        }
        sqlx::query(
            "INSERT INTO spool_events \
            (producer_id, producer_seq, kind, session_id, arrival_turn_id, payload, source_cursor, recorded_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.producer_id)
        .bind(producer_seq)
        .bind(&event.kind)
        .bind(&event.session_id)
        .bind(&event.arrival_turn_id)
        .bind(&event.payload)
        .bind(&event.source_cursor)
        .bind(event.recorded_at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Allocate the next producer sequence and append the event in one
    /// SQLite transaction. This is the normal runtime path: a crash cannot
    /// consume a sequence between two commits and create an unreplayable gap.
    pub async fn append_next(&self, mut event: SpoolEvent) -> Result<u64, SpoolError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO spool_sequences(producer_id, next_seq) VALUES (?, 1) \
             ON CONFLICT(producer_id) DO NOTHING",
        )
        .bind(&event.producer_id)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            "UPDATE spool_sequences SET next_seq = next_seq + 1 \
             WHERE producer_id = ? RETURNING next_seq - 1 AS allocated",
        )
        .bind(&event.producer_id)
        .fetch_one(&mut *tx)
        .await?;
        let allocated: i64 = row.try_get("allocated")?;
        event.producer_seq =
            u64::try_from(allocated).map_err(|_| SpoolError::SequenceOutOfRange(u64::MAX))?;

        let used: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(payload)), 0) FROM spool_events")
                .fetch_one(&mut *tx)
                .await?;
        let used = u64::try_from(used).unwrap_or(u64::MAX);
        let incoming = event.payload.len() as u64;
        if used.saturating_add(incoming) > self.max_bytes {
            tx.rollback().await?;
            return Err(SpoolError::Overflow {
                used,
                incoming,
                maximum: self.max_bytes,
            });
        }
        sqlx::query(
            "INSERT INTO spool_events \
            (producer_id, producer_seq, kind, session_id, arrival_turn_id, payload, source_cursor, recorded_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.producer_id)
        .bind(
            i64::try_from(event.producer_seq)
                .map_err(|_| SpoolError::SequenceOutOfRange(event.producer_seq))?,
        )
        .bind(&event.kind)
        .bind(&event.session_id)
        .bind(&event.arrival_turn_id)
        .bind(&event.payload)
        .bind(&event.source_cursor)
        .bind(event.recorded_at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(event.producer_seq)
    }

    /// Read pending rows in producer order, starting at the supplied sequence.
    pub async fn pending_from(
        &self,
        producer_id: &str,
        first_seq: u64,
        limit: usize,
    ) -> Result<Vec<SpoolEvent>, SpoolError> {
        let first_seq = to_i64(first_seq)?;
        let rows = sqlx::query(
            "SELECT producer_id, producer_seq, kind, session_id, arrival_turn_id, payload, source_cursor, recorded_at_ms \
             FROM spool_events WHERE producer_id = ? AND producer_seq >= ? \
             ORDER BY producer_seq LIMIT ?",
        )
        .bind(producer_id)
        .bind(first_seq)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(SpoolEvent {
                    producer_id: row.try_get("producer_id")?,
                    producer_seq: u64::try_from(row.try_get::<i64, _>("producer_seq")?)
                        .map_err(|_| sqlx::Error::Protocol("negative producer sequence".into()))?,
                    kind: row.try_get("kind")?,
                    session_id: row.try_get("session_id")?,
                    arrival_turn_id: row.try_get("arrival_turn_id")?,
                    payload: row.try_get("payload")?,
                    source_cursor: row.try_get("source_cursor")?,
                    recorded_at_ms: row.try_get("recorded_at_ms")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(SpoolError::Database)
    }

    pub async fn pending(
        &self,
        producer_id: &str,
        limit: usize,
    ) -> Result<Vec<SpoolEvent>, SpoolError> {
        let acked = self.acked_through(producer_id).await?;
        self.pending_from(producer_id, acked.saturating_add(1), limit)
            .await
    }

    /// Delete exactly the contiguous prefix acknowledged by the server.
    pub async fn ack_through(&self, producer_id: &str, through: u64) -> Result<usize, SpoolError> {
        let through_i64 = to_i64(through)?;
        let mut tx = self.pool.begin().await?;
        let current: i64 = sqlx::query_scalar(
            "SELECT COALESCE((SELECT acked_through FROM spool_acks WHERE producer_id = ?), 0)",
        )
        .bind(producer_id)
        .fetch_one(&mut *tx)
        .await?;
        if through_i64 <= current {
            tx.rollback().await?;
            return Ok(0);
        }
        let expected = current.saturating_add(1);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM spool_events \
             WHERE producer_id = ? AND producer_seq BETWEEN ? AND ?",
        )
        .bind(producer_id)
        .bind(expected)
        .bind(through_i64)
        .fetch_one(&mut *tx)
        .await?;
        if count != through_i64.saturating_sub(expected).saturating_add(1) {
            tx.rollback().await?;
            return Err(SpoolError::AckGap {
                producer_id: producer_id.to_string(),
                through,
            });
        }
        let deleted =
            sqlx::query("DELETE FROM spool_events WHERE producer_id = ? AND producer_seq <= ?")
                .bind(producer_id)
                .bind(through_i64)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
        sqlx::query(
            "INSERT INTO spool_acks(producer_id, acked_through) VALUES (?, ?) \
             ON CONFLICT(producer_id) DO UPDATE SET acked_through = MAX(acked_through, excluded.acked_through)",
        )
        .bind(producer_id)
        .bind(through_i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(deleted)
    }

    pub async fn acked_through(&self, producer_id: &str) -> Result<u64, SpoolError> {
        let value: Option<i64> =
            sqlx::query_scalar("SELECT acked_through FROM spool_acks WHERE producer_id = ?")
                .bind(producer_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(value
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0))
    }

    pub async fn pending_watermark(&self, producer_id: &str) -> Result<u64, SpoolError> {
        let through: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(producer_seq), 0) FROM spool_events WHERE producer_id = ?",
        )
        .bind(producer_id)
        .fetch_one(&self.pool)
        .await?;
        u64::try_from(through)
            .map_err(|_| sqlx::Error::Protocol("negative producer sequence".into()).into())
    }

    pub async fn size_bytes(&self) -> Result<u64, SpoolError> {
        let value: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(length(payload)), 0) FROM spool_events")
                .fetch_one(&self.pool)
                .await?;
        Ok(u64::try_from(value).unwrap_or(u64::MAX))
    }

    pub async fn is_empty(&self) -> Result<bool, SpoolError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM spool_events")
            .fetch_one(&self.pool)
            .await?;
        Ok(count == 0)
    }

    pub async fn checkpoint(&self) -> Result<(), SpoolError> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Checkpoint and move a WAL-mode spool safely. The active pool is closed;
    /// callers should open the destination as a new spool.
    pub async fn checkpoint_and_rename(
        self,
        destination: impl AsRef<Path>,
    ) -> Result<(), SpoolError> {
        let destination = destination.as_ref().to_path_buf();
        if destination.exists() {
            return Err(SpoolError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("destination {} already exists", destination.display()),
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.checkpoint().await?;
        self.pool.close().await;
        std::fs::rename(&self.path, &destination)?;
        for sidecar in [
            self.path.with_extension("db-wal"),
            self.path.with_extension("db-shm"),
        ] {
            if sidecar.exists() {
                std::fs::remove_file(sidecar)?;
            }
        }
        Ok(())
    }
}

fn to_i64(value: u64) -> Result<i64, SpoolError> {
    i64::try_from(value).map_err(|_| SpoolError::SequenceOutOfRange(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spool(max_bytes: u64) -> Spool {
        let dir = tempfile::tempdir().unwrap();
        // Keep the directory alive by leaking it for the duration of this
        // small unit test; the database itself owns no external resources.
        let path = dir.path().join("spool.db");
        std::mem::forget(dir);
        Spool::open(SpoolConfig::new(path, max_bytes))
            .await
            .unwrap()
    }

    fn event(seq: u64, bytes: &[u8]) -> SpoolEvent {
        SpoolEvent {
            producer_id: "p".into(),
            producer_seq: seq,
            kind: "event".into(),
            session_id: "env:agent:native".into(),
            arrival_turn_id: None,
            payload: bytes.to_vec(),
            source_cursor: Some(format!("offset:{seq}")),
            recorded_at_ms: Some(seq as i64),
        }
    }

    #[tokio::test]
    async fn sequence_and_ack_are_durable_and_contiguous() {
        let spool = spool(1024).await;
        assert_eq!(spool.next_sequence("p").await.unwrap(), 1);
        assert_eq!(spool.next_sequence("p").await.unwrap(), 2);
        spool.append(&event(1, b"a")).await.unwrap();
        spool.append(&event(2, b"b")).await.unwrap();
        assert_eq!(spool.pending("p", 10).await.unwrap().len(), 2);
        assert_eq!(spool.ack_through("p", 1).await.unwrap(), 1);
        assert_eq!(spool.pending("p", 10).await.unwrap()[0].producer_seq, 2);
        assert_eq!(spool.acked_through("p").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn exact_replay_is_idempotent_but_conflicting_replay_is_refused() {
        let spool = spool(1024).await;
        spool.append(&event(2, b"same")).await.unwrap();
        spool.append(&event(2, b"same")).await.unwrap();
        assert!(matches!(
            spool.append(&event(2, b"different")).await,
            Err(SpoolError::ConflictingDuplicate { .. })
        ));
    }

    #[tokio::test]
    async fn overflow_is_reported_without_dropping_existing_events() {
        let spool = spool(3).await;
        spool.append(&event(1, b"abc")).await.unwrap();
        assert!(matches!(
            spool.append(&event(2, b"d")).await,
            Err(SpoolError::Overflow { .. })
        ));
        assert_eq!(spool.pending("p", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn atomic_append_does_not_consume_a_sequence_on_overflow() {
        let spool = spool(3).await;
        let mut first = event(0, b"abcd");
        first.producer_seq = 0;
        assert!(matches!(
            spool.append_next(first).await,
            Err(SpoolError::Overflow { .. })
        ));

        let second = event(0, b"ok");
        assert_eq!(spool.append_next(second).await.unwrap(), 1);
        assert_eq!(spool.pending("p", 10).await.unwrap()[0].producer_seq, 1);
    }

    #[tokio::test]
    async fn allocated_sequences_survive_reopening_the_spool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spool.db");
        let spool = Spool::open(SpoolConfig::new(path.clone(), 1024))
            .await
            .unwrap();
        assert_eq!(spool.append_next(event(0, b"one")).await.unwrap(), 1);
        spool.pool.close().await;

        let reopened = Spool::open(SpoolConfig::new(path, 1024)).await.unwrap();
        assert_eq!(reopened.append_next(event(0, b"two")).await.unwrap(), 2);
        let pending = reopened.pending("p", 10).await.unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|row| row.producer_seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn an_ack_that_skips_an_unspooled_sequence_is_rejected() {
        let spool = spool(1024).await;
        spool.append(&event(1, b"a")).await.unwrap();
        spool.append(&event(3, b"c")).await.unwrap();
        assert!(matches!(
            spool.ack_through("p", 3).await,
            Err(SpoolError::AckGap { .. })
        ));
        assert_eq!(spool.pending("p", 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn checkpoint_then_rename_keeps_rows_without_wal_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("spool.db");
        let destination = dir.path().join("moved").join("spool.db");
        let spool = Spool::open(SpoolConfig::new(&source, 1024)).await.unwrap();
        spool.append(&event(1, b"one")).await.unwrap();
        spool.checkpoint_and_rename(&destination).await.unwrap();
        assert!(destination.exists());
        assert!(!source.with_extension("db-wal").exists());
        let reopened = Spool::open(SpoolConfig::new(destination, 1024))
            .await
            .unwrap();
        assert_eq!(reopened.pending("p", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reopening_the_spool_replays_the_unacknowledged_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spool.db");
        {
            let spool = Spool::open(SpoolConfig::new(&path, 1024)).await.unwrap();
            let mut row = event(0, b"durable");
            row.producer_seq = 0;
            spool.append_next(row).await.unwrap();
        }
        let reopened = Spool::open(SpoolConfig::new(path, 1024)).await.unwrap();
        let pending = reopened.pending("p", 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload, b"durable");
    }
}
