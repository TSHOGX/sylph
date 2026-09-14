//! Pool construction shared by the stores that must isolate reads from writes.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

pub(crate) async fn pool(
    options: SqliteConnectOptions,
    connections: u32,
    read_only: bool,
    tokenize: bool,
    native_path: Option<String>,
) -> Result<SqlitePool, sqlx::Error> {
    SqlitePoolOptions::new()
        .max_connections(connections)
        .after_connect(move |conn, _| {
            let native_path = native_path.clone();
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
                if let Some(path) = native_path {
                    sqlx::query("ATTACH DATABASE ? AS native")
                        .bind(path)
                        .execute(&mut *conn)
                        .await?;
                }
                if read_only {
                    // Also reject writes to attached databases. Pool ownership is a
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
