//! Consistent SQLite acquisition. A SQL read transaction observes WAL commits and keeps
//! schema, storage classes, unknown columns and the selected rollout from the same cut.

use anyhow::{Context, Result};
use sqlx::{Connection, Row, TypeInfo, ValueRef};
use std::{collections::BTreeMap, path::Path};
use sylph_protocol::{
    NativeDatabaseColumn, NativeDatabaseSnapshot, NativeDatabaseStatus, NativeDatabaseValue,
};

const MAX_ROW_BYTES: u64 = 8 * 1024 * 1024;
const MAX_COLUMNS: usize = 256;

fn identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", metadata.dev(), metadata.ino())
}
fn snapshot(path: &Path,table:&str) -> NativeDatabaseSnapshot {
    NativeDatabaseSnapshot {
        database_locator: path.to_string_lossy().into_owned(),
        table: table.into(),
        columns: vec![],
        status: NativeDatabaseStatus::MissingDatabase,
        row: BTreeMap::new(),
        selected_source: None,
        selected_generation: None,
        selected_record_count: None,
        selected_pending_bytes: None,
    }
}

pub(super) async fn read(path: &Path, native_id: &str) -> Result<(NativeDatabaseSnapshot, String)> {
    read_table(path,"threads","id",native_id).await
}
pub(super) async fn read_goal(path:&Path,native_id:&str)->Result<(NativeDatabaseSnapshot,String)> {
    read_table(path,"thread_goals","thread_id",native_id).await
}
async fn read_table(path:&Path,table:&str,key:&str,native_id:&str)->Result<(NativeDatabaseSnapshot,String)> {
    let mut snapshot = snapshot(path,table);
    let before = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((snapshot, "missing".into()))
        }
        Err(error) => return Err(error.into()),
    };
    let file_identity = identity(&before);
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .busy_timeout(std::time::Duration::from_secs(2));
    let mut database = sqlx::SqliteConnection::connect_with(&options).await?;
    let mut tx = database.begin().await?;
    let schema = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(&mut *tx)
        .await?;
    anyhow::ensure!(
        !schema.is_empty() && schema.len() <= MAX_COLUMNS,
        "vendor threads schema is absent or exceeds the column bound"
    );
    for column in schema {
        snapshot.columns.push(NativeDatabaseColumn {
            name: column.try_get("name")?,
            declared_type: column.try_get("type")?,
        });
    }
    anyhow::ensure!(
        snapshot.columns.iter().any(|column| column.name == key)
            && snapshot
                .columns
                .iter()
                .any(|column| column.name == if table=="threads" {"rollout_path"} else {"goal_id"}),
        "vendor threads schema lacks id or rollout_path"
    );
    let quoted: Vec<_> = snapshot
        .columns
        .iter()
        .map(|column| format!("\"{}\"", column.name.replace('"', "\"\"")))
        .collect();
    let size_sql = format!(
        "SELECT {} FROM {table} WHERE {key}=? LIMIT 1",
        quoted
            .iter()
            .map(|column| format!("coalesce(length(CAST({column} AS BLOB)),0)"))
            .collect::<Vec<_>>()
            .join(" + ")
    );
    let size: Option<i64> = sqlx::query_scalar(&size_sql)
        .bind(native_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(size) = size else {
        snapshot.status = NativeDatabaseStatus::MissingRow;
        tx.commit().await?;
        return Ok((snapshot, file_identity));
    };
    anyhow::ensure!(
        size >= 0 && size as u64 <= MAX_ROW_BYTES,
        "vendor thread row exceeds the {MAX_ROW_BYTES}-byte capture bound"
    );
    let query = format!(
        "SELECT {} FROM {table} WHERE {key}=? LIMIT 1",
        quoted.join(",")
    );
    let row = sqlx::query(&query)
        .bind(native_id)
        .fetch_one(&mut *tx)
        .await?;
    for (index, column) in snapshot.columns.iter().enumerate() {
        let value = row.try_get_raw(index)?;
        let value = if value.is_null() {
            NativeDatabaseValue::Null
        } else {
            match value.type_info().name() {
                "INTEGER" => NativeDatabaseValue::Integer(row.try_get_unchecked(index)?),
                "REAL" => {
                    NativeDatabaseValue::RealBits(row.try_get_unchecked::<f64, _>(index)?.to_bits())
                }
                "TEXT" => {
                    let bytes: Vec<u8> = row.try_get_unchecked(index)?;
                    match String::from_utf8(bytes) {
                        Ok(value) => NativeDatabaseValue::Text(value),
                        Err(error) => NativeDatabaseValue::TextBytes(error.into_bytes()),
                    }
                }
                "BLOB" => NativeDatabaseValue::Blob(row.try_get_unchecked(index)?),
                kind => anyhow::bail!("unsupported SQLite storage class {kind}"),
            }
        };
        snapshot.row.insert(column.name.clone(), value);
    }
    tx.commit().await?;
    let current =
        std::fs::metadata(path).context("database disappeared during its read transaction")?;
    anyhow::ensure!(
        identity(&current) == file_identity,
        "database source was replaced during its read transaction"
    );
    snapshot.status = NativeDatabaseStatus::Available;
    Ok((snapshot, file_identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn database_capture_reads_committed_wal_and_preserves_every_column_storage_class() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state_5.sqlite");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let mut writer = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, vendor_blob BLOB, precise INTEGER, vendor_real REAL, vendor_text TEXT, absent TEXT)").execute(&mut writer).await.unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut writer)
            .await
            .unwrap();
        sqlx::query("INSERT INTO threads VALUES('native','/actual/selected.jsonl',X'00FF10',9223372036854775806,1.25,CAST(X'61FF62' AS TEXT),NULL)").execute(&mut writer).await.unwrap();
        assert!(
            std::fs::metadata(path.with_extension("sqlite-wal"))
                .unwrap()
                .len()
                > 0
        );
        let (captured, _) = read(&path, "native").await.unwrap();
        assert_eq!(captured.status, NativeDatabaseStatus::Available);
        assert_eq!(captured.row.len(), 7);
        assert_eq!(
            captured.row["rollout_path"],
            NativeDatabaseValue::Text("/actual/selected.jsonl".into())
        );
        assert_eq!(
            captured.row["vendor_blob"],
            NativeDatabaseValue::Blob(vec![0, 255, 16])
        );
        assert_eq!(
            captured.row["precise"],
            NativeDatabaseValue::Integer(9223372036854775806)
        );
        assert_eq!(
            captured.row["vendor_real"],
            NativeDatabaseValue::RealBits(1.25f64.to_bits())
        );
        assert_eq!(
            captured.row["vendor_text"],
            NativeDatabaseValue::TextBytes(vec![b'a', 255, b'b'])
        );
        assert_eq!(captured.row["absent"], NativeDatabaseValue::Null);
        assert_eq!(
            captured.columns[2],
            NativeDatabaseColumn {
                name: "vendor_blob".into(),
                declared_type: "BLOB".into()
            }
        );
        assert_eq!(
            read(&path, "absent").await.unwrap().0.status,
            NativeDatabaseStatus::MissingRow
        );
    }
    #[tokio::test]
    async fn database_capture_refuses_oversized_rows_before_materializing_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state_5.sqlite");
        let mut writer = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
        sqlx::query("CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT, large BLOB)")
            .execute(&mut writer)
            .await
            .unwrap();
        sqlx::query("INSERT INTO threads VALUES('native','/path',zeroblob(?))")
            .bind(MAX_ROW_BYTES as i64)
            .execute(&mut writer)
            .await
            .unwrap();
        assert!(read(&path, "native")
            .await
            .unwrap_err()
            .to_string()
            .contains("capture bound"));
    }
}
