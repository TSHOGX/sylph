//! Durable receipt-gated capture spool. Exact bytes remain queued until terminal admission
//! or an authorized deletion receipt; reconnect retransmits the same observation identity.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use thiserror::Error;

pub mod attachments;
pub mod capture;
mod commands;
pub mod sources;

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

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("spool capacity exceeded: {used} + {incoming} > {maximum} bytes")]
    Overflow {
        used: u64,
        incoming: u64,
        maximum: u64,
    },
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
    readers: SqlitePool,
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
            .after_release(|connection, _| {
                Box::pin(async move {
                    use sqlx::Connection;
                    // SQLx 0.8 custom BEGIN has a cancellation await before its RAII
                    // transaction exists. Flush queued rollback first, then close an
                    // orphaned transaction instead of giving it to another capture.
                    connection.ping().await?;
                    Ok(!connection.is_in_transaction())
                })
            })
            .connect_with(options.clone())
            .await?;
        sqlx::migrate!("./migrations/spool")
            .run(&pool)
            .await
            .map_err(anyhow::Error::from)?;
        let readers = SqlitePoolOptions::new()
            .max_connections(2)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA query_only = ON").execute(conn).await?;
                    Ok(())
                })
            })
            .connect_with(options.read_only(true))
            .await?;
        Ok(Self {
            path: config.path,
            max_bytes: config.max_bytes,
            pool,
            readers,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub async fn size_bytes(&self) -> Result<u64, SpoolError> {
        let value: i64 = sqlx::query_scalar("SELECT bytes FROM spool_usage WHERE singleton = 1")
            .fetch_one(&self.readers)
            .await?;
        Ok(u64::try_from(value).unwrap_or(u64::MAX))
    }

    pub async fn is_empty(&self) -> Result<bool, SpoolError> {
        let count: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM capture_queue) OR EXISTS(SELECT 1 FROM capture_epoch_progress WHERE tail_bytes>0)")
            .fetch_one(&self.readers)
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
        self.readers.close().await;
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

#[cfg(test)]
mod release_tests {
    use super::*;
    use sqlx::{sqlite::SqliteTransactionManager, Connection, TransactionManager};

    #[tokio::test]
    async fn spool_reuses_ordinary_rollbacks_but_discards_orphaned_custom_begins() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::open(SpoolConfig::new(dir.path().join("spool.db"), 1024 * 1024))
            .await
            .unwrap();
        let mut connection = spool.pool.acquire().await.unwrap();
        sqlx::query("CREATE TEMP TABLE release_probe(value TEXT)")
            .execute(&mut *connection)
            .await
            .unwrap();
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("INSERT INTO release_probe VALUES('rollback')")
            .execute(&mut *transaction)
            .await
            .unwrap();
        drop(transaction);
        drop(connection);
        let mut connection = spool.pool.acquire().await.unwrap();
        assert!(!connection.is_in_transaction());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM release_probe")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            0
        );
        SqliteTransactionManager::begin(&mut connection, Some("BEGIN IMMEDIATE".into()))
            .await
            .unwrap();
        drop(connection);
        let mut connection = spool.pool.acquire().await.unwrap();
        assert!(!connection.is_in_transaction());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE name='release_probe'"
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap(),
            0,
            "orphaned begin requires a fresh connection"
        );
        connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
}
