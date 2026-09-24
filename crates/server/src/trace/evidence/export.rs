//! Bounded, environment-scoped raw admission reads. No semantic projection filter.
use super::*;
use crate::trace::evidence::catalogue::PinOwnerKind;
use sqlx::Connection;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use sylph_protocol::EnvironmentId;

pub(crate) const PAGE_QUERY_WORK: u64 = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportCapture {
    pub admission_seq: u64,
    pub admitted_at_ms: i64,
    pub occurrence_id: u64,
    pub canonical_observation_ref: ObservationRef,
    pub scope: IngestionScope,
    pub capture: Capture,
}
pub(crate) struct ExportPage {
    pub captures: Vec<ExportCapture>,
    pub after: u64,
    pub more: bool,
    pub candidates: usize,
    #[cfg(test)]
    pub vm_steps: u64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportTicket {
    pub id: String,
    pub environment: EnvironmentId,
    pub evidence_cut: u64,
    pub visibility_revision: u64,
    pub expires_at_ms: i64,
}

impl EvidenceStore {
    pub(crate) async fn export_page(
        &self,
        environment: &EnvironmentId,
        after: u64,
        cut: u64,
        limit: usize,
    ) -> Result<ExportPage> {
        if limit == 0 || limit > self.limits.max_read_items {
            return Err(EvidenceError::Limit("export admission page"));
        }
        let mut connection = self.readers.acquire().await?;
        let steps = Arc::new(AtomicU64::new(0));
        let counter = steps.clone();
        connection
            .lock_handle()
            .await?
            .set_progress_handler(1, move || {
                counter.fetch_add(1, Ordering::Relaxed) < PAGE_QUERY_WORK
            });
        let mut tx = connection.begin().await?;
        let rows=sqlx::query("SELECT o.admission_seq,o.admitted_at_ms,o.occurrence_id,o.envelope_bytes,c.canonical_observation_ref,s.scope_json,d.tombstone_id FROM environment_admissions a JOIN observations o ON o.admission_seq=a.admission_seq JOIN occurrences c ON c.occurrence_id=o.occurrence_id JOIN ingestion_epochs e ON e.epoch=c.ingestion_epoch JOIN ingestion_scopes s ON s.scope_ref=e.scope_ref LEFT JOIN ingestion_epoch_tombstones d ON d.epoch=e.epoch WHERE a.environment_ref=? AND a.admission_seq>? AND a.admission_seq<=? ORDER BY a.admission_seq LIMIT ?")
            .bind(environment.as_str()).bind(signed(after)?).bind(signed(cut)?).bind((limit+1) as i64).fetch_all(&mut *tx).await?;
        let mut result = ExportPage {
            captures: vec![],
            after,
            more: rows.len() > limit,
            candidates: 0,
            #[cfg(test)]
            vm_steps: 0,
        };
        let mut bytes = 0;
        for row in rows.into_iter().take(limit) {
            let sequence = unsigned(row.try_get("admission_seq")?)?;
            if row.try_get::<Option<String>, _>("tombstone_id")?.is_none() {
                let size = unsigned(row.try_get("envelope_bytes")?)? as usize;
                if bytes + size > self.limits.max_read_bytes {
                    if result.after == after {
                        return Err(EvidenceError::Limit("export envelope bytes"));
                    }
                    result.more = true;
                    break;
                }
                let envelope: String = sqlx::query_scalar(
                    "SELECT envelope_json FROM observations WHERE admission_seq=?",
                )
                .bind(signed(sequence)?)
                .fetch_one(&mut *tx)
                .await?;
                bytes += size;
                result.captures.push(ExportCapture {
                    admission_seq: sequence,
                    admitted_at_ms: row.try_get("admitted_at_ms")?,
                    occurrence_id: unsigned(row.try_get("occurrence_id")?)?,
                    canonical_observation_ref: ObservationRef::decode(
                        &row.try_get::<String, _>("canonical_observation_ref")?,
                    )
                    .map_err(invalid)?,
                    scope: serde_json::from_str(&row.try_get::<String, _>("scope_json")?)?,
                    capture: serde_json::from_str(&envelope)?,
                });
            }
            result.after = sequence;
            result.candidates += 1;
        }
        tx.commit().await?;
        connection.lock_handle().await?.remove_progress_handler();
        #[cfg(test)]
        {
            result.vm_steps = steps.load(Ordering::Relaxed);
        }
        Ok(result)
    }

    pub(crate) async fn begin_export(
        &self,
        environment: EnvironmentId,
        cut: u64,
        lifetime_ms: i64,
    ) -> Result<ExportTicket> {
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        let latest: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(admission_seq),0) FROM observations")
                .fetch_one(&mut *tx)
                .await?;
        if cut > unsigned(latest)? || lifetime_ms <= 0 {
            return Err(EvidenceError::Invalid(
                "Invalid export cut or lifetime".into(),
            ));
        }
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM trace_environment_visibility WHERE environment_ref=?",
        )
        .bind(environment.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let ticket = ExportTicket {
            id: uuid::Uuid::new_v4().to_string(),
            environment,
            evidence_cut: cut,
            visibility_revision: unsigned(revision.unwrap_or(0))?,
            expires_at_ms: now_ms() + lifetime_ms,
        };
        sqlx::query("INSERT INTO trace_evidence_exports VALUES(?,?,?,?,?)")
            .bind(&ticket.id)
            .bind(ticket.environment.as_str())
            .bind(signed(cut)?)
            .bind(signed(ticket.visibility_revision)?)
            .bind(ticket.expires_at_ms)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ticket)
    }

    pub(crate) async fn export_is_current(&self, ticket: &ExportTicket) -> Result<bool> {
        let mut connection = self.readers.acquire().await?;
        export_is_current(&mut connection, ticket).await
    }
    pub(crate) async fn finish_export(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM trace_evidence_exports WHERE export_id=?")
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }
}

/// Shared by output authorization and transactional pin publication.
pub(super) async fn export_is_current(
    connection: &mut sqlx::SqliteConnection,
    ticket: &ExportTicket,
) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_evidence_exports e LEFT JOIN trace_environment_visibility v ON v.environment_ref=e.environment_ref WHERE e.export_id=? AND e.environment_ref=? AND e.evidence_cut=? AND e.visibility_revision=? AND e.expires_at_ms=? AND e.expires_at_ms>? AND e.visibility_revision=COALESCE(v.revision,0))")
        .bind(&ticket.id).bind(ticket.environment.as_str()).bind(signed(ticket.evidence_cut)?)
        .bind(signed(ticket.visibility_revision)?).bind(ticket.expires_at_ms).bind(now_ms())
        .fetch_one(connection).await?)
}

impl catalogue::EvidenceCatalogue {
    /// Retirement is bounded and idempotent; process restart abandons every old job.
    pub(crate) async fn retire_exports(
        &self,
        restart: bool,
        limit: usize,
    ) -> catalogue::Result<usize> {
        let jobs:Vec<String>=sqlx::query_scalar("SELECT export_id FROM trace_evidence_exports WHERE expires_at_ms<=? ORDER BY expires_at_ms,export_id LIMIT ?")
            .bind(if restart {i64::MAX}else{now_ms()}).bind(limit as i64).fetch_all(&self.evidence().readers).await?;
        let mut retired = 0;
        for id in jobs {
            let removed = self
                .release_owner_pins(PinOwnerKind::Export, &id, self.limits().max_pin_batch)
                .await?;
            if removed < self.limits().max_pin_batch {
                self.evidence()
                    .finish_export(&id)
                    .await
                    .map_err(|error| catalogue::CatalogueError::Invalid(error.to_string()))?;
                retired += 1;
            }
        }
        Ok(retired)
    }
}
