use super::{CatalogueError, EvidenceCatalogue, PinOwnerKind, Result};
use sqlx::{Row, SqliteConnection};
use sylph_protocol::trace::PayloadRef;

pub(crate) fn pin_id(kind: PinOwnerKind, owner: &str, payload: &PayloadRef) -> Result<String> {
    Ok(serde_json::to_string(&(kind, owner, payload))?)
}

pub(crate) async fn add_root(
    conn: &mut SqliteConnection,
    kind: &str,
    id: &str,
    epoch: &str,
    digest: &str,
) -> Result<()> {
    super::reject_deleting(conn, digest).await?;
    sqlx::query("INSERT INTO trace_physical_roots VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
        .bind(kind)
        .bind(id)
        .bind(epoch)
        .bind(digest)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO trace_blob_inventory VALUES(?,0) ON CONFLICT(physical_digest) DO UPDATE SET collected=0").bind(digest).execute(conn).await?;
    Ok(())
}

pub(crate) async fn add_payload_roots(
    conn: &mut SqliteConnection,
    kind: &str,
    id: &str,
    epoch: &str,
    payload: &PayloadRef,
) -> Result<()> {
    add_root(conn, kind, id, epoch, payload.digest()).await?;
    let chunks:Vec<String>=sqlx::query_scalar("SELECT DISTINCT p.physical_digest FROM payload_chunks c JOIN payloads p ON p.payload_ref=c.chunk_ref WHERE c.manifest_ref=?")
        .bind(payload.as_str()).fetch_all(&mut *conn).await?;
    for digest in chunks {
        add_root(conn, kind, id, epoch, &digest).await?;
    }
    Ok(())
}

pub(crate) async fn remove_root(
    conn: &mut SqliteConnection,
    kind: &str,
    id: &str,
    epoch: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM trace_physical_roots WHERE root_kind=? AND root_id=? AND ingestion_epoch=?",
    )
    .bind(kind)
    .bind(id)
    .bind(epoch)
    .execute(conn)
    .await?;
    Ok(())
}

pub(crate) async fn rooted(conn: &mut SqliteConnection, digest: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_physical_roots WHERE physical_digest=?) OR EXISTS(SELECT 1 FROM trace_blob_writes WHERE physical_digest=?)").bind(digest).bind(digest).fetch_one(conn).await?)
}

impl EvidenceCatalogue {
    pub(crate) async fn gc_acquire(&self) -> Result<String> {
        let token = uuid::Uuid::new_v4().to_string();
        let result = sqlx::query(
            "UPDATE trace_gc_guard SET guard_token=? WHERE singleton=1 AND guard_token IS NULL",
        )
        .bind(&token)
        .execute(&self.evidence.writer)
        .await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::Invalid(
                "another collector owns the deletion pass".into(),
            ));
        }
        Ok(token)
    }
    pub(crate) async fn gc_release(&self, token: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE trace_gc_guard SET guard_token=NULL WHERE singleton=1 AND guard_token=?",
        )
        .bind(token)
        .execute(&self.evidence.writer)
        .await?;
        if result.rows_affected() != 1 {
            return Err(CatalogueError::Invalid(
                "collector lost its pass ownership".into(),
            ));
        }
        Ok(())
    }
    pub(crate) async fn gc_recover(&self) -> Result<()> {
        sqlx::query("UPDATE trace_gc_guard SET guard_token=NULL WHERE singleton=1")
            .execute(&self.evidence.writer)
            .await?;
        Ok(())
    }
    /// Epoch cleanup is resumable and bounds both epochs inspected and roots
    /// removed. Tombstones already block reads before physical reclamation starts.
    pub(crate) async fn gc_release_deleted_roots(
        &self,
        max_epochs: usize,
        max_roots: usize,
    ) -> Result<usize> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let after: i64 =
            sqlx::query_scalar("SELECT epoch_order FROM trace_gc_cursor WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        let epochs=sqlx::query("SELECT epoch,epoch_order FROM ingestion_epochs WHERE epoch_order>? ORDER BY epoch_order LIMIT ?").bind(after).bind(max_epochs as i64).fetch_all(&mut *tx).await?;
        let mut released = 0usize;
        let mut cursor = after;
        for row in &epochs {
            let epoch: String = row.try_get("epoch")?;
            if super::super::epoch_tombstone(&mut tx, &epoch)
                .await
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?
                .is_some()
            {
                let result=sqlx::query("DELETE FROM trace_physical_roots WHERE (root_kind,root_id,ingestion_epoch,physical_digest) IN (SELECT root_kind,root_id,ingestion_epoch,physical_digest FROM trace_physical_roots WHERE ingestion_epoch=? ORDER BY root_kind,root_id,physical_digest LIMIT ?)")
                    .bind(&epoch).bind((max_roots-released) as i64).execute(&mut *tx).await?;
                released += result.rows_affected() as usize;
                let remain: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM trace_physical_roots WHERE ingestion_epoch=?)",
                )
                .bind(&epoch)
                .fetch_one(&mut *tx)
                .await?;
                if remain {
                    break;
                }
            }
            cursor = row.try_get("epoch_order")?;
            if released == max_roots {
                break;
            }
        }
        if epochs.len() < max_epochs && released < max_roots {
            cursor = 0;
        }
        sqlx::query("UPDATE trace_gc_cursor SET epoch_order=? WHERE singleton=1")
            .bind(cursor)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(released)
    }

    pub(crate) async fn gc_release_retired_pins(&self, now_ms: i64, limit: usize) -> Result<usize> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let rows=sqlx::query("SELECT p.owner_id,p.payload_ref,p.scope_epoch FROM trace_payload_pins p JOIN trace_generations g ON g.generation=p.owner_id WHERE p.owner_kind='generation' AND g.state IN ('retained','cancelled','retired') AND NOT EXISTS(SELECT 1 FROM trace_revision_pins r WHERE r.generation=g.generation) AND NOT EXISTS(SELECT 1 FROM trace_view_leases l WHERE l.generation=g.generation AND l.state IN ('pending','active') AND l.expires_at_ms>?) ORDER BY p.owner_id,p.payload_ref,p.scope_epoch LIMIT ?")
            .bind(now_ms).bind(limit as i64).fetch_all(&mut *tx).await?;
        for row in &rows {
            let owner: String = row.try_get("owner_id")?;
            let epoch: String = row.try_get("scope_epoch")?;
            let payload = PayloadRef::parse(row.try_get::<String, _>("payload_ref")?)
                .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
            remove_root(
                &mut tx,
                "pin",
                &pin_id(PinOwnerKind::Generation, &owner, &payload)?,
                &epoch,
            )
            .await?;
            sqlx::query("DELETE FROM trace_payload_pins WHERE owner_kind='generation' AND owner_id=? AND payload_ref=? AND scope_epoch=?").bind(owner).bind(payload.as_str()).bind(epoch).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(rows.len())
    }

    pub(crate) async fn gc_candidates(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT physical_digest FROM trace_blob_inventory WHERE collected=0 AND physical_digest>? ORDER BY physical_digest LIMIT ?").bind(after.unwrap_or("")).bind(limit as i64).fetch_all(&self.evidence.readers).await?)
    }

    pub(crate) async fn gc_interrupted(&self, limit: usize) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT physical_digest FROM trace_blob_gc WHERE state='deleting' ORDER BY physical_digest LIMIT ?").bind(limit as i64).fetch_all(&self.evidence.readers).await?)
    }

    pub(crate) async fn gc_unlink(&self, digest: &str) -> Result<u64> {
        let bytes = self
            .evidence
            .blobs
            .size(digest)
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?
            .unwrap_or(0);
        self.evidence
            .blobs
            .remove(digest)
            .await
            .map_err(|e| CatalogueError::Invalid(e.to_string()))?;
        Ok(bytes)
    }

    pub(crate) async fn gc_complete(&self, digest: &str) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let reserved:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_blob_gc WHERE physical_digest=? AND state='deleting')").bind(digest).fetch_one(&mut *tx).await?;
        if !reserved || rooted(&mut tx, digest).await? {
            return Err(CatalogueError::Invalid(
                "GC lost its exclusive deletion reservation".into(),
            ));
        }
        sqlx::query("UPDATE payloads SET availability='collected' WHERE physical_digest=?")
            .bind(digest)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE trace_blob_inventory SET collected=1 WHERE physical_digest=?")
            .bind(digest)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM trace_blob_gc WHERE physical_digest=?")
            .bind(digest)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
