//! Retrying a wire frame never repeats an at-most-once vendor side effect.

use super::{Spool, SpoolError};
use sqlx::Row;
use sylph_protocol::{CommandOutcome, CommandRequestId, EnvironmentCommand, IdempotencyPolicy};

impl Spool {
    /// The caller serializes concurrent attempts of this request id. A start is committed
    /// before the first vendor operation; a crash after it leaves an explicit ambiguity.
    pub async fn begin_command(
        &self,
        request_id: &CommandRequestId,
        policy: IdempotencyPolicy,
        command: &EnvironmentCommand,
    ) -> Result<Option<CommandOutcome>, SpoolError> {
        let encoded = serde_json::to_vec(&(policy, command)).map_err(anyhow::Error::from)?;
        let fingerprint = blake3::hash(&encoded).to_hex().to_string();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let inserted=sqlx::query("INSERT INTO command_attempts(request_id,fingerprint) VALUES(?,?) ON CONFLICT DO NOTHING")
            .bind(request_id.as_str()).bind(&fingerprint).execute(&mut *tx).await?.rows_affected()==1;
        let row =
            sqlx::query("SELECT fingerprint,outcome_json FROM command_attempts WHERE request_id=?")
                .bind(request_id.as_str())
                .fetch_one(&mut *tx)
                .await?;
        let previous: String = row.try_get("fingerprint")?;
        let outcome: Option<String> = row.try_get("outcome_json")?;
        tx.commit().await?;
        if previous != fingerprint {
            return Ok(Some(CommandOutcome::Rejected {
                code: "COMMAND_ID_REUSED".into(),
                message: "command identity already names a different immutable request".into(),
            }));
        }
        if let Some(outcome) = outcome {
            return Ok(Some(
                serde_json::from_str(&outcome).map_err(anyhow::Error::from)?,
            ));
        }
        if !inserted && policy == IdempotencyPolicy::AtMostOnce {
            return Ok(Some(CommandOutcome::Unknown {
                message: Some(
                    "an earlier attempt may have reached the vendor; this command was not repeated"
                        .into(),
                ),
            }));
        }
        Ok(None)
    }

    pub async fn finish_command(
        &self,
        request_id: &CommandRequestId,
        outcome: &CommandOutcome,
    ) -> Result<(), SpoolError> {
        sqlx::query("UPDATE command_attempts SET outcome_json=? WHERE request_id=? AND outcome_json IS NULL")
            .bind(serde_json::to_string(outcome).map_err(anyhow::Error::from)?).bind(request_id.as_str())
            .execute(&self.pool).await?;
        Ok(())
    }
}
