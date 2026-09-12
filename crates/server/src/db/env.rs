//! Persistence helpers for the envagent bridge.

use crate::error::{Error, Result};
use crate::models::now_ms;
use crate::models::Event;
use sqlx::Row;
use sylph_protocol::{
    CommandOutcome, EnvironmentId, IdempotencyPolicy, ProducerId, ProducerSeq, ServerSeq,
    SessionEdge, SessionId,
};

use super::JournalStore;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvIngestResult {
    Inserted,
    Duplicate { server_seq: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvIngestOutcome {
    pub result: EnvIngestResult,
    pub server_seq: ServerSeq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvReceipt {
    pub server_seq: u64,
    pub payload_hash: String,
}

impl JournalStore {
    /// Persist machine facts reported by the owning envagent. Facts are opaque
    /// key/value metadata; the envagent is the authority because only it can
    /// inspect the OS user, home, and browse roots on that machine.
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
        .fetch_all(self.raw_pool())
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

    pub async fn list_session_edges(
        &self,
        session: Option<&SessionId>,
    ) -> Result<Vec<SessionEdge>> {
        let rows = if let Some(session) = session {
            sqlx::query(
                "SELECT parent_env, parent_agent, parent_native, child_env, child_agent, child_native, relation \
                 FROM session_edges WHERE (parent_env = ? AND parent_agent = ? AND parent_native = ?) \
                    OR (child_env = ? AND child_agent = ? AND child_native = ?)",
            )
            .bind(session.environment.as_str())
            .bind(session.agent.as_str())
            .bind(session.native_id.as_str())
            .bind(session.environment.as_str())
            .bind(session.agent.as_str())
            .bind(session.native_id.as_str())
            .fetch_all(self.raw_pool())
            .await
            .map_err(Error::Database)?
        } else {
            sqlx::query(
                "SELECT parent_env, parent_agent, parent_native, child_env, child_agent, child_native, relation \
                 FROM session_edges",
            )
            .fetch_all(self.raw_pool())
            .await
            .map_err(Error::Database)?
        };
        rows.into_iter()
            .map(|row| {
                let parent = SessionId::new(
                    EnvironmentId::parse(
                        row.try_get::<String, _>("parent_env")
                            .map_err(Error::Database)?,
                    )
                    .map_err(|error| Error::Internal(error.to_string()))?,
                    sylph_protocol::AgentName::parse(
                        row.try_get::<String, _>("parent_agent")
                            .map_err(Error::Database)?,
                    )
                    .map_err(|error| Error::Internal(error.to_string()))?,
                    sylph_protocol::NativeId::new(
                        row.try_get::<String, _>("parent_native")
                            .map_err(Error::Database)?,
                    ),
                );
                let child = SessionId::new(
                    EnvironmentId::parse(
                        row.try_get::<String, _>("child_env")
                            .map_err(Error::Database)?,
                    )
                    .map_err(|error| Error::Internal(error.to_string()))?,
                    sylph_protocol::AgentName::parse(
                        row.try_get::<String, _>("child_agent")
                            .map_err(Error::Database)?,
                    )
                    .map_err(|error| Error::Internal(error.to_string()))?,
                    sylph_protocol::NativeId::new(
                        row.try_get::<String, _>("child_native")
                            .map_err(Error::Database)?,
                    ),
                );
                let relation = match row
                    .try_get::<String, _>("relation")
                    .map_err(Error::Database)?
                    .as_str()
                {
                    "delegation" => sylph_protocol::SessionEdgeRelation::Delegation,
                    "fork" => sylph_protocol::SessionEdgeRelation::Fork,
                    other => {
                        return Err(Error::Internal(format!(
                            "unknown session edge relation {other}"
                        )))
                    }
                };
                SessionEdge::new(parent, child, relation)
                    .map_err(|error| Error::Internal(error.to_string()))
            })
            .collect()
    }

    /// Allocate a canonical sequence and persist the event/receipt atomically.
    ///
    /// Remote ingest cannot reserve a number in the in-memory allocator first:
    /// a failed transaction would leave a gap that a reconnect could never
    /// explain. The sequence is derived while the same write transaction is
    /// open and is returned alongside the deduplication result.
    pub async fn ingest_env_event_auto(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
        event: &mut Event,
        payload_hash: &str,
        min_next_seq: i32,
    ) -> Result<EnvIngestOutcome> {
        if &event.session_id.environment != environment {
            return Err(Error::InvalidRequest(
                "event session environment does not match the authenticated envagent".into(),
            ));
        }
        let mut tx = self.raw_pool().begin().await.map_err(Error::Database)?;
        let producer_seq_i64 = i64::try_from(producer_seq.get())
            .map_err(|_| Error::InvalidRequest("producer sequence is too large".into()))?;
        if let Some(existing) = sqlx::query(
            "SELECT server_seq, payload_hash FROM env_event_receipts \
             WHERE environment_id = ? AND producer_id = ? AND producer_seq = ?",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(producer_seq_i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::Database)?
        {
            let existing_hash: String =
                existing.try_get("payload_hash").map_err(Error::Database)?;
            if existing_hash != payload_hash {
                tx.rollback().await.map_err(Error::Database)?;
                return Err(Error::InvalidRequest(
                    "producer sequence was replayed with different payload".into(),
                ));
            }
            let existing: i64 = existing.try_get("server_seq").map_err(Error::Database)?;
            tx.rollback().await.map_err(Error::Database)?;
            let server_seq = u64::try_from(existing)
                .map(ServerSeq::new)
                .map_err(|_| Error::Internal("negative server sequence in receipt".into()))?;
            return Ok(EnvIngestOutcome {
                result: EnvIngestResult::Duplicate {
                    server_seq: server_seq.get(),
                },
                server_seq,
            });
        }
        let max_event: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(seq) FROM events WHERE session_env = ? AND session_agent = ? AND session_native = ?",
        )
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let max_receipt: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(server_seq) FROM env_event_receipts \
             WHERE session_env = ? AND session_agent = ? AND session_native = ?",
        )
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let next = max_event
            .into_iter()
            .chain(max_receipt)
            .chain(std::iter::once(i64::from(min_next_seq.saturating_sub(1))))
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        let server_seq = ServerSeq::new(
            u64::try_from(next).map_err(|_| Error::Internal("server sequence overflow".into()))?,
        );
        event.seq = i32::try_from(next)
            .map_err(|_| Error::Internal("server sequence exceeds journal range".into()))?;
        sqlx::query(
            "INSERT INTO events (
                session_env, session_agent, session_native, turn_id, seq, kind,
                phase, stream_id, span, interior_state, name, text, data_json,
                is_error, stability, source, arrival_at_ms, recorded_at_ms, created_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.span)
        .bind(&event.interior_state)
        .bind(&event.name)
        .bind(&event.text)
        .bind(&event.data_json)
        .bind(event.is_error)
        .bind(&event.stability)
        .bind("agent")
        .bind(event.arrival_at_ms)
        .bind(event.recorded_at_ms)
        .bind(event.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        sqlx::query(
            "INSERT INTO env_event_receipts (
                environment_id, producer_id, producer_seq, session_env,
                session_agent, session_native, server_seq, payload_hash, accepted_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(producer_seq_i64)
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .bind(i64::try_from(server_seq.get()).unwrap_or(i64::MAX))
        .bind(payload_hash)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        tx.commit().await.map_err(Error::Database)?;
        Ok(EnvIngestOutcome {
            result: EnvIngestResult::Inserted,
            server_seq,
        })
    }

    pub async fn set_turn_request_text(
        &self,
        session: &SessionId,
        turn_id: &str,
        text: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE turns SET request_text = ? \
             WHERE id = ? AND session_env = ? AND session_agent = ? AND session_native = ?",
        )
        .bind(text)
        .bind(turn_id)
        .bind(session.environment.as_str())
        .bind(session.agent.as_str())
        .bind(session.native_id.as_str())
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn delete_environment(&self, id: &EnvironmentId) -> Result<bool> {
        let result = sqlx::query("DELETE FROM env_installations WHERE environment_id = ?")
            .bind(id.as_str())
            .execute(self.raw_pool())
            .await
            .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    /// Atomically append a canonical event and its producer receipt.
    ///
    /// The receipt and event share one transaction so a crash cannot leave
    /// the server with a deduplication marker but no journal row (or the
    /// inverse, which would replay the event under a new server sequence).
    pub async fn ingest_env_event(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
        server_seq: ServerSeq,
        event: &Event,
        payload_hash: &str,
    ) -> Result<EnvIngestResult> {
        if &event.session_id.environment != environment {
            return Err(Error::InvalidRequest(
                "event session environment does not match the authenticated envagent".into(),
            ));
        }
        let mut tx = self.raw_pool().begin().await.map_err(Error::Database)?;
        let producer_seq_i64 = i64::try_from(producer_seq.get())
            .map_err(|_| Error::InvalidRequest("producer sequence is too large".into()))?;
        if let Some(existing) = sqlx::query_scalar::<_, i64>(
            "SELECT server_seq FROM env_event_receipts \
             WHERE environment_id = ? AND producer_id = ? AND producer_seq = ?",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(producer_seq_i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::Database)?
        {
            tx.rollback().await.map_err(Error::Database)?;
            return Ok(EnvIngestResult::Duplicate {
                server_seq: u64::try_from(existing).unwrap_or(0),
            });
        }

        sqlx::query(
            "INSERT INTO events (
                session_env, session_agent, session_native, turn_id, seq, kind,
                phase, stream_id, span, interior_state, name, text, data_json,
                is_error, stability, source, arrival_at_ms, recorded_at_ms, created_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.span)
        .bind(&event.interior_state)
        .bind(&event.name)
        .bind(&event.text)
        .bind(&event.data_json)
        .bind(event.is_error)
        .bind(&event.stability)
        .bind("agent")
        .bind(event.arrival_at_ms)
        .bind(event.recorded_at_ms)
        .bind(event.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;

        sqlx::query(
            "INSERT INTO env_event_receipts (
                environment_id, producer_id, producer_seq, session_env,
                session_agent, session_native, server_seq, payload_hash, accepted_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(producer_seq_i64)
        .bind(event.session_id.environment.as_str())
        .bind(event.session_id.agent.as_str())
        .bind(event.session_id.native_id.as_str())
        .bind(i64::try_from(server_seq.get()).unwrap_or(i64::MAX))
        .bind(payload_hash)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        tx.commit().await.map_err(Error::Database)?;
        Ok(EnvIngestResult::Inserted)
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

    /// Consume a pairing code exactly once.  Returning false means either
    /// unknown, expired, or already consumed; callers deliberately do not
    /// distinguish those cases to avoid turning codes into an oracle.
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
        .fetch_optional(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        row.map(|row| {
            let raw_id: String = row.try_get("environment_id").map_err(Error::Database)?;
            let id = EnvironmentId::parse(raw_id).map_err(|error| {
                Error::Internal(format!("invalid environment id in journal: {error}"))
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
        .fetch_all(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        rows.into_iter()
            .map(|row| {
                let raw_id: String = row.try_get("environment_id").map_err(Error::Database)?;
                let id = EnvironmentId::parse(raw_id).map_err(|error| {
                    Error::Internal(format!("invalid environment id in journal: {error}"))
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

    /// Atomically acquire the next connection epoch for an installation.
    ///
    /// Registration used to read `connection_epoch`, add one in Rust, then
    /// write it back. Two simultaneous envagent connections could therefore
    /// receive the same epoch and either socket could later pass the stale
    /// writer check. The increment and credential check belong in one SQL
    /// statement so each lease has a unique monotonic epoch.
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

    /// Mark a lease disconnected only if it is still the lease identified by
    /// `epoch`.  This is the disconnect-side compare-and-set counterpart to
    /// `set_environment_connection`: an old socket must not clobber a newer
    /// reconnect that reused the same installation credential.
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
        let mut tx = self.raw_pool().begin().await.map_err(Error::Database)?;
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
        .fetch_optional(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        raw.map(|raw| {
            serde_json::from_str(&raw).map_err(|error| {
                Error::Internal(format!("invalid command outcome in journal: {error}"))
            })
        })
        .transpose()
    }

    /// Insert a producer receipt and return whether it was new.  The bridge
    /// calls this before assigning a canonical server sequence.
    pub async fn env_receipt(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
    ) -> Result<Option<u64>> {
        let seq = i64::try_from(producer_seq.get())
            .map_err(|_| Error::InvalidRequest("producer sequence is too large".into()))?;
        let value: Option<i64> = sqlx::query_scalar(
            "SELECT server_seq FROM env_event_receipts \
             WHERE environment_id = ? AND producer_id = ? AND producer_seq = ?",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(seq)
        .fetch_optional(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(value.and_then(|value| u64::try_from(value).ok()))
    }

    pub async fn env_receipt_detail(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
    ) -> Result<Option<EnvReceipt>> {
        let seq = i64::try_from(producer_seq.get())
            .map_err(|_| Error::InvalidRequest("producer sequence is too large".into()))?;
        let row = sqlx::query(
            "SELECT server_seq, payload_hash FROM env_event_receipts \
             WHERE environment_id = ? AND producer_id = ? AND producer_seq = ?",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(seq)
        .fetch_optional(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        row.map(|row| {
            Ok(EnvReceipt {
                server_seq: u64::try_from(
                    row.try_get::<i64, _>("server_seq")
                        .map_err(Error::Database)?,
                )
                .unwrap_or(0),
                payload_hash: row.try_get("payload_hash").map_err(Error::Database)?,
            })
        })
        .transpose()
    }

    pub async fn env_accepted_through(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
    ) -> Result<u64> {
        let values: Vec<i64> = sqlx::query_scalar(
            "SELECT producer_seq FROM env_event_receipts \
             WHERE environment_id = ? AND producer_id = ? ORDER BY producer_seq",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .fetch_all(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        let mut expected = 1_u64;
        let mut values: Vec<u64> = values
            .into_iter()
            .filter_map(|value| u64::try_from(value).ok())
            .collect();
        values.sort_unstable();
        for value in values {
            if value != expected {
                break;
            }
            expected = expected.saturating_add(1);
        }
        Ok(expected.saturating_sub(1))
    }

    pub async fn insert_env_receipt(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
        session: &SessionId,
        server_seq: ServerSeq,
        payload_hash: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO env_event_receipts \
             (environment_id, producer_id, producer_seq, session_env, session_agent, \
              session_native, server_seq, payload_hash, accepted_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(i64::try_from(producer_seq.get()).unwrap_or(i64::MAX))
        .bind(session.environment.as_str())
        .bind(session.agent.as_str())
        .bind(session.native_id.as_str())
        .bind(i64::try_from(server_seq.get()).unwrap_or(i64::MAX))
        .bind(payload_hash)
        .bind(now_ms())
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_env_message_receipt(
        &self,
        environment: &EnvironmentId,
        producer: &ProducerId,
        producer_seq: ProducerSeq,
        payload_hash: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO env_event_receipts (
                environment_id, producer_id, producer_seq, session_env,
                session_agent, session_native, server_seq, payload_hash, accepted_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, 0, ?, ?)",
        )
        .bind(environment.as_str())
        .bind(producer.as_str())
        .bind(i64::try_from(producer_seq.get()).unwrap_or(i64::MAX))
        .bind(environment.as_str())
        .bind("__message__")
        .bind("__message__")
        .bind(payload_hash)
        .bind(now_ms())
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_session_edge(&self, edge: &SessionEdge) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO session_edges \
             (parent_env, parent_agent, parent_native, child_env, child_agent, child_native, \
              relation, source, created_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(edge.parent.environment.as_str())
        .bind(edge.parent.agent.as_str())
        .bind(edge.parent.native_id.as_str())
        .bind(edge.child.environment.as_str())
        .bind(edge.child.agent.as_str())
        .bind(edge.child.native_id.as_str())
        .bind(edge.relation.as_str())
        // The edge is a server-owned read model. The envagent supplied
        // provenance, but it cannot author the server's relationship policy.
        .bind("server")
        .bind(now_ms())
        .execute(self.raw_pool())
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }
}
