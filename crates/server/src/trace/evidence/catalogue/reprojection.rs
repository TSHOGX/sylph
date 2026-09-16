//! Durable product jobs are separate from the coordinator's private build tickets.

use super::{CatalogueError, EvidenceCatalogue, Result};
use crate::trace::generation::Snapshot;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReprojectionState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReprojectionJob {
    pub job_id: String,
    pub generation: String,
    pub state: ReprojectionState,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub projected_cut: u64,
    pub snapshot: Option<Snapshot>,
    pub error_code: Option<String>,
}

impl EvidenceCatalogue {
    pub async fn admit_reprojection(&self, subject: &str, now: i64) -> Result<ReprojectionJob> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row) = sqlx::query("SELECT * FROM trace_reprojection_jobs WHERE state IN ('queued','running','cancelling')")
            .fetch_optional(&mut *tx).await? {
            return decode(row);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let generation = format!("trace-{}", uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO trace_reprojection_jobs(job_id,generation,requested_by,state,created_at_ms) VALUES(?,?,?,'queued',?)")
            .bind(&id).bind(&generation).bind(subject).bind(now).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT * FROM trace_reprojection_jobs WHERE job_id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        decode(row)
    }

    pub async fn reprojection_job(&self, id: &str) -> Result<Option<ReprojectionJob>> {
        sqlx::query("SELECT * FROM trace_reprojection_jobs WHERE job_id=?")
            .bind(id)
            .fetch_optional(&self.evidence.readers)
            .await?
            .map(decode)
            .transpose()
    }

    pub async fn reprojection_jobs(&self) -> Result<Vec<ReprojectionJob>> {
        sqlx::query("SELECT * FROM trace_reprojection_jobs ORDER BY created_at_ms DESC,job_id DESC LIMIT 32")
            .fetch_all(&self.evidence.readers).await?.into_iter().map(decode).collect()
    }

    pub async fn claim_reprojection(&self, now: i64) -> Result<Option<ReprojectionJob>> {
        sqlx::query("UPDATE trace_reprojection_jobs SET state='running',started_at_ms=? WHERE state='queued' RETURNING *")
            .bind(now).fetch_optional(&self.evidence.writer).await?.map(decode).transpose()
    }

    pub async fn cancel_reprojection(&self, id: &str, now: i64) -> Result<Option<ReprojectionJob>> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE trace_reprojection_jobs SET state=CASE state WHEN 'queued' THEN 'cancelled' ELSE 'cancelling' END,finished_at_ms=CASE state WHEN 'queued' THEN ? ELSE NULL END WHERE job_id=? AND state IN ('queued','running')")
            .bind(now).bind(id).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT * FROM trace_reprojection_jobs WHERE job_id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        row.map(decode).transpose()
    }

    pub async fn reprojection_cancelled(&self, generation: &str) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_reprojection_jobs WHERE generation=? AND state IN ('cancelling','cancelled'))")
            .bind(generation).fetch_one(&self.evidence.readers).await?)
    }

    pub async fn reprojection_progress(&self, id: &str, cut: u64) -> Result<()> {
        sqlx::query("UPDATE trace_reprojection_jobs SET projected_cut=MAX(projected_cut,?) WHERE job_id=? AND state='running'")
            .bind(super::signed(cut)?).bind(id).execute(&self.evidence.writer).await?;
        Ok(())
    }

    pub async fn finish_reprojection_failure(&self, id: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE trace_reprojection_jobs SET state=CASE state WHEN 'cancelling' THEN 'cancelled' ELSE 'failed' END,error_code=CASE state WHEN 'cancelling' THEN NULL ELSE 'reprojection_failed' END,finished_at_ms=? WHERE job_id=? AND state IN ('running','cancelling')")
            .bind(now).bind(id).execute(&self.evidence.writer).await?;
        Ok(())
    }

    /// Exclusive startup owns recovery. Published completion is already atomic
    /// with the pointer and cannot be changed by interrupted-job cleanup.
    pub async fn recover_reprojection_jobs(&self, now: i64) -> Result<()> {
        sqlx::query("UPDATE trace_reprojection_jobs SET state=CASE state WHEN 'cancelling' THEN 'cancelled' ELSE 'failed' END,error_code=CASE state WHEN 'cancelling' THEN NULL ELSE 'server_restarted' END,finished_at_ms=? WHERE state IN ('queued','running','cancelling')")
            .bind(now).execute(&self.evidence.writer).await?;
        Ok(())
    }
}

pub(super) async fn check_cancellation(
    conn: &mut SqliteConnection,
    generation: &str,
) -> Result<()> {
    let cancelled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_reprojection_jobs WHERE generation=? AND state IN ('cancelling','cancelled'))")
        .bind(generation).fetch_one(conn).await?;
    if cancelled {
        return Err(CatalogueError::BuildCancelled);
    }
    Ok(())
}

pub(super) async fn complete(conn: &mut SqliteConnection, snapshot: &Snapshot) -> Result<()> {
    sqlx::query("UPDATE trace_reprojection_jobs SET state='completed',finished_at_ms=?,projected_cut=?,snapshot_json=?,error_code=NULL WHERE generation=? AND state='running'")
        .bind(crate::models::now_ms()).bind(super::signed(snapshot.evidence_cut)?).bind(serde_json::to_string(snapshot)?).bind(&snapshot.generation).execute(conn).await?;
    Ok(())
}

fn decode(row: sqlx::sqlite::SqliteRow) -> Result<ReprojectionJob> {
    let state: String = row.try_get("state")?;
    Ok(ReprojectionJob {
        job_id: row.try_get("job_id")?,
        generation: row.try_get("generation")?,
        state: serde_json::from_value(serde_json::Value::String(state))?,
        created_at_ms: row.try_get("created_at_ms")?,
        started_at_ms: row.try_get("started_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        projected_cut: super::unsigned(row.try_get("projected_cut")?)?,
        snapshot: row
            .try_get::<Option<String>, _>("snapshot_json")?
            .map(|json| serde_json::from_str(&json))
            .transpose()?,
        error_code: row.try_get("error_code")?,
    })
}
