use super::*;
use crate::trace::cursor::{CursorBinding, CursorCodec, CursorError};
use sqlx::Connection;

async fn open(directory: &tempfile::TempDir) -> Result<EvidenceStore> {
    EvidenceStore::open(
        directory.path().join("evidence.db"),
        BlobStore::new(directory.path().join("blobs")),
        AdmissionLimits::default(),
    )
    .await
}

fn binding() -> CursorBinding<'static> {
    CursorBinding {
        domain: "trace/reader/forward",
        subject: "subject",
        lease: "retained-lease",
        query_digest: "query-and-grants",
    }
}

#[tokio::test]
async fn cursor_key_survives_archive_restart_and_is_private_to_that_archive() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory).await.unwrap();
    let codec = CursorCodec::new(&store.cursor_key().await.unwrap(), 128).unwrap();
    let cursor = codec.seal("hidden-pagination-position", binding()).unwrap();
    store.close().await;

    let reopened = open(&directory).await.unwrap();
    let codec = CursorCodec::new(&reopened.cursor_key().await.unwrap(), 128).unwrap();
    assert_eq!(
        codec.open(&cursor, binding()).unwrap(),
        "hidden-pagination-position"
    );
    let second_directory = tempfile::tempdir().unwrap();
    let other = open(&second_directory).await.unwrap();
    let other_codec = CursorCodec::new(&other.cursor_key().await.unwrap(), 128).unwrap();
    assert_eq!(
        other_codec.open(&cursor, binding()),
        Err(CursorError::InvalidCursor)
    );
    reopened.close().await;
    other.close().await;
}

#[tokio::test]
async fn missing_or_malformed_existing_cursor_keys_fail_without_rotation() {
    for (corruption, expected) in [
        ("DELETE FROM trace_cursor_secret", None),
        (
            "UPDATE trace_cursor_secret SET key_bytes=zeroblob(31)",
            Some(("blob", 31i64)),
        ),
        (
            "UPDATE trace_cursor_secret SET key_bytes=zeroblob(33)",
            Some(("blob", 33i64)),
        ),
        (
            "UPDATE trace_cursor_secret SET key_bytes='01234567890123456789012345678901'",
            Some(("text", 32i64)),
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store = open(&directory).await.unwrap();
        {
            let mut connection = store.writer.acquire().await.unwrap();
            sqlx::query("PRAGMA ignore_check_constraints=ON")
                .execute(&mut *connection)
                .await
                .unwrap();
            sqlx::query(corruption)
                .execute(&mut *connection)
                .await
                .unwrap();
        }
        assert!(matches!(
            store.cursor_key().await,
            Err(EvidenceError::Invalid(_))
        ));
        store.close().await;
        assert!(matches!(
            open(&directory).await,
            Err(EvidenceError::Invalid(_))
        ));
        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(directory.path().join("evidence.db"))
                .read_only(true),
        )
        .await
        .unwrap();
        let retained: Option<(String, i64)> = sqlx::query_as(
            "SELECT typeof(key_bytes),length(key_bytes) FROM trace_cursor_secret WHERE singleton=1",
        )
        .fetch_optional(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            retained
                .as_ref()
                .map(|(kind, length)| (kind.as_str(), *length)),
            expected
        );
        connection.close().await.unwrap();
    }
}

#[tokio::test]
async fn cursor_secret_schema_enforces_one_exact_blob_key() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory).await.unwrap();
    let mut connection = store.writer.acquire().await.unwrap();
    for mutation in [
        "INSERT INTO trace_cursor_secret(singleton,key_bytes) VALUES(2,zeroblob(32))",
        "UPDATE trace_cursor_secret SET key_bytes=zeroblob(31)",
        "UPDATE trace_cursor_secret SET key_bytes='01234567890123456789012345678901'",
    ] {
        assert!(sqlx::query(mutation)
            .execute(&mut *connection)
            .await
            .is_err());
    }
    drop(connection);
    assert!(store.cursor_key().await.is_ok());
    store.close().await;
}
