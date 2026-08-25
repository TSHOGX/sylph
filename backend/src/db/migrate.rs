//! Renaming the journal file in place, for installs that predate the name.
//!
//! `live.db` became `journal.db` when the tiering was named after what each file
//! holds rather than when it is read. Installed data exists, so the old name has
//! to be carried forward rather than abandoned.
//!
//! **Checkpoint, then rename the main file alone** — never rename the three WAL
//! files together. A crash between renaming `-wal` and renaming `.db` leaves
//! committed data stranded in an orphaned WAL beside a database that no longer
//! knows about it, which is silent loss of exactly the turns that were in flight.
//! `wal_checkpoint(TRUNCATE)` folds the WAL back into the database and empties it,
//! so afterwards the sidecars carry nothing and their removal cannot lose a write.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tracing::{info, warn};

/// Rename a pre-existing `live.db` to `journal.db`, once.
///
/// A no-op when the new name already exists or the old one never did, so it is
/// safe on every startup. Failure is reported and not fatal: `JournalStore::new`
/// then creates a fresh `journal.db`, which loses history but keeps the server
/// serving — and the old file is still on disk to recover by hand.
pub async fn rename_legacy_journal(journal_path: &str) {
    let new_path = Path::new(journal_path);
    if new_path.exists() {
        return;
    }
    let Some(dir) = new_path.parent() else { return };
    let old_path = dir.join("live.db");
    if !old_path.exists() {
        return;
    }

    // Opened read-write because checkpointing is a write. WAL mode is stated
    // explicitly so this cannot be the call that converts the file's mode.
    let options = SqliteConnectOptions::new()
        .filename(&old_path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            warn!(
                "Could not open {} to rename it to journal.db: {}. Starting with an empty journal.",
                old_path.display(),
                error
            );
            return;
        }
    };

    let checkpointed = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await;
    // Every connection must be gone before the file moves: an open handle keeps
    // recreating the sidecars, and on some platforms holds the path itself.
    pool.close().await;

    if let Err(error) = checkpointed {
        warn!(
            "Could not checkpoint {}: {}. Leaving it in place rather than risking \
             a rename that strands its WAL.",
            old_path.display(),
            error
        );
        return;
    }

    if let Err(error) = std::fs::rename(&old_path, new_path) {
        warn!(
            "Could not rename {} to {}: {}",
            old_path.display(),
            new_path.display(),
            error
        );
        return;
    }

    // Empty after the truncating checkpoint, so removing them cannot lose a
    // write. Left behind they would be adopted by a *new* live.db later.
    for suffix in ["-wal", "-shm"] {
        let sidecar = dir.join(format!("live.db{suffix}"));
        if sidecar.exists() {
            let _ = std::fs::remove_file(sidecar);
        }
    }

    info!(
        "Renamed the journal from {} to {}",
        old_path.display(),
        new_path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The rename has to carry committed data across, including whatever was
    /// still in the WAL — that is the whole reason it checkpoints first.
    #[tokio::test]
    async fn a_legacy_journal_is_renamed_with_its_wal_contents_intact() {
        let dir = TempDir::new().unwrap();
        let old = dir.path().join("live.db");
        let new = dir.path().join("journal.db");

        // Written through a WAL-mode connection and deliberately *not*
        // checkpointed, so the row is in the WAL rather than the main file.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&old)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE carried (value TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO carried (value) VALUES ('survived')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        rename_legacy_journal(new.to_str().unwrap()).await;

        assert!(new.exists(), "the journal moved to the new name");
        assert!(!old.exists(), "and the old name is gone");
        assert!(
            !dir.path().join("live.db-wal").exists(),
            "the emptied sidecar is not left to be adopted by a later live.db"
        );

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&new))
            .await
            .unwrap();
        let value: String = sqlx::query_scalar("SELECT value FROM carried")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, "survived", "the WAL's contents came across");
    }

    /// Idempotent, because it runs on every startup.
    #[tokio::test]
    async fn an_existing_journal_is_never_overwritten_by_a_stale_live_db() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("live.db"), b"stale").unwrap();
        let new = dir.path().join("journal.db");
        std::fs::write(&new, b"current").unwrap();

        rename_legacy_journal(new.to_str().unwrap()).await;

        assert_eq!(std::fs::read(&new).unwrap(), b"current");
        assert!(
            dir.path().join("live.db").exists(),
            "the stale file is left alone rather than silently deleted"
        );
    }

    #[tokio::test]
    async fn a_fresh_install_with_no_legacy_file_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        let new = dir.path().join("journal.db");

        rename_legacy_journal(new.to_str().unwrap()).await;

        assert!(!new.exists(), "nothing is created before the store opens it");
    }
}
