//! Pool construction shared by the stores that must isolate reads from writes.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Connection;

pub(crate) async fn reusable_after_release(
    connection: &mut sqlx::SqliteConnection,
) -> Result<bool, sqlx::Error> {
    // SQLx 0.8 acknowledges custom BEGIN before its asynchronous handle check
    // creates Transaction. Cancellation in that await leaves no rollback guard.
    // Drain normal queued rollbacks before inspecting SQLx's transaction depth;
    // discard any still-open connection instead of guessing at a depth reset.
    connection.ping().await?;
    Ok(!connection.is_in_transaction())
}

pub(crate) async fn pool(
    options: SqliteConnectOptions,
    connections: u32,
    read_only: bool,
    tokenize: bool,
) -> Result<SqlitePool, sqlx::Error> {
    SqlitePoolOptions::new()
        .max_connections(connections)
        .after_release(|connection, _| Box::pin(reusable_after_release(connection)))
        .after_connect(move |conn, _| {
            Box::pin(async move {
                if tokenize {
                    let mut handle = conn.lock_handle().await?;
                    // SAFETY: the handle is live and exclusively held for registration.
                    unsafe {
                        fts5jieba::register_on_connection(handle.as_raw_handle().as_ptr().cast())
                    }
                    .map_err(|code| {
                        sqlx::Error::Configuration(
                            format!("Could not register jieba tokenizer (SQLite code {code})")
                                .into(),
                        )
                    })?;
                }
                if read_only {
                    // Reject writes from every read connection. Pool ownership is a
                    // checked boundary, not a convention about which caller uses it.
                    sqlx::query("PRAGMA query_only = ON")
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(())
            })
        })
        .connect_with(options.read_only(read_only))
        .await
}

#[cfg(test)]
pub(crate) async fn assert_abandoned_begin_is_discarded(pool: &SqlitePool) {
    use sqlx::{sqlite::SqliteTransactionManager, TransactionManager};
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("CREATE TABLE cancelled_begin_probe(value TEXT)")
        .execute(&mut *connection)
        .await
        .unwrap();
    // Enter the same acknowledged-BEGIN state reproduced by the deterministic
    // evidence cancellation test, without constructing a Transaction guard.
    SqliteTransactionManager::begin(&mut connection, Some("BEGIN IMMEDIATE".into()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO cancelled_begin_probe VALUES('uncommitted')")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let mut connection = pool.acquire().await.unwrap();
    assert!(!connection.is_in_transaction());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cancelled_begin_probe")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "discarding a cancelled connection must roll back its writes"
    );
    connection
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();
}
