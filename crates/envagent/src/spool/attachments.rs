//! Sidecar follow state is local acquisition progress, never a second trace record.
use super::{capture::CaptureAuthorization, Spool, SpoolError};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sylph_protocol::trace::ObservationRef;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentWatch {
    pub parent: ObservationRef,
    pub locator: String,
    pub parent_slot: String,
    pub relation: String,
    pub authorization: CaptureAuthorization,
}
impl Spool {
    pub async fn attachments_scanned(&self, parent: &ObservationRef) -> Result<bool, SpoolError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM capture_attachment_scans WHERE observation_key=?)",
        )
        .bind(parent.encode())
        .fetch_one(&self.readers)
        .await?)
    }
    pub async fn watch_attachments(
        &self,
        parent: &ObservationRef,
        watches: Vec<AttachmentWatch>,
    ) -> Result<(), SpoolError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for watch in watches {
            sqlx::query("INSERT INTO capture_attachment_watches(parent_key,locator,parent_slot,watch_json) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
                .bind(parent.encode()).bind(&watch.locator).bind(&watch.parent_slot).bind(serde_json::to_string(&watch).map_err(anyhow::Error::from)?).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO capture_attachment_scans VALUES(?) ON CONFLICT DO NOTHING")
            .bind(parent.encode())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn attachment_watches(
        &self,
        limit: usize,
    ) -> Result<Vec<(AttachmentWatch, Option<String>)>, SpoolError> {
        let rows=sqlx::query("SELECT watch_json,last_state FROM capture_attachment_watches ORDER BY last_checked,parent_key,locator LIMIT ?").bind(limit as i64).fetch_all(&self.readers).await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    serde_json::from_str(&row.try_get::<String, _>("watch_json")?)
                        .map_err(anyhow::Error::from)?,
                    row.try_get("last_state")?,
                ))
            })
            .collect()
    }
    pub async fn record_attachment_check(
        &self,
        watch: &AttachmentWatch,
        state: &str,
        now: i64,
    ) -> Result<(), SpoolError> {
        sqlx::query("UPDATE capture_attachment_watches SET last_state=?,last_checked=? WHERE parent_key=? AND locator=? AND parent_slot=?")
            .bind(state).bind(now).bind(watch.parent.encode()).bind(&watch.locator).bind(&watch.parent_slot).execute(&self.pool).await?;
        Ok(())
    }
}
