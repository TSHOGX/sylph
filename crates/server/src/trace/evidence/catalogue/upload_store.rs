use super::EvidenceCatalogue;
use crate::trace::{
    evidence::{EvidenceError, ReceiptDisposition},
    upload::{AdmissionGuard, CaptureUploadTicket, PayloadDeclaration, Result, UploadError},
};
use sqlx::{Row, SqliteConnection};
use sylph_protocol::trace::{Capture, Issuer, PayloadRef};

impl EvidenceCatalogue {
    pub(crate) async fn begin_capture_upload(
        &self,
        issuer: &Issuer,
        capture: &Capture,
        declarations: &[PayloadDeclaration],
        max_open: usize,
    ) -> Result<CaptureUploadTicket> {
        let capture_json = serde_json::to_string(capture)?;
        let digest = blake3::hash(capture_json.as_bytes()).to_hex().to_string();
        let declarations_json = serde_json::to_string(declarations)?;
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(row)=sqlx::query("SELECT upload_id,capture_digest,declarations_json,state FROM trace_capture_uploads WHERE observation_ref=?").bind(capture.observation_ref.encode()).fetch_optional(&mut *tx).await?{
            if row.try_get::<String,_>("capture_digest")?!=digest || row.try_get::<String,_>("declarations_json")?!=declarations_json{return Err(UploadError::Collision);}
            if row.try_get::<String,_>("state")?=="cancelled"{return Err(UploadError::Unavailable);}
            return Ok(CaptureUploadTicket{id:row.try_get("upload_id")?,observation:capture.observation_ref.clone()});
        }
        // Capacity reservations are already bounded by max_open. Releasing
        // deleted epochs here cannot scan the unbounded historical upload table.
        let slots=sqlx::query("SELECT upload_id,ingestion_epoch FROM trace_capture_upload_slots ORDER BY upload_id LIMIT ?").bind((max_open+1) as i64).fetch_all(&mut *tx).await?;
        for slot in slots {
            let epoch: String = slot.try_get("ingestion_epoch")?;
            if super::super::epoch_tombstone(&mut tx, &epoch)
                .await?
                .is_some()
            {
                let id: String = slot.try_get("upload_id")?;
                sqlx::query("UPDATE trace_capture_uploads SET state='discarded' WHERE upload_id=? AND state='open'").bind(&id).execute(&mut *tx).await?;
                sqlx::query("DELETE FROM trace_capture_upload_slots WHERE upload_id=?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trace_capture_upload_slots")
            .fetch_one(&mut *tx)
            .await?;
        if count >= max_open as i64 {
            return Err(UploadError::Limit("open capture uploads"));
        }
        let epoch = capture.ingestion_authorization_epoch.as_str();
        let occurrence:Option<i64>=sqlx::query_scalar("SELECT occurrence_id FROM occurrences WHERE ingestion_epoch=? AND source_ref=? AND generation=? AND locator_json=?")
            .bind(epoch).bind(capture.source_position.source.encode()).bind(capture.source_position.generation as i64).bind(serde_json::to_string(&capture.source_position.locator)?).fetch_optional(&mut *tx).await?;
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO trace_capture_uploads VALUES(?,?,?,?,?,?,?,'open')")
            .bind(&id)
            .bind(capture.observation_ref.encode())
            .bind(serde_json::to_string(issuer)?)
            .bind(epoch)
            .bind(capture_json)
            .bind(digest)
            .bind(declarations_json)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO trace_capture_upload_slots VALUES(?,?)")
            .bind(&id)
            .bind(epoch)
            .execute(&mut *tx)
            .await?;
        for declaration in declarations {
            let reference = declaration.reference();
            super::reject_deleting(&mut tx, reference.digest()).await?;
            let (kind, length, manifest) = match declaration {
                PayloadDeclaration::Bytes { length, .. } => ("bytes", Some(*length as i64), None),
                PayloadDeclaration::Manifest { manifest, .. } => {
                    ("manifest", None, Some(serde_json::to_string(manifest)?))
                }
            };
            let reused = if let Some(occurrence) = occurrence {
                source_has_payload(&mut tx, occurrence, reference)
                    .await?
                    .then_some(occurrence)
            } else {
                None
            };
            sqlx::query("INSERT INTO trace_capture_payloads(upload_id,payload_ref,physical_digest,kind,expected_length,manifest_json,received,reused_occurrence) VALUES(?,?,?,?,?,?,?,?)")
                .bind(&id).bind(reference.as_str()).bind(reference.digest()).bind(kind).bind(length).bind(manifest).bind(reused.is_some()||kind=="manifest").bind(reused).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO trace_blob_inventory VALUES(?,0) ON CONFLICT(physical_digest) DO UPDATE SET collected=0").bind(reference.digest()).execute(&mut *tx).await?;
            super::gc_store::add_root(&mut tx, "capture", &id, epoch, reference.digest()).await?;
        }
        tx.commit().await?;
        Ok(CaptureUploadTicket {
            id,
            observation: capture.observation_ref.clone(),
        })
    }

    pub(crate) async fn capture_chunk_length(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
        reference: &PayloadRef,
    ) -> Result<u64> {
        let mut conn = self.evidence.readers.acquire().await?;
        authenticate(&mut conn, issuer, ticket, true).await?;
        let length:Option<i64>=sqlx::query_scalar("SELECT expected_length FROM trace_capture_payloads WHERE upload_id=? AND payload_ref=? AND kind='bytes'").bind(&ticket.id).bind(reference.as_str()).fetch_optional(&mut *conn).await?;
        u64::try_from(length.ok_or(UploadError::Unavailable)?)
            .map_err(|_| UploadError::Invalid("negative byte extent".into()))
    }

    pub(crate) async fn record_capture_chunk(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
        reference: &PayloadRef,
    ) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        authenticate(&mut tx, issuer, ticket, true).await?;
        let result=sqlx::query("UPDATE trace_capture_payloads SET received=1 WHERE upload_id=? AND payload_ref=? AND kind='bytes'").bind(&ticket.id).bind(reference.as_str()).execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(UploadError::Unavailable);
        }
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn capture_upload(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
    ) -> Result<(Capture, Vec<PayloadDeclaration>)> {
        let mut conn = self.evidence.readers.acquire().await?;
        let row = authenticate(&mut conn, issuer, ticket, false).await?;
        Ok((
            serde_json::from_str(&row.try_get::<String, _>("capture_json")?)?,
            serde_json::from_str(&row.try_get::<String, _>("declarations_json")?)?,
        ))
    }

    pub(crate) async fn capture_is_complete(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
    ) -> Result<()> {
        let mut conn = self.evidence.readers.acquire().await?;
        authenticate(&mut conn, issuer, ticket, false).await?;
        let missing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_capture_payloads WHERE upload_id=? AND kind='bytes' AND received=0)").bind(&ticket.id).fetch_one(&mut *conn).await?;
        if missing {
            return Err(UploadError::Incomplete);
        }
        Ok(())
    }

    pub(crate) async fn cancel_capture_upload(
        &self,
        issuer: &Issuer,
        ticket: &CaptureUploadTicket,
    ) -> Result<()> {
        let mut tx = self.evidence.writer.begin_with("BEGIN IMMEDIATE").await?;
        let row = authenticate(&mut tx, issuer, ticket, false).await?;
        sqlx::query(
            "UPDATE trace_capture_uploads SET state='cancelled' WHERE upload_id=? AND state='open'",
        )
        .bind(&ticket.id)
        .execute(&mut *tx)
        .await?;
        super::gc_store::remove_root(
            &mut tx,
            "capture",
            &ticket.id,
            &row.try_get::<String, _>("ingestion_epoch")?,
        )
        .await?;
        sqlx::query("DELETE FROM trace_capture_upload_slots WHERE upload_id=?")
            .bind(&ticket.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn recover_blob_writes(&self) -> super::Result<()> {
        sqlx::query("DELETE FROM trace_blob_writes")
            .execute(&self.evidence.writer)
            .await?;
        Ok(())
    }
}

async fn authenticate(
    conn: &mut SqliteConnection,
    issuer: &Issuer,
    ticket: &CaptureUploadTicket,
    require_open: bool,
) -> Result<sqlx::sqlite::SqliteRow> {
    let row = sqlx::query("SELECT * FROM trace_capture_uploads WHERE upload_id=?")
        .bind(&ticket.id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(UploadError::Unavailable)?;
    if row.try_get::<String, _>("issuer_json")? != serde_json::to_string(issuer)?
        || row.try_get::<String, _>("observation_ref")? != ticket.observation.encode()
    {
        return Err(UploadError::Authority);
    }
    if row.try_get::<String, _>("state")? == "cancelled"
        || (require_open && row.try_get::<String, _>("state")? != "open")
    {
        return Err(UploadError::Unavailable);
    }
    if require_open
        && super::super::epoch_tombstone(conn, &row.try_get::<String, _>("ingestion_epoch")?)
            .await?
            .is_some()
    {
        return Err(UploadError::Unavailable);
    }
    Ok(row)
}

async fn source_has_payload(
    conn: &mut SqliteConnection,
    occurrence: i64,
    payload: &PayloadRef,
) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM observations o JOIN observation_payload_roots r ON r.admission_seq=o.admission_seq JOIN payloads p ON p.payload_ref=r.payload_ref WHERE o.occurrence_id=? AND r.payload_ref=? AND p.availability='available') OR EXISTS(SELECT 1 FROM observations o JOIN observation_payload_roots r ON r.admission_seq=o.admission_seq JOIN payload_chunks c ON c.manifest_ref=r.payload_ref JOIN payloads p ON p.payload_ref=c.chunk_ref WHERE o.occurrence_id=? AND c.chunk_ref=? AND p.availability='available')")
        .bind(occurrence).bind(payload.as_str()).bind(occurrence).bind(payload.as_str()).fetch_one(conn).await?)
}

pub(crate) async fn verify_guard(
    conn: &mut SqliteConnection,
    guard: &AdmissionGuard,
    capture: &Capture,
    require_complete: bool,
) -> crate::trace::evidence::Result<()> {
    let row = sqlx::query(
        "SELECT capture_digest,observation_ref,state FROM trace_capture_uploads WHERE upload_id=?",
    )
    .bind(&guard.upload_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| EvidenceError::Invalid("missing capture upload ownership".into()))?;
    let digest = blake3::hash(&serde_json::to_vec(capture)?)
        .to_hex()
        .to_string();
    if row.try_get::<String, _>("capture_digest")? != digest
        || row.try_get::<String, _>("observation_ref")? != capture.observation_ref.encode()
        || row.try_get::<String, _>("state")? == "cancelled"
    {
        return Err(EvidenceError::Invalid(
            "capture upload ownership mismatch".into(),
        ));
    }
    if require_complete {
        let missing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trace_capture_payloads WHERE upload_id=? AND kind='bytes' AND received=0)").bind(&guard.upload_id).fetch_one(&mut *conn).await?;
        if missing {
            return Err(EvidenceError::Invalid(
                "capture lacks its own byte transfer receipts".into(),
            ));
        }
        let reused=sqlx::query("SELECT payload_ref,reused_occurrence FROM trace_capture_payloads WHERE upload_id=? AND reused_occurrence IS NOT NULL").bind(&guard.upload_id).fetch_all(&mut *conn).await?;
        for reused in reused {
            let occurrence: i64 = reused.try_get("reused_occurrence")?;
            let same:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM occurrences WHERE occurrence_id=? AND ingestion_epoch=? AND source_ref=? AND generation=? AND locator_json=?)")
                .bind(occurrence).bind(capture.ingestion_authorization_epoch.as_str()).bind(capture.source_position.source.encode()).bind(capture.source_position.generation as i64).bind(serde_json::to_string(&capture.source_position.locator)?).fetch_one(&mut *conn).await?;
            let reference = PayloadRef::parse(reused.try_get::<String, _>("payload_ref")?)
                .map_err(|e| EvidenceError::Invalid(e.to_string()))?;
            if !same
                || !source_has_payload(conn, occurrence, &reference)
                    .await
                    .map_err(|e| EvidenceError::Invalid(e.to_string()))?
            {
                return Err(EvidenceError::Invalid(
                    "source occurrence no longer proves retained payload ownership".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) async fn finish_guard(
    conn: &mut SqliteConnection,
    guard: &AdmissionGuard,
    capture: &Capture,
    disposition: &ReceiptDisposition,
    require_complete: bool,
) -> crate::trace::evidence::Result<()> {
    verify_guard(conn, guard, capture, require_complete).await?;
    let state = match disposition {
        ReceiptDisposition::Retained { .. } => "admitted",
        ReceiptDisposition::DiscardedByDeletion { .. } => "discarded",
    };
    sqlx::query("UPDATE trace_capture_uploads SET state=? WHERE upload_id=?")
        .bind(state)
        .bind(&guard.upload_id)
        .execute(&mut *conn)
        .await?;
    super::gc_store::remove_root(
        conn,
        "capture",
        &guard.upload_id,
        capture.ingestion_authorization_epoch.as_str(),
    )
    .await
    .map_err(|e| EvidenceError::Invalid(e.to_string()))?;
    sqlx::query("DELETE FROM trace_capture_upload_slots WHERE upload_id=?")
        .bind(&guard.upload_id)
        .execute(conn)
        .await?;
    Ok(())
}
