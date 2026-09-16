//! Environment credentials, connection leases and durable command transport outcomes.
use super::ControlStore;
use crate::{
    error::{Error, Result},
    models::now_ms,
};
use sqlx::Row;
use sylph_protocol::{CommandOutcome, EnvironmentId, IdempotencyPolicy};

#[derive(Debug, Clone)]
pub struct EnvironmentRow {
    pub id: EnvironmentId,
    pub installation_id: String,
    pub name: String,
    pub origin: String,
    pub token_hash: String,
    pub connected: bool,
    pub connection_epoch: u64,
    pub last_seen_at_ms: Option<i64>,
    pub reachable_via: Option<String>,
    pub agents_json: String,
    pub facts_json: String,
}

impl ControlStore {
    pub async fn update_environment_facts(
        &self,
        id: &EnvironmentId,
        facts_json: &str,
    ) -> Result<bool> {
        let result =
            sqlx::query("UPDATE env_installations SET facts_json = ? WHERE environment_id = ?")
                .bind(facts_json)
                .bind(id.as_str())
                .execute(self.raw_pool())
                .await
                .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn disconnect_stale_environments(
        &self,
        cutoff_ms: i64,
    ) -> Result<Vec<EnvironmentId>> {
        // Read the epoch together with the candidate id, then compare it in
        // the update.  A reconnect may win between these statements; without
        // the compare-and-set the reaper would mark the newer lease offline.
        let rows = sqlx::query(
            "SELECT environment_id, connection_epoch FROM env_installations \
             WHERE connected = 1 AND (last_seen_at_ms IS NULL OR last_seen_at_ms < ?)",
        )
        .bind(cutoff_ms)
        .fetch_all(self.read_pool())
        .await
        .map_err(Error::Database)?;
        let mut disconnected = Vec::new();
        for row in rows {
            let raw: String = row.try_get("environment_id").map_err(Error::Database)?;
            let epoch: i64 = row.try_get("connection_epoch").map_err(Error::Database)?;
            let result = sqlx::query(
                "UPDATE env_installations SET connected = 0 \
                 WHERE environment_id = ? AND connected = 1 \
                   AND connection_epoch = ? \
                   AND (last_seen_at_ms IS NULL OR last_seen_at_ms < ?)",
            )
            .bind(&raw)
            .bind(epoch)
            .bind(cutoff_ms)
            .execute(self.raw_pool())
            .await
            .map_err(Error::Database)?;
            if result.rows_affected() == 1 {
                if let Ok(id) = EnvironmentId::parse(raw) {
                    disconnected.push(id);
                }
            }
        }
        Ok(disconnected)
    }

    pub async fn delete_environment(&self, id: &EnvironmentId) -> Result<bool> {
        let result = sqlx::query("DELETE FROM env_installations WHERE environment_id = ?")
            .bind(id.as_str())
            .execute(self.raw_pool())
            .await
            .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn put_pairing(&self, code_hash: &str, expires_at_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO env_pairings(code_hash, expires_at_ms, used_at_ms) VALUES (?, ?, NULL)",
        )
        .bind(code_hash)
        .bind(expires_at_ms)
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(())
    }

    pub async fn consume_pairing(&self, code_hash: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE env_pairings SET used_at_ms = ? \
             WHERE code_hash = ? AND used_at_ms IS NULL AND expires_at_ms > ?",
        )
        .bind(now_ms())
        .bind(code_hash)
        .bind(now_ms())
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn put_invite(
        &self,
        code_hash: &str,
        environment_id: &EnvironmentId,
        installation_id: &str,
        capability: &str,
        expires_at_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO env_invites (code_hash, environment_id, installation_id, capability, expires_at_ms, consumed_at_ms) \
             VALUES (?, ?, ?, ?, ?, NULL)",
        )
        .bind(code_hash)
        .bind(environment_id.as_str())
        .bind(installation_id)
        .bind(capability)
        .bind(expires_at_ms)
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(())
    }

    pub async fn redeem_invite(
        &self,
        code_hash: &str,
        requested_environment: &EnvironmentId,
        subject_id: &str,
    ) -> Result<Option<(EnvironmentId, String)>> {
        let mut tx = self
            .raw_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::Database)?;
        let row = sqlx::query(
            "SELECT i.environment_id, i.installation_id, i.capability, i.expires_at_ms \
             FROM env_invites i \
             JOIN env_installations e ON e.environment_id = i.environment_id \
               AND e.installation_id = i.installation_id \
             WHERE i.code_hash = ? AND i.environment_id = ? \
               AND i.consumed_at_ms IS NULL AND i.expires_at_ms > ?",
        )
        .bind(code_hash)
        .bind(requested_environment.as_str())
        .bind(now_ms())
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let Some(row) = row else {
            tx.rollback().await.map_err(Error::Database)?;
            return Ok(None);
        };
        let environment_id = EnvironmentId::parse(
            row.try_get::<String, _>("environment_id")
                .map_err(Error::Database)?,
        )
        .map_err(|error| Error::Internal(format!("invalid invite environment: {error}")))?;
        let capability: String = row.try_get("capability").map_err(Error::Database)?;
        let issued_at = now_ms();
        sqlx::query(
            "INSERT INTO env_grants (subject_id, environment_id, capability, issuer_subject_id, issued_at_ms, expires_at_ms, consumed_at_ms) \
             VALUES (?, ?, ?, NULL, ?, NULL, NULL) \
             ON CONFLICT(subject_id, environment_id, capability) DO UPDATE SET \
               issuer_subject_id = NULL, issued_at_ms = excluded.issued_at_ms, \
               expires_at_ms = NULL, consumed_at_ms = NULL",
        )
        .bind(subject_id)
        .bind(environment_id.as_str())
        .bind(&capability)
        .bind(issued_at)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let consumed = sqlx::query(
            "UPDATE env_invites SET consumed_at_ms = ? WHERE code_hash = ? AND consumed_at_ms IS NULL",
        )
        .bind(issued_at)
        .bind(code_hash)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        if consumed.rows_affected() != 1 {
            tx.rollback().await.map_err(Error::Database)?;
            return Ok(None);
        }
        tx.commit().await.map_err(Error::Database)?;
        Ok(Some((environment_id, capability)))
    }

    pub async fn upsert_environment(
        &self,
        id: &EnvironmentId,
        installation_id: &str,
        name: &str,
        origin: &str,
        token_hash: &str,
        agents_json: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO env_installations \
             (environment_id, installation_id, name, origin, grant_authority, token_hash, agents_json) \
             VALUES (?, ?, ?, ?, CASE WHEN ? IN ('sandbox-provisioned', 'development') THEN 'server' ELSE 'machine' END, ?, ?) \
             ON CONFLICT(environment_id) DO UPDATE SET \
               installation_id = excluded.installation_id, name = excluded.name, \
               origin = excluded.origin, grant_authority = excluded.grant_authority, token_hash = excluded.token_hash, \
               agents_json = excluded.agents_json",
        )
        .bind(id.as_str())
        .bind(installation_id)
        .bind(name)
        .bind(origin)
        .bind(origin)
        .bind(token_hash)
        .bind(agents_json)
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(())
    }

    pub async fn environment_row(&self, id: &EnvironmentId) -> Result<Option<EnvironmentRow>> {
        let row = sqlx::query(
            "SELECT environment_id, installation_id, name, origin, token_hash, \
                    connected, connection_epoch, last_seen_at_ms, reachable_via, agents_json, facts_json \
             FROM env_installations WHERE environment_id = ?",
        )
        .bind(id.as_str())
        .fetch_optional(self.read_pool())
        .await
        .map_err(Error::Database)?;
        row.map(|row| {
            let raw_id: String = row.try_get("environment_id").map_err(Error::Database)?;
            let id = EnvironmentId::parse(raw_id).map_err(|error| {
                Error::Internal(format!(
                    "invalid environment id in operational store: {error}"
                ))
            })?;
            let epoch: i64 = row.try_get("connection_epoch").map_err(Error::Database)?;
            Ok(EnvironmentRow {
                id,
                installation_id: row.try_get("installation_id").map_err(Error::Database)?,
                name: row.try_get("name").map_err(Error::Database)?,
                origin: row.try_get("origin").map_err(Error::Database)?,
                token_hash: row.try_get("token_hash").map_err(Error::Database)?,
                connected: row
                    .try_get::<i64, _>("connected")
                    .map_err(Error::Database)?
                    != 0,
                connection_epoch: u64::try_from(epoch).unwrap_or(0),
                last_seen_at_ms: row.try_get("last_seen_at_ms").map_err(Error::Database)?,
                reachable_via: row.try_get("reachable_via").map_err(Error::Database)?,
                agents_json: row.try_get("agents_json").map_err(Error::Database)?,
                facts_json: row.try_get("facts_json").map_err(Error::Database)?,
            })
        })
        .transpose()
    }

    pub async fn list_environment_rows(&self) -> Result<Vec<EnvironmentRow>> {
        let rows = sqlx::query(
            "SELECT environment_id, installation_id, name, origin, token_hash, \
                    connected, connection_epoch, last_seen_at_ms, reachable_via, agents_json, facts_json \
             FROM env_installations ORDER BY environment_id",
        )
        .fetch_all(self.read_pool())
        .await
        .map_err(Error::Database)?;
        rows.into_iter()
            .map(|row| {
                let raw_id: String = row.try_get("environment_id").map_err(Error::Database)?;
                let id = EnvironmentId::parse(raw_id).map_err(|error| {
                    Error::Internal(format!(
                        "invalid environment id in operational store: {error}"
                    ))
                })?;
                let epoch: i64 = row.try_get("connection_epoch").map_err(Error::Database)?;
                Ok(EnvironmentRow {
                    id,
                    installation_id: row.try_get("installation_id").map_err(Error::Database)?,
                    name: row.try_get("name").map_err(Error::Database)?,
                    origin: row.try_get("origin").map_err(Error::Database)?,
                    token_hash: row.try_get("token_hash").map_err(Error::Database)?,
                    connected: row
                        .try_get::<i64, _>("connected")
                        .map_err(Error::Database)?
                        != 0,
                    connection_epoch: u64::try_from(epoch).unwrap_or(0),
                    last_seen_at_ms: row.try_get("last_seen_at_ms").map_err(Error::Database)?,
                    reachable_via: row.try_get("reachable_via").map_err(Error::Database)?,
                    agents_json: row.try_get("agents_json").map_err(Error::Database)?,
                    facts_json: row.try_get("facts_json").map_err(Error::Database)?,
                })
            })
            .collect()
    }

    pub async fn set_environment_connection(
        &self,
        id: &EnvironmentId,
        token_hash: &str,
        epoch: u64,
        connected: bool,
        reachable_via: Option<&str>,
        agents_json: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE env_installations SET connected = ?, connection_epoch = ?, \
                    last_seen_at_ms = ?, reachable_via = ?, agents_json = ? \
             WHERE environment_id = ? AND token_hash = ?",
        )
        .bind(if connected { 1_i64 } else { 0_i64 })
        .bind(i64::try_from(epoch).unwrap_or(i64::MAX))
        .bind(now_ms())
        .bind(reachable_via)
        .bind(agents_json)
        .bind(id.as_str())
        .bind(token_hash)
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn lease_environment_connection(
        &self,
        id: &EnvironmentId,
        token_hash: &str,
        agents_json: &str,
        facts_json: &str,
    ) -> Result<Option<u64>> {
        let epoch: Option<i64> = sqlx::query_scalar(
            "UPDATE env_installations SET connected = 1, connection_epoch = connection_epoch + 1, \
                    last_seen_at_ms = ?, reachable_via = NULL, agents_json = ?, facts_json = ? \
             WHERE environment_id = ? AND token_hash = ? \
             RETURNING connection_epoch",
        )
        .bind(now_ms())
        .bind(agents_json)
        .bind(facts_json)
        .bind(id.as_str())
        .bind(token_hash)
        .fetch_optional(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        epoch
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| Error::Internal("negative connection epoch".into()))
            })
            .transpose()
    }

    pub async fn clear_environment_connection_if_epoch(
        &self,
        id: &EnvironmentId,
        token_hash: &str,
        epoch: u64,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE env_installations SET connected = 0, last_seen_at_ms = ? \
             WHERE environment_id = ? AND token_hash = ? \
               AND connected = 1 AND connection_epoch = ?",
        )
        .bind(now_ms())
        .bind(id.as_str())
        .bind(token_hash)
        .bind(i64::try_from(epoch).unwrap_or(i64::MAX))
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn record_command_outcome(
        &self,
        environment: &EnvironmentId,
        request_id: &str,
        policy: IdempotencyPolicy,
        outcome: &CommandOutcome,
    ) -> Result<()> {
        let policy_name = match policy {
            IdempotencyPolicy::Replayable => "replayable",
            IdempotencyPolicy::AtMostOnce => "at_most_once",
        };
        let outcome_json =
            serde_json::to_string(outcome).map_err(|error| Error::Internal(error.to_string()))?;
        let mut tx = self
            .raw_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::Database)?;
        if let Some(existing) = sqlx::query(
            "SELECT idempotency, outcome_json FROM env_command_results \
             WHERE environment_id = ? AND request_id = ?",
        )
        .bind(environment.as_str())
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::Database)?
        {
            let existing_policy: String =
                existing.try_get("idempotency").map_err(Error::Database)?;
            if existing_policy != policy_name {
                tx.rollback().await.map_err(Error::Database)?;
                return Err(Error::InvalidRequest(
                    "command request id was reused with a different idempotency policy".into(),
                ));
            }
            let existing_json: String =
                existing.try_get("outcome_json").map_err(Error::Database)?;
            if let Ok(existing_outcome) = serde_json::from_str::<CommandOutcome>(&existing_json) {
                if existing_outcome.is_terminal() {
                    tx.rollback().await.map_err(Error::Database)?;
                    return Ok(());
                }
            }
            sqlx::query(
                "UPDATE env_command_results SET outcome_json = ?, updated_at_ms = ? \
                 WHERE environment_id = ? AND request_id = ?",
            )
            .bind(outcome_json)
            .bind(now_ms())
            .bind(environment.as_str())
            .bind(request_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::Database)?;
        } else {
            sqlx::query(
                "INSERT INTO env_command_results \
                 (environment_id, request_id, idempotency, outcome_json, updated_at_ms) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(environment.as_str())
            .bind(request_id)
            .bind(policy_name)
            .bind(outcome_json)
            .bind(now_ms())
            .execute(&mut *tx)
            .await
            .map_err(Error::Database)?;
        }
        tx.commit().await.map_err(Error::Database)?;
        Ok(())
    }

    pub async fn command_outcome(
        &self,
        environment: &EnvironmentId,
        request_id: &str,
    ) -> Result<Option<CommandOutcome>> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT outcome_json FROM env_command_results \
             WHERE environment_id = ? AND request_id = ?",
        )
        .bind(environment.as_str())
        .bind(request_id)
        .fetch_optional(self.read_pool())
        .await
        .map_err(Error::Database)?;
        raw.map(|raw| {
            serde_json::from_str(&raw).map_err(|error| {
                Error::Internal(format!(
                    "invalid command outcome in operational store: {error}"
                ))
            })
        })
        .transpose()
    }
}
