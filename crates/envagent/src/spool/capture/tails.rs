//! Superseded staging is delivered under its original source and authorization.
use super::*;
use sylph_protocol::trace::{CaptureDiagnostic, CaptureDiagnosticCode};

fn diagnostics(unterminated: bool) -> Vec<CaptureDiagnostic> {
    let mut values = vec![CaptureDiagnostic {
        code: CaptureDiagnosticCode::SourceChanged,
        message:
            "Staged bytes retained after their source generation changed or became unavailable"
                .into(),
    }];
    if unterminated {
        values.push(CaptureDiagnostic {
            code: CaptureDiagnosticCode::IncompleteRecord,
            message: "The source generation ended before this record acquired a line terminator"
                .into(),
        });
    }
    values
}

pub(super) async fn schedule(
    tx: &mut Transaction<'_, Sqlite>,
    source: &str,
    generation: u64,
    reason: &str,
) -> Result<()> {
    sqlx::query("UPDATE capture_generations SET closed=1 WHERE source_ref=? AND generation=? AND file_identity IS NOT NULL")
        .bind(source).bind(signed(generation)?).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO capture_tail_drains(source_ref,generation,reason) SELECT ?,?,? WHERE EXISTS(SELECT 1 FROM capture_epoch_progress WHERE source_ref=? AND generation=? AND tail_bytes>0) ON CONFLICT DO NOTHING")
        .bind(source).bind(signed(generation)?).bind(reason).bind(source).bind(signed(generation)?)
        .execute(&mut **tx).await?;
    Ok(())
}

impl CaptureRegistry {
    pub(super) async fn close_generation(
        &self,
        source: &RegisteredSource,
        cursor: &Cursor,
        reason: &str,
    ) -> Result<()> {
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query(
            "UPDATE capture_sources SET revision=revision+1 WHERE source_ref=? AND revision=?",
        )
        .bind(source.reference.encode())
        .bind(cursor.revision)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(CaptureSpoolError::ReservationChanged);
        }
        schedule(
            &mut tx,
            &source.reference.encode(),
            cursor.generation,
            reason,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The reservation covers one worst-sized record from these bytes. Integer
    /// coordinates use their maximum encoded width, and both closure diagnostics
    /// are included. Queued drain records borrow this credit until their receipt;
    /// otherwise a spool full of staged bytes could never afford an envelope.
    pub(super) fn tail_reserve(
        &self,
        source: &RegisteredSource,
        authorization: &CaptureAuthorization,
        generation: u64,
        bytes: &[u8],
    ) -> Result<u64> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut item = self.prepare(
            authorization,
            SourcePosition {
                source: source.reference.clone(),
                generation,
                locator: SourceLocator::Jsonl {
                    byte_start: i64::MAX as u64 - bytes.len() as u64,
                    byte_end: i64::MAX as u64,
                    record_ordinal: i64::MAX as u64,
                },
            },
            bytes.to_vec(),
        )?;
        item.capture.observation_ref.producer_seq = i64::MAX as u64;
        item.capture.capture_diagnostics = diagnostics(true);
        let size = item_metadata_size(&item)?;
        if size > self.limits.max_metadata_bytes as u64 {
            return Err(invalid(
                "staged record metadata exceeds configured byte limit",
            ));
        }
        Ok(size)
    }

    pub(super) async fn insert_tail(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        source: &str,
        generation: u64,
        epoch: &IngestionEpoch,
        offset: u64,
        bytes: &[u8],
    ) -> Result<()> {
        for (index, bytes) in bytes.chunks(self.limits.chunk_bytes).enumerate() {
            sqlx::query("INSERT INTO capture_tail_chunks(source_ref,generation,ingestion_epoch,byte_start,bytes) VALUES(?,?,?,?,?)")
                .bind(source).bind(signed(generation)?).bind(epoch.as_str())
                .bind(signed(offset + (index * self.limits.chunk_bytes) as u64)?).bind(bytes)
                .execute(&mut **tx).await?;
        }
        Ok(())
    }

    pub(super) async fn load_tail(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        source: &RegisteredSource,
        generation: u64,
        epoch: &IngestionEpoch,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        if length > self.limits.max_record_bytes as u64 {
            return Err(CaptureSpoolError::RecordLimit(self.limits.max_record_bytes));
        }
        if length == 0 {
            return Ok(vec![]);
        }
        let rows = sqlx::query("SELECT byte_start,bytes FROM capture_tail_chunks WHERE source_ref=? AND generation=? AND ingestion_epoch=? ORDER BY byte_start LIMIT ?")
            .bind(source.reference.encode()).bind(signed(generation)?).bind(epoch.as_str())
            .bind(signed(self.limits.max_record_bytes.div_ceil(self.limits.chunk_bytes) as u64 + 1)?)
            .fetch_all(&mut **tx).await?;
        let mut bytes = Vec::new();
        for row in rows {
            let chunk: Vec<u8> = row.try_get("bytes")?;
            if unsigned(row.try_get("byte_start")?)? != offset + bytes.len() as u64
                || chunk.len() > self.limits.chunk_bytes
            {
                return Err(invalid("staged tail chunks are not contiguous and bounded"));
            }
            bytes.extend(chunk);
            if bytes.len() as u64 > length {
                return Err(invalid("staged tail exceeds its recorded length"));
            }
        }
        if bytes.len() as u64 != length {
            return Err(invalid("staged tail has missing bytes"));
        }
        Ok(bytes)
    }

    pub async fn closed_tail_frontier(&self) -> Result<u64> {
        let value: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(drain_id),0) FROM capture_tail_drains")
                .fetch_one(&self.spool.readers)
                .await?;
        unsigned(value)
    }

    pub async fn closed_tails_pending(&self, through: u64) -> Result<bool> {
        Ok(
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM capture_tail_drains WHERE drain_id<=?)",
            )
            .bind(signed(through)?)
            .fetch_one(&self.spool.readers)
            .await?,
        )
    }

    /// One job and at most max_records records per pass. Chunk reads, framing and
    /// hashing precede the writer; its transaction transfers staging ownership to
    /// the queue. No live path or current adoption is consulted during replay.
    pub async fn drain_closed_tails(&self, through: u64) -> Result<usize> {
        for _ in 0..4 {
            match self.drain_tail(through).await {
                Err(CaptureSpoolError::ReservationChanged) => continue,
                result => return result,
            }
        }
        Err(CaptureSpoolError::ReservationChanged)
    }

    async fn drain_tail(&self, through: u64) -> Result<usize> {
        let mut read = self.spool.readers.begin().await?;
        let job: Option<(String, i64)> = sqlx::query_as("SELECT source_ref,generation FROM capture_tail_drains WHERE drain_id<=? ORDER BY drain_id LIMIT 1")
            .bind(signed(through)?).fetch_optional(&mut *read).await?;
        let Some((source, generation)) = job else {
            return Ok(0);
        };
        let row = sqlx::query("SELECT p.* FROM capture_epoch_progress p WHERE p.source_ref=? AND p.generation=? AND p.tail_bytes>0 ORDER BY p.ingestion_epoch LIMIT 1")
            .bind(&source).bind(generation).fetch_optional(&mut *read).await?;
        let Some(row) = row else {
            read.commit().await?;
            sqlx::query("DELETE FROM capture_tail_drains WHERE source_ref=? AND generation=? AND NOT EXISTS(SELECT 1 FROM capture_epoch_progress WHERE source_ref=? AND generation=? AND tail_bytes>0)")
                .bind(&source).bind(generation).bind(&source).bind(generation).execute(&self.spool.pool).await?;
            return Ok(0);
        };
        let authorization: CaptureAuthorization =
            serde_json::from_str(row.try_get("authorization_json")?)?;
        let epoch: String = row.try_get("ingestion_epoch")?;
        if authorization.epoch.as_str() != epoch {
            return Err(invalid(
                "staged authorization epoch differs from its progress",
            ));
        }
        let offset = unsigned(row.try_get("complete_offset")?)?;
        let ordinal = unsigned(row.try_get("next_ordinal")?)?;
        let length = unsigned(row.try_get("tail_bytes")?)?;
        let available_credit = unsigned(
            row.try_get::<i64, _>("tail_reserve")? - row.try_get::<i64, _>("tail_credit")?,
        )?;
        if available_credit == 0 {
            return Ok(0);
        }
        let mut bytes = Vec::new();
        let mut ends = Vec::new();
        while (bytes.len() as u64) < length {
            let chunk = sqlx::query("SELECT byte_start,bytes FROM capture_tail_chunks WHERE source_ref=? AND generation=? AND ingestion_epoch=? AND byte_start>=? ORDER BY byte_start LIMIT 1")
                .bind(&source).bind(generation).bind(&epoch).bind(signed(offset + bytes.len() as u64)?)
                .fetch_optional(&mut *read).await?.ok_or_else(|| invalid("staged tail has a missing chunk"))?;
            if unsigned(chunk.try_get("byte_start")?)? != offset + bytes.len() as u64 {
                return Err(invalid("staged tail has a gap"));
            }
            let value: Vec<u8> = chunk.try_get("bytes")?;
            if value.is_empty()
                || value.len() > self.limits.chunk_bytes
                || bytes.len() + value.len() > self.limits.max_record_bytes
            {
                return Err(invalid("staged tail chunk exceeds its bounds"));
            }
            let base = bytes.len();
            ends.extend(
                value
                    .iter()
                    .enumerate()
                    .filter(|(_, byte)| **byte == b'\n')
                    .map(|(index, _)| base + index + 1),
            );
            bytes.extend(value);
            if bytes.len() as u64 > length {
                return Err(invalid("staged tail exceeds its recorded length"));
            }
            if ends.len() >= self.limits.max_records
                || (bytes.len() >= self.limits.read_bytes && !ends.is_empty())
            {
                break;
            }
        }
        if bytes.len() as u64 == length && !bytes.ends_with(b"\n") {
            ends.push(bytes.len());
        }
        read.commit().await?;
        let source_ref = SourceRef::decode(&source).map_err(invalid)?;
        let mut prepared = Vec::new();
        let mut used_credit = 0;
        let mut consumed = 0;
        for end in ends.into_iter().take(self.limits.max_records) {
            if !prepared.is_empty() && end > self.limits.read_bytes {
                break;
            }
            let mut item = self.prepare(
                &authorization,
                SourcePosition {
                    source: source_ref.clone(),
                    generation: unsigned(generation)?,
                    locator: SourceLocator::Jsonl {
                        byte_start: offset + consumed as u64,
                        byte_end: offset + end as u64,
                        record_ordinal: ordinal + prepared.len() as u64,
                    },
                },
                bytes[consumed..end].to_vec(),
            )?;
            item.capture.capture_diagnostics = diagnostics(bytes[end - 1] != b'\n');
            // Actual producer sequence width is assigned below. Charge its maximum
            // here, so a long-lived producer cannot overdraw staging credit.
            item.capture.observation_ref.producer_seq = i64::MAX as u64;
            let credit = item_metadata_size(&item)?;
            if used_credit + credit > available_credit {
                break;
            }
            used_credit += credit;
            prepared.push(item);
            consumed = end;
            if consumed >= self.limits.read_bytes {
                break;
            }
        }
        if prepared.is_empty() {
            if row.try_get::<i64, _>("tail_credit")? == 0 {
                return Err(invalid(
                    "staged metadata reservation cannot encode its next record",
                ));
            }
            return Ok(0);
        }
        let mut tx = self.spool.pool.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query("UPDATE capture_epoch_progress SET revision=revision+1 WHERE source_ref=? AND generation=? AND ingestion_epoch=? AND revision=?")
            .bind(&source).bind(generation).bind(&epoch).bind(row.try_get::<i64,_>("revision")?).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(CaptureSpoolError::ReservationChanged);
        }
        let issuer = serde_json::to_string(&self.issuer)?;
        for item in &mut prepared {
            let sequence: i64 = sqlx::query_scalar("UPDATE capture_producers SET next_seq=next_seq+1 WHERE issuer_json=? AND producer_id=? AND producer_epoch=? AND next_seq<9223372036854775807 RETURNING next_seq-1")
                .bind(&issuer).bind(&self.producer_id).bind(&self.producer_epoch).fetch_one(&mut *tx).await?;
            item.capture.observation_ref.producer_seq = unsigned(sequence)?;
            self.insert_credited(&mut tx, item, item_metadata_size(item)?)
                .await?;
        }
        let end = offset + consumed as u64;
        // Only one boundary chunk can straddle the released prefix. Every BLOB
        // fetched or rewritten here is bounded by chunk_bytes, never by tail size.
        let boundary = sqlx::query("SELECT byte_start,bytes FROM capture_tail_chunks WHERE source_ref=? AND generation=? AND ingestion_epoch=? AND byte_start<? ORDER BY byte_start DESC LIMIT 1")
            .bind(&source).bind(generation).bind(&epoch).bind(signed(end)?).fetch_one(&mut *tx).await?;
        let start = unsigned(boundary.try_get("byte_start")?)?;
        let remainder: Vec<u8> = boundary.try_get("bytes")?;
        sqlx::query("DELETE FROM capture_tail_chunks WHERE source_ref=? AND generation=? AND ingestion_epoch=? AND byte_start<?")
            .bind(&source).bind(generation).bind(&epoch).bind(signed(end)?).execute(&mut *tx).await?;
        let released = usize::try_from(end - start).map_err(invalid)?;
        if released < remainder.len() {
            self.insert_tail(
                &mut tx,
                &source,
                unsigned(generation)?,
                &authorization.epoch,
                end,
                &remainder[released..],
            )
            .await?;
        }
        sqlx::query("UPDATE capture_epoch_progress SET complete_offset=?,next_ordinal=? WHERE source_ref=? AND generation=? AND ingestion_epoch=?")
            .bind(signed(end)?).bind(signed(ordinal + prepared.len() as u64)?).bind(&source).bind(generation).bind(&epoch).execute(&mut *tx).await?;
        if let Some(identity) = row.try_get::<Option<i64>, _>("identity_record_ordinal")? {
            if let Some(item) = prepared.iter().find(|item| matches!(item.capture.source_position.locator, SourceLocator::Jsonl { record_ordinal, .. } if record_ordinal == identity as u64)) {
                sqlx::query("INSERT INTO capture_source_proofs(source_ref,generation,ingestion_epoch,observation_json) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
                    .bind(&source).bind(generation).bind(&epoch).bind(serde_json::to_string(&item.capture.observation_ref)?).execute(&mut *tx).await?;
            }
        }
        #[cfg(test)]
        self.crash_at("before_tail_commit");
        tx.commit().await?;
        #[cfg(test)]
        self.crash_at("after_tail_commit");
        Ok(prepared.len())
    }
}
