//! Operational subjects, grants and native Session metadata.

use super::ControlStore;
use crate::{
    error::{Error, Result},
    models::{now_ms, Session},
};
use sqlx::SqlitePool;
use sylph_protocol::SessionId;

#[derive(Debug, Clone)]
pub struct SessionFilter {
    pub limit: i64,
    pub agent: Option<String>,
    /// Which environment's sessions to list.
    pub environment: Option<String>,
    /// **One directory, matched exactly** — never a substring.
    ///
    /// This was `instr(lower(workdir), lower(?)) > 0`, and `AGENTS.md` records what
    /// that shape costs: asking for `/w/app` also returns `/w/app-old` and every
    /// subdirectory. It is fixed here rather than left for a caller to re-filter,
    /// because `api::workspaces` groups by exact workdir — so a listing and its own
    /// grouping disagreed about which sessions belong to a directory.
    ///
    /// Trailing slashes are normalized on both sides. The stored spelling is the
    /// canonical path, which is what session creation journals.
    pub workdir: Option<String>,
    pub updated_after_ms: Option<i64>,
    pub updated_before_ms: Option<i64>,
    /// Subject whose environment grants constrain this listing. When set, the
    /// capability predicate is evaluated in SQL before `LIMIT`, so an
    /// inaccessible environment cannot consume a page slot.
    pub capability_subject: Option<String>,
    pub capability: Option<String>,
}

impl Default for SessionFilter {
    fn default() -> Self {
        Self {
            limit: 50,
            agent: None,
            environment: None,
            workdir: None,
            updated_after_ms: None,
            updated_before_ms: None,
            capability_subject: None,
            capability: None,
        }
    }
}

impl ControlStore {
    pub async fn set_current_request(
        &self,
        id: &SessionId,
        request: &sylph_protocol::Request,
        epoch: u64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO current_requests(environment_id,session_id,request_id,request_json,connection_epoch) VALUES(?,?,?,?,?) ON CONFLICT(session_id,request_id) DO UPDATE SET request_json=excluded.request_json,connection_epoch=excluded.connection_epoch WHERE current_requests.state='waiting'").bind(id.environment.as_str()).bind(id.encode()).bind(&request.id).bind(serde_json::to_string(request).map_err(|e|Error::Internal(e.to_string()))?).bind(epoch as i64).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn current_request(
        &self,
        id: &SessionId,
        request: &str,
    ) -> Result<Option<sylph_protocol::Request>> {
        let json: Option<String> = sqlx::query_scalar(
            "SELECT request_json FROM current_requests WHERE session_id=? AND request_id=? AND state='waiting'",
        )
        .bind(id.encode())
        .bind(request)
        .fetch_optional(&self.readers)
        .await?;
        json.map(|j| serde_json::from_str(&j).map_err(|e| Error::Internal(e.to_string())))
            .transpose()
    }
    pub async fn set_control_state(
        &self,
        id: &SessionId,
        lease: Option<&str>,
        native: Option<&str>,
        active: bool,
        epoch: u64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO session_control_state VALUES(?,?,?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET lease=excluded.lease,native_turn_id=excluded.native_turn_id,active=excluded.active,connection_epoch=excluded.connection_epoch").bind(id.environment.as_str()).bind(id.encode()).bind(lease).bind(native).bind(active).bind(epoch as i64).execute(&self.pool).await?;
        self.update_session_status(id, if active { "running" } else { "idle" })
            .await?;
        Ok(())
    }
    pub async fn control_state(&self, id: &SessionId) -> Result<Option<(Option<String>, bool)>> {
        Ok(
            sqlx::query_as("SELECT lease,active FROM session_control_state WHERE session_id=?")
                .bind(id.encode())
                .fetch_optional(&self.readers)
                .await?,
        )
    }
    pub async fn clear_environment_runtime(
        &self,
        id: &sylph_protocol::EnvironmentId,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // A reconnect can repeat the same scoped vendor request. Retain its
        // resolution claim across disconnects; only unclaimed notices are stale.
        sqlx::query("DELETE FROM current_requests WHERE environment_id=? AND state='waiting'")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM session_control_state WHERE environment_id=?")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE sessions SET status='idle' WHERE environment=?")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn reset_runtime_connections(&self) -> Result<()> {
        sqlx::query("UPDATE env_installations SET connected=0")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM current_requests WHERE state='waiting'")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM session_control_state")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn suppress_session(&self, id: &SessionId) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE environment=? AND agent=? AND native_id=?")
            .bind(id.environment.as_str())
            .bind(id.agent.as_str())
            .bind(id.native_id.as_str())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM current_requests WHERE session_id=?")
            .bind(id.encode())
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM session_control_state WHERE session_id=?")
            .bind(id.encode())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn current_requests(&self, id: &SessionId) -> Result<Vec<serde_json::Value>> {
        let rows:Vec<String>=sqlx::query_scalar("SELECT request_json FROM current_requests WHERE session_id=? AND state='waiting' ORDER BY request_id LIMIT 128").bind(id.encode()).fetch_all(&self.readers).await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(&row).map_err(|e| Error::Internal(e.to_string())))
            .collect()
    }
    pub async fn revoke_subject_work(&self, subject: &str, environment: &str) -> Result<u64> {
        let ids:Vec<String>=sqlx::query_scalar("SELECT command_id FROM trace_commands WHERE environment_id=? AND origin_subject=? AND state IN('accepted','dispatching','receiver_accepted') ORDER BY command_id LIMIT 4096").bind(environment).bind(subject).fetch_all(&self.readers).await?;
        for id in &ids {
            let command = self
                .command(id)
                .await
                .map_err(|e| Error::Internal(e.to_string()))?
                .ok_or(Error::RequestNotFound)?;
            let fact = if command.state == crate::db::control::CommandState::Accepted {
                sylph_protocol::trace::DeliveryFact::Cancelled {
                    reason: "control authorization revoked before dispatch".into(),
                }
            } else {
                sylph_protocol::trace::DeliveryFact::OutcomeUnknown {
                    reason: "control authorization revoked".into(),
                }
            };
            self.record_delivery(id, "authorization-revoked", fact, now_ms())
                .await
                .map_err(|e| Error::Internal(e.to_string()))?;
        }
        Ok(ids.len() as u64)
    }
    pub async fn upsert_subject(
        &self,
        id: &str,
        name: &str,
        token_hash: &str,
        is_admin: bool,
    ) -> Result<()> {
        sqlx::query("INSERT INTO subjects (id, name, token_hash, is_admin, created_at_ms) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, token_hash = excluded.token_hash")
            .bind(id).bind(name).bind(token_hash).bind(is_admin as i64).bind(now_ms())
            .execute(&self.pool).await.map_err(Error::Database)?;
        Ok(())
    }

    pub async fn subject_id_for_token_hash(&self, token_hash: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT id FROM subjects WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_optional(&self.readers)
            .await
            .map_err(Error::Database)
    }

    pub async fn has_capability(
        &self,
        subject_id: &str,
        environment_id: &str,
        capability: &str,
    ) -> Result<bool> {
        if subject_id == "bootstrap" {
            // Bootstrap is a first-run local identity. It is active only until
            // the first registered Subject exists; after that every environment
            // is governed by durable grants.
            let active: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM subjects WHERE id <> 'bootstrap' LIMIT 1")
                    .fetch_optional(&self.readers)
                    .await
                    .map_err(Error::Database)?;
            if active.is_some() {
                return Ok(false);
            }
            return Ok(true);
        }
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM env_grants WHERE subject_id = ? AND environment_id = ? AND capability = ? AND (expires_at_ms IS NULL OR expires_at_ms > ?)")
            .bind(subject_id).bind(environment_id).bind(capability).bind(now_ms()).fetch_optional(&self.readers).await.map_err(Error::Database)?;
        Ok(found.is_some())
    }

    pub async fn capable_environments(
        &self,
        subject_id: &str,
        capability: &str,
    ) -> Result<Vec<String>> {
        if subject_id == "bootstrap" {
            return sqlx::query_scalar(
                "SELECT environment_id FROM env_installations
                 WHERE NOT EXISTS (SELECT 1 FROM subjects WHERE id <> 'bootstrap')",
            )
            .fetch_all(&self.readers)
            .await
            .map_err(Error::Database);
        }
        sqlx::query_scalar(
            "SELECT environment_id FROM env_grants
             WHERE subject_id = ? AND capability = ?
               AND (expires_at_ms IS NULL OR expires_at_ms > ?)",
        )
        .bind(subject_id)
        .bind(capability)
        .bind(now_ms())
        .fetch_all(&self.readers)
        .await
        .map_err(Error::Database)
    }

    pub async fn authorization_status(
        &self,
        subject_id: &str,
        environment_id: &str,
        capability: &str,
    ) -> Result<Option<&'static str>> {
        if subject_id == "bootstrap" {
            let active: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM subjects WHERE id <> 'bootstrap' LIMIT 1")
                    .fetch_optional(&self.readers)
                    .await
                    .map_err(Error::Database)?;
            if active.is_some() {
                return Ok(Some("not_granted"));
            }
            return Ok(Some("granted"));
        }
        let row: Option<Option<i64>> = sqlx::query_scalar(
            "SELECT expires_at_ms FROM env_grants WHERE subject_id = ? AND environment_id = ? AND capability = ?",
        )
        .bind(subject_id)
        .bind(environment_id)
        .bind(capability)
        .fetch_optional(&self.readers)
        .await
        .map_err(Error::Database)?;
        Ok(match row {
            None => Some("not_granted"),
            Some(Some(expires)) if expires <= now_ms() => Some("expired"),
            Some(_) => Some("granted"),
        })
    }

    pub async fn is_admin(&self, subject_id: &str) -> Result<bool> {
        if subject_id == "bootstrap" {
            let active: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM subjects WHERE id <> 'bootstrap' LIMIT 1")
                    .fetch_optional(&self.readers)
                    .await
                    .map_err(Error::Database)?;
            return Ok(active.is_none());
        }
        let value: Option<i64> = sqlx::query_scalar("SELECT is_admin FROM subjects WHERE id = ?")
            .bind(subject_id)
            .fetch_optional(&self.readers)
            .await
            .map_err(Error::Database)?;
        Ok(value == Some(1))
    }

    pub async fn grant_authority(&self, environment_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT grant_authority FROM env_installations WHERE environment_id = ?")
            .bind(environment_id)
            .fetch_optional(&self.readers)
            .await
            .map_err(Error::Database)
    }

    pub async fn revoke_grant(
        &self,
        subject_id: &str,
        environment_id: &str,
        capability: &str,
    ) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM env_grants WHERE subject_id = ? AND environment_id = ? AND capability = ?",
        )
        .bind(subject_id)
        .bind(environment_id)
        .bind(capability)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn issue_grant(
        &self,
        subject_id: &str,
        environment_id: &str,
        capability: &str,
        issuer_subject_id: &str,
        expires_at_ms: Option<i64>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO env_grants (subject_id, environment_id, capability, issuer_subject_id, issued_at_ms, expires_at_ms, consumed_at_ms) VALUES (?, ?, ?, ?, ?, ?, NULL) ON CONFLICT(subject_id, environment_id, capability) DO UPDATE SET issuer_subject_id = excluded.issuer_subject_id, issued_at_ms = excluded.issued_at_ms, expires_at_ms = excluded.expires_at_ms, consumed_at_ms = NULL")
            .bind(subject_id).bind(environment_id).bind(capability).bind(issuer_subject_id).bind(now_ms()).bind(expires_at_ms)
            .execute(&self.pool).await.map_err(Error::Database)?;
        Ok(())
    }

    pub async fn create_session(&self, session: &Session) -> Result<Session> {
        sqlx::query_as::<_, Session>(
            r#"
            INSERT INTO sessions (
                environment, agent, native_id, workdir, title,
                inject_identity, fidelity, flags_json, status,
                created_at_ms, updated_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(session.id.environment.as_str())
        .bind(session.id.agent.as_str())
        .bind(session.id.native_id.as_str())
        .bind(&session.workdir)
        .bind(&session.title)
        .bind(session.inject_identity)
        .bind(session.fidelity.to_string())
        .bind(&session.flags_json)
        .bind(&session.status)
        .bind(session.created_at_ms)
        .bind(session.updated_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_session(&self, id: &SessionId) -> Result<Option<Session>> {
        let session = sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .fetch_optional(&self.readers)
        .await
        .map_err(Error::Database)?;
        Ok(session)
    }

    pub async fn adopted_native_ids(
        &self,
        environment: &sylph_protocol::EnvironmentId,
        agent: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT native_id FROM sessions WHERE environment = ? AND agent = ?")
                .bind(environment.as_str())
                .bind(agent)
                .fetch_all(&self.readers)
                .await
                .map_err(Error::Database)?;
        Ok(rows.into_iter().map(|(native,)| native).collect())
    }

    pub async fn list_sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>> {
        // Every predicate is a no-op when its bound value is NULL, which keeps
        // one prepared statement covering all filter combinations.
        let sessions = sqlx::query_as::<_, Session>(
            r#"
            SELECT * FROM sessions
            WHERE (?1 IS NULL OR agent = ?1)
              AND (?2 IS NULL OR environment = ?2)
              -- An exact match, not a substring: see `SessionFilter::workdir`.
              -- `rtrim` normalizes a trailing slash on both sides, so two
              -- spellings of one directory cannot become two workspaces.
              AND (?3 IS NULL OR rtrim(workdir, '/') = rtrim(?3, '/'))
              AND (?4 IS NULL OR updated_at_ms >= ?4)
              AND (?5 IS NULL OR updated_at_ms <= ?5)
              AND (
                    ?7 IS NULL
                    OR EXISTS (
                        SELECT 1 FROM env_grants
                        WHERE env_grants.subject_id = ?7
                          AND env_grants.environment_id = sessions.environment
                          AND env_grants.capability = ?8
                          AND (env_grants.expires_at_ms IS NULL OR env_grants.expires_at_ms > ?9)
                    )
                    OR (
                        ?7 = 'bootstrap'
                        AND NOT EXISTS (SELECT 1 FROM subjects WHERE id <> 'bootstrap')
                    )
              )
            -- The identity triple breaks ties, which makes this a **total** order.
            -- `updated_at_ms` alone is not one: it is milliseconds, and adopting a
            -- native store writes many sessions inside a single tick. Two rows sharing
            -- a stamp then have no defined order, so a page boundary landing inside a
            -- tied group returns rows in an order the next query need not agree with —
            -- and `updatedBeforeMs` is exactly that next query, since it *is* the
            -- keyset cursor for this listing. The result is a page that duplicates or
            -- drops a session while looking entirely well-formed, which is the
            -- expensive kind of defect: nothing errors.
            --
            -- The triple is the primary key, so it is unique and never null. Left off
            -- `sessions_updated_idx` deliberately: a tied group is a handful of rows,
            -- and sorting those inside the scan costs less than a wider index on every
            -- write. Add one when a tie group is measured large, not before.
            ORDER BY updated_at_ms DESC, environment DESC, agent DESC, native_id DESC
            LIMIT ?6
            "#,
        )
        .bind(filter.agent.as_deref())
        .bind(filter.environment.as_deref())
        .bind(filter.workdir.as_deref())
        .bind(filter.updated_after_ms)
        .bind(filter.updated_before_ms)
        .bind(filter.limit)
        .bind(filter.capability_subject.as_deref())
        .bind(filter.capability.as_deref())
        .bind(filter.capability_subject.as_ref().map(|_| now_ms()))
        .fetch_all(&self.readers)
        .await
        .map_err(Error::Database)?;
        Ok(sessions)
    }

    pub async fn update_session_status(&self, id: &SessionId, status: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET status = ?, updated_at_ms = ?
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(status)
        .bind(now_ms())
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_session(&self, id: &SessionId) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET updated_at_ms = ?
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(now_ms())
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_session_title(&self, id: &SessionId, title: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET title = ?, updated_at_ms = ?
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(title)
        .bind(now_ms())
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_session_title_if_empty(&self, id: &SessionId, title: &str) -> Result<()> {
        let title: String = title
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect();
        if title.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE sessions SET title = ?
              WHERE environment = ? AND agent = ? AND native_id = ?
                AND (title IS NULL OR trim(title) = '')",
        )
        .bind(&title)
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_session_flags(&self, id: &SessionId, flags_json: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET flags_json = ?, updated_at_ms = ?
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(flags_json)
        .bind(now_ms())
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_session_flag(&self, id: &SessionId, key: &str, value: &str) -> Result<()> {
        // SQLite's JSON1 update is one write statement, so two controls changed at
        // nearly the same time cannot lose each other's key as a read/merge/write
        // sequence would. Rows produced by Sylph always contain an object, but the
        // `json_valid` guard keeps a hand-edited store from turning a config change
        // into an opaque database error.
        let result = sqlx::query(
            "UPDATE sessions
                SET flags_json = json_patch(
                      CASE WHEN json_valid(flags_json)
                                AND json_type(flags_json) = 'object'
                           THEN flags_json ELSE '{}' END,
                      json_object(?, ?)),
                    updated_at_ms = ?
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(key)
        .bind(value)
        .bind(now_ms())
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        if result.rows_affected() == 0 {
            return Err(Error::SessionNotFound);
        }
        Ok(())
    }

    pub async fn count_sessions_by_status(&self) -> Result<Vec<(String, i64)>> {
        sqlx::query_as::<_, (String, i64)>("SELECT status, COUNT(*) FROM sessions GROUP BY status")
            .fetch_all(&self.readers)
            .await
            .map_err(Error::Database)
    }

    pub async fn session_status(&self, id: &SessionId) -> Option<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM sessions
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .fetch_optional(&self.readers)
        .await
        .ok()
        .flatten()
    }

    pub(crate) fn raw_pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub(crate) fn read_pool(&self) -> &SqlitePool {
        &self.readers
    }
}
