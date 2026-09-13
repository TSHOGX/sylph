use crate::db::labels::summarize_line;
use crate::error::{Error, Result};
use crate::events::data_from_column;
use crate::models::now_ms;
use crate::models::{Event, PendingRequest, Session, Turn};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use sylph_protocol::SessionId;

#[derive(Clone)]
pub struct JournalStore {
    pool: SqlitePool,
}

/// Narrowing criteria for a session listing. `None` means "do not narrow on
/// this field", which is distinct from matching an empty value.
///
/// `origin` is gone with the one-class-of-session rule (`docs/architecture.md`
/// § One class of session), and `environment` replaces it as the axis a listing
/// genuinely filters on — every listing does, which is why it is also the leading
/// column of `sessions_workdir_idx`.
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

impl JournalStore {
    pub async fn upsert_subject(
        &self,
        id: &str,
        name: &str,
        token_hash: &str,
        is_admin: bool,
    ) -> Result<()> {
        sqlx::query("INSERT INTO subjects (id, name, token_hash, is_admin, created_at_ms) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name")
            .bind(id).bind(name).bind(token_hash).bind(is_admin as i64).bind(now_ms())
            .execute(&self.pool).await.map_err(Error::Database)?;
        Ok(())
    }

    pub async fn subject_id_for_token_hash(&self, token_hash: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT id FROM subjects WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_optional(&self.pool)
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
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(Error::Database)?;
            if active.is_some() {
                return Ok(false);
            }
            return Ok(true);
        }
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM env_grants WHERE subject_id = ? AND environment_id = ? AND capability = ? AND (expires_at_ms IS NULL OR expires_at_ms > ?)")
            .bind(subject_id).bind(environment_id).bind(capability).bind(now_ms()).fetch_optional(&self.pool).await.map_err(Error::Database)?;
        Ok(found.is_some())
    }

    /// Return every environment on which a subject currently holds `capability`.
    ///
    /// Search and other cross-environment reads use this set as a SQL boundary in
    /// the history database. Resolving it once avoids an N+1 capability check and,
    /// crucially, lets the history query apply its limit *after* authorization.
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
            .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
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
                    .fetch_optional(&self.pool)
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
        .fetch_optional(&self.pool)
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
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(Error::Database)?;
            return Ok(active.is_none());
        }
        let value: Option<i64> = sqlx::query_scalar("SELECT is_admin FROM subjects WHERE id = ?")
            .bind(subject_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(value == Some(1))
    }

    pub async fn grant_authority(&self, environment_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT grant_authority FROM env_installations WHERE environment_id = ?")
            .bind(environment_id)
            .fetch_optional(&self.pool)
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

    pub async fn revoke_subject_work(&self, subject_id: &str, environment_id: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await.map_err(Error::Database)?;
        let closed_trajectories = sqlx::query(
            "UPDATE events SET phase = 'completed', is_error = 0, data_json = ?
             WHERE turn_id IN (SELECT id FROM turns WHERE origin_subject = ? AND session_env = ?
               AND status IN ('queued','starting','running','stopping','settling'))
               AND kind = 'subagent' AND phase <> 'completed'",
        )
        .bind(r#"{"closedBy":"server","reason":"abandoned"}"#)
        .bind(subject_id)
        .bind(environment_id)
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let result = sqlx::query("UPDATE turns SET status = 'interrupted', close_reason = 'grant_revoked', completed_at_ms = COALESCE(completed_at_ms, ?) WHERE origin_subject = ? AND session_env = ? AND status IN ('queued','starting','running','stopping','settling')")
            .bind(now_ms()).bind(subject_id).bind(environment_id).execute(&mut *tx).await.map_err(Error::Database)?;
        tx.commit().await.map_err(Error::Database)?;
        Ok(result
            .rows_affected()
            .max(closed_trajectories.rows_affected()))
    }
    /// Internal access for the environment bridge's migration-backed tables.
    ///
    /// Keeping the pool private to the database module prevents API handlers
    /// from issuing ad-hoc journal queries while still letting the bridge use
    /// one connection pool and transaction boundary.
    pub(crate) fn raw_pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn new(path: &str) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Internal(format!("Failed to create data dir: {}", e)))?;
        }

        // WAL keeps readers (SSE replay, session list) from blocking the
        // writer that is appending a running turn's events. `foreign_keys` is
        // off by default in SQLite and must be enabled per connection for the
        // schema's ON DELETE CASCADE rules to actually fire.
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            // SQLite has one writer even in WAL mode. The bridge, event pump,
            // consolidator, and request handlers may all arrive together; a
            // five-second timeout turned ordinary startup bursts into 503s.
            // Wait long enough for the writer transaction to finish rather
            // than exposing a transient lock as an unavailable environment.
            .busy_timeout(std::time::Duration::from_secs(30));

        let pool = SqlitePoolOptions::new()
            // One writer connection makes read-then-write CAS transactions
            // deterministic. WAL still lets the separate history database and
            // external readers proceed, while multiple journal connections can
            // otherwise hit SQLITE_BUSY_SNAPSHOT when a transaction upgrades.
            .max_connections(1)
            .connect_with(options)
            .await?;

        // Run migrations.
        //
        // A *checksum* failure here is the expected cost of this project's compatibility
        // posture, not a bug: migrations are edited in place rather than appended to
        // (`AGENTS.md` § Compatibility), so a journal written before an edit cannot be
        // migrated onto it. The remedy is to delete the data directory, which is safe
        // because a live journal is re-derivable from the agents' own native stores — and
        // saying so here is the difference between a one-line fix and reading sqlx's
        // internals to guess at one.
        sqlx::migrate!("./migrations/journal")
            .run(&pool)
            .await
            .map_err(|e| {
                Error::Internal(format!(
                    "Migration failed: {e}\n\
                     A version-mismatch or checksum error means this journal predates an \
                     edit to a migration. Sylph's journal is a cache of the agents' own \
                     stores, so deleting the data directory and restarting is the fix."
                ))
            })?;

        Ok(Self { pool })
    }

    // Session operations

    /// Record a session an agent has already assigned an id.
    ///
    /// **The caller must have started the agent first.** A row exists only once a
    /// session has an identity (`docs/architecture.md` § Identity), so this is the
    /// second half of a prepared-session promotion rather than the whole of it — see
    /// The bridge promotion path owns that ordering.
    /// Nothing here can enforce it, and that is what the type is for: a [`Session`]
    /// cannot be constructed without a [`SessionId`], which only an agent's answer
    /// produces.
    ///
    /// A duplicate triple is a genuine conflict rather than an upsert. Two rows for
    /// one conversation is exactly the state the primary key exists to refuse.
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

    /// Insert a newly promoted session and its first user turn atomically.
    ///
    /// A prepared link has no durable identity until promotion, so the session row and
    /// first turn cannot be created independently: a crash between the two would leave
    /// an empty history row, recreating the bug this lifecycle exists to remove.
    pub async fn create_session_and_turn(
        &self,
        session: &Session,
        turn: &Turn,
    ) -> Result<(Session, Turn)> {
        let mut tx = self.pool.begin().await.map_err(Error::Database)?;
        let stored_session = sqlx::query_as::<_, Session>(
            r#"INSERT INTO sessions (
                environment, agent, native_id, workdir, title,
                inject_identity, fidelity, flags_json, status,
                created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *"#,
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
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        let stored_turn = sqlx::query_as::<_, Turn>(
            r#"INSERT INTO turns (
                id, session_env, session_agent, session_native, status, origin,
                origin_subject, request_text, command_invocation_id, idempotency_key, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *"#,
        )
        .bind(&turn.id)
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
        .bind(&turn.status)
        .bind(&turn.origin)
        .bind(&turn.origin_subject)
        .bind(&turn.request_text)
        .bind(&turn.command_invocation_id)
        .bind(&turn.idempotency_key)
        .bind(turn.created_at_ms)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        tx.commit().await.map_err(Error::Database)?;
        Ok((stored_session, stored_turn))
    }

    pub async fn get_session(&self, id: &SessionId) -> Result<Option<Session>> {
        let mut session = sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)?;
        if let Some(session) = session.as_mut() {
            self.hydrate_session_title(session).await?;
        }
        Ok(session)
    }

    /// Which native ids of one agent on one environment are already recorded.
    ///
    /// # Why this exists rather than a `get_session` per candidate
    ///
    /// `api::sessions::enumerate` marks each native session the store holds with whether
    /// Sylph already has it, and it has to answer that for **every** row: a picker that
    /// silently omitted adopted sessions would leave a user hunting for one plainly on
    /// the machine, and one that offered them would hand out a `409` nobody can act on.
    ///
    /// Asked per row, that is one indexed lookup each — correct, and measured at **seconds**
    /// on a real store, which is the round-trip count rather than the query. It also inverts
    /// the requirement the adapter contract puts on `enumerate`: that listing pass is
    /// deliberately one short bounded read per file precisely so a whole-store listing is
    /// affordable, and paying a database round trip per row hands that back one layer up.
    ///
    /// One statement, projecting the single column the caller compares. Deliberately **not**
    /// [`Self::list_sessions`]: that returns whole rows, hydrates every title, and is
    /// capped at a page — so a client filtering its response would filter the truncated
    /// page and silently miss older matches.
    ///
    /// Unbounded on purpose. It is one `TEXT` column per adopted session of one agent, and
    /// a `LIMIT` here would make an already-adopted row read as adoptable past the cap —
    /// which is the `409`-nobody-can-act-on this exists to prevent, reappearing only for
    /// users with enough history to hit it.
    pub async fn adopted_native_ids(
        &self,
        environment: &sylph_protocol::EnvironmentId,
        agent: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT native_id FROM sessions WHERE environment = ? AND agent = ?")
                .bind(environment.as_str())
                .bind(agent)
                .fetch_all(&self.pool)
                .await
                .map_err(Error::Database)?;
        Ok(rows.into_iter().map(|(native,)| native).collect())
    }

    /// List sessions newest-activity-first, narrowed by `filter`.
    ///
    /// Filtering belongs in SQL rather than in the client: the query is capped
    /// by `LIMIT`, so a client filtering the response would only ever filter
    /// the truncated page and would silently miss older matches.
    pub async fn list_sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>> {
        // Every predicate is a no-op when its bound value is NULL, which keeps
        // one prepared statement covering all filter combinations.
        let mut sessions = sqlx::query_as::<_, Session>(
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
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)?;
        for session in &mut sessions {
            self.hydrate_session_title(session).await?;
        }
        Ok(sessions)
    }

    /// Fill in a title from the session's opening ask, when it has none.
    ///
    /// The rule lives in [`crate::db::labels`] rather than here, because it is a
    /// fact about transcripts rather than about this table: the first `user` event
    /// is usually *not* what the user typed, and skipping the agent's injected
    /// envelopes is what raised recognizable labels from ~10% to 98% over 311
    /// measured sessions.
    async fn hydrate_session_title(&self, session: &mut Session) -> Result<()> {
        if session
            .title
            .as_deref()
            .is_some_and(|title| !title.trim().is_empty())
        {
            return Ok(());
        }
        let Some(text) = sqlx::query_scalar::<_, String>(
            "SELECT text FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
                AND kind = 'user'
              ORDER BY seq LIMIT 1",
        )
        .bind(session.id.environment.as_str())
        .bind(session.id.agent.as_str())
        .bind(session.id.native_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)?
        else {
            return Ok(());
        };
        let title = summarize_line(&text);
        if title.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE sessions SET title = ?
              WHERE environment = ? AND agent = ? AND native_id = ?
                AND (title IS NULL OR trim(title) = '')",
        )
        .bind(&title)
        .bind(session.id.environment.as_str())
        .bind(session.id.agent.as_str())
        .bind(session.id.native_id.as_str())
        .execute(&self.pool)
        .await?;
        session.title = Some(title);
        Ok(())
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

    // Turn operations
    pub async fn create_turn(&self, turn: &Turn) -> Result<Turn> {
        sqlx::query_as::<_, Turn>(
            r#"
            INSERT INTO turns (
                id, session_env, session_agent, session_native, status, origin,
                origin_subject, request_text, command_invocation_id, idempotency_key, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&turn.id)
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
        .bind(&turn.status)
        .bind(&turn.origin)
        .bind(&turn.origin_subject)
        .bind(&turn.request_text)
        .bind(&turn.command_invocation_id)
        .bind(&turn.idempotency_key)
        .bind(turn.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Insert a turn and claim the session's running slot atomically, answering `None`
    /// when another turn already holds it.
    ///
    /// **The database is the compare-and-set boundary**, which is what distinguishes this
    /// from [`Self::create_turn`]'s unconditional insert. There is no process-local mutex
    /// available to serialize against: every turn arrives over the bridge from an
    /// envagent, so the concurrent writers are HTTP handlers in this process *and*
    /// whatever else is addressing the same session, and only the store sees both.
    pub async fn create_turn_claiming_slot(&self, turn: &Turn) -> Result<Option<Turn>> {
        let mut tx = self.pool.begin().await.map_err(Error::Database)?;
        let active: Option<String> = sqlx::query_scalar(
            "SELECT id FROM turns WHERE session_env = ? AND session_agent = ? AND session_native = ? \
             AND status IN ('queued','starting','running','stopping','settling') LIMIT 1",
        )
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::Database)?;
        if active.is_some() {
            tx.rollback().await.map_err(Error::Database)?;
            return Ok(None);
        }
        let stored = sqlx::query_as::<_, Turn>(
            r#"INSERT INTO turns (
                id, session_env, session_agent, session_native, status, origin,
                origin_subject, request_text, command_invocation_id, idempotency_key, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *"#,
        )
        .bind(&turn.id)
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
        .bind(&turn.status)
        .bind(&turn.origin)
        .bind(&turn.origin_subject)
        .bind(&turn.request_text)
        .bind(&turn.command_invocation_id)
        .bind(&turn.idempotency_key)
        .bind(turn.created_at_ms)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::Database)?;
        sqlx::query(
            "UPDATE sessions SET status = 'running', updated_at_ms = ? \
             WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(now_ms())
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
        tx.commit().await.map_err(Error::Database)?;
        Ok(Some(stored))
    }

    pub async fn get_turn(&self, id: &str) -> Result<Option<Turn>> {
        sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    pub async fn list_turns(&self, session_id: &SessionId, limit: i64) -> Result<Vec<Turn>> {
        sqlx::query_as::<_, Turn>(
            "SELECT * FROM turns
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
              ORDER BY created_at_ms DESC LIMIT ?",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    // Event operations
    pub async fn create_event(&self, event: &Event) -> Result<Event> {
        sqlx::query_as::<_, Event>(
            r#"
            INSERT INTO events (
                session_env, session_agent, session_native, turn_id, seq, kind,
                phase, stream_id, span, interior_state, name, text, data_json, is_error,
                stability, source, arrival_at_ms, recorded_at_ms, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
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
        .bind(&event.source)
        .bind(event.arrival_at_ms)
        .bind(event.recorded_at_ms)
        .bind(event.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_events(
        &self,
        session_id: &SessionId,
        turn_id: Option<&str>,
        after_seq: Option<i32>,
    ) -> Result<Vec<Event>> {
        let query = match (turn_id, after_seq) {
            (Some(tid), Some(seq)) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events
                      WHERE session_env = ? AND session_agent = ? AND session_native = ?
                        AND turn_id = ? AND seq > ?
                      ORDER BY seq",
                )
                .bind(session_id.environment.as_str())
                .bind(session_id.agent.as_str())
                .bind(session_id.native_id.as_str())
                .bind(tid)
                .bind(seq)
            }
            (Some(tid), None) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events
                      WHERE session_env = ? AND session_agent = ? AND session_native = ? AND turn_id = ?
                      ORDER BY seq",
                )
                .bind(session_id.environment.as_str())
                .bind(session_id.agent.as_str())
                .bind(session_id.native_id.as_str())
                .bind(tid)
            }
            (None, Some(seq)) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events
                      WHERE session_env = ? AND session_agent = ? AND session_native = ? AND seq > ?
                      ORDER BY seq",
                )
                .bind(session_id.environment.as_str())
                .bind(session_id.agent.as_str())
                .bind(session_id.native_id.as_str())
                .bind(seq)
            }
            (None, None) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events
                      WHERE session_env = ? AND session_agent = ? AND session_native = ?
                      ORDER BY seq",
                )
                .bind(session_id.environment.as_str())
                .bind(session_id.agent.as_str())
                .bind(session_id.native_id.as_str())
            }
        };

        query.fetch_all(&self.pool).await.map_err(Error::Database)
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

    /// Set the opening title once, without changing activity ordering. Called when a turn
    /// starts, so a row is recognisable from its first message — most agents record no title
    /// of their own (`docs/agents.md`).
    pub async fn set_session_title_if_empty(&self, id: &SessionId, title: &str) -> Result<()> {
        let title = summarize_line(title);
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

    /// Replace a session's runner flags.
    ///
    /// Flags were write-once until the composer needed to change model and
    /// reasoning effort mid-session. They are stored whole rather than merged
    /// per key: the caller has already validated the complete set against the
    /// agent's declared schema, and a merge here would let a stale key survive a
    /// change the user believed was total.
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

    /// Replace one persisted launch flag without dropping settings written by another
    /// control. The caller supplies an adapter-approved key/value pair; keeping the
    /// read/merge here gives every runtime config endpoint one update rule.
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

    // Turn lifecycle

    /// Look up a prior turn with the same idempotency key.
    ///
    /// Turn creation is retried by clients on network failure, so a repeat of
    /// the same key must return the original turn rather than starting a second
    /// agent process.
    pub async fn find_turn_by_idempotency_key(
        &self,
        session_id: &SessionId,
        key: &str,
    ) -> Result<Option<Turn>> {
        sqlx::query_as::<_, Turn>(
            "SELECT * FROM turns
              WHERE session_env = ? AND session_agent = ? AND session_native = ? AND idempotency_key = ?",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Claim a queued turn for execution, returning false if it is no longer
    /// queued. The compare-and-set keeps two supervisors from starting the same
    /// turn twice.
    pub async fn mark_turn_running(&self, turn_id: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'running', started_at_ms = ?
            WHERE id = ? AND status IN ('queued', 'starting')
            "#,
        )
        .bind(now_ms())
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Claim a queued turn and its session-running marker in one transaction.
    ///
    /// Readers use the turn status as the signal that work has begun. Updating
    /// the two rows in separate statements lets a reader observe `running` on
    /// the turn while the session still says `idle`, which is an impossible
    /// state for the API and was exposed by restart/lifecycle polling. The
    /// compare-and-set remains on the turn row; the session update is part of
    /// the same commit, so either both facts become visible or neither does.
    pub async fn claim_turn_running(&self, turn_id: &str, session_id: &SessionId) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'running', started_at_ms = ?
            WHERE id = ? AND session_env = ? AND session_agent = ? AND session_native = ?
              AND status IN ('queued', 'starting')
            "#,
        )
        .bind(now_ms())
        .bind(turn_id)
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE sessions SET status = 'running', updated_at_ms = ? \
             WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(now_ms())
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn mark_turn_stopping(&self, turn_id: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'stopping', stopped_at_ms = ?
            WHERE id = ? AND status IN ('queued', 'starting', 'running')
            "#,
        )
        .bind(now_ms())
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Claim a live turn for settling, recording the outcome it settled with.
    ///
    /// This is the **first** half of settling a live turn; [`Self::promote_settled_turn`]
    /// is the second, and the settle sweep runs between them. The pair exists because
    /// `status` answers two questions a client cannot otherwise separate — *what did this
    /// turn produce*, and *is its record finished* — and one column reporting both meant a
    /// reader between the two writes saw a terminal turn over a record still being written.
    /// `crate::supervisor::TurnCloser` owns the ordering and states the cost.
    ///
    /// So the outcome lands here, under a non-terminal status, and the status becomes
    /// terminal only once the record is complete. A reader therefore never observes a
    /// terminal turn whose sweep has not run.
    ///
    /// `completed_at_ms` is stamped **here**, not at promotion: the turn ended when its
    /// outcome arrived, and charging the sweep's duration to the turn would report a
    /// duration nobody spent.
    ///
    /// # Why this is the first-writer-wins guard
    ///
    /// The `status NOT IN (...)` guard is what makes a settled turn final, and it is why
    /// the claim rather than the promotion is the guard: two settlements race by design.
    /// `stop` interrupts the agent and records `cancelled`, and the agent then emits its own
    /// `turn` event, which arrives a moment later and would otherwise overwrite that with
    /// `completed`. The user asked for a stop and got one, so reporting the turn as having
    /// completed normally is a lie about what happened.
    ///
    /// `settling` is in the guard alongside the four terminal statuses. Without it two
    /// racing callers would both claim, both sweep, and both emit a close for every
    /// trajectory the turn left open — so the guard is what keeps the sweep single-shot as
    /// well as the outcome.
    ///
    /// Returns whether this call was the one that claimed it, so a caller can tell "I am
    /// settling this" from "someone already is".
    ///
    /// # Why the outcome is not stored for the promotion to read
    ///
    /// The status the turn will end at is written by [`Self::promote_settled_turn`], which
    /// takes it as an argument rather than reading a column this call left behind. A
    /// `settling_status` column would be a second field holding one fact, live only
    /// between two calls in a single function body — and the one caller already holds the
    /// value, from the same `Outcome::as_status` binding it passes here.
    pub async fn begin_settling(
        &self,
        turn_id: &str,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'settling', completed_at_ms = ?,
                error_code = ?, error_message = ?
            WHERE id = ?
              AND status NOT IN
                ('settling', 'completed', 'failed', 'cancelled', 'interrupted')
            "#,
        )
        .bind(now_ms())
        .bind(error_code)
        .bind(error_message)
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Publish a swept turn's outcome, ending the settle.
    ///
    /// The second half of [`Self::begin_settling`], and the write every client's "is this
    /// finished" question turns on: after it the turn's record does not change again.
    ///
    /// Guarded on `settling` so it can only follow a claim, which is what makes the two a
    /// pair rather than two independent writers of the same column. It also means a
    /// promotion cannot resurrect a turn `interrupt_stale_turns` settled while the sweep
    /// was in flight — that turn is already terminal, so this reports `false` and the
    /// restart's account of it stands.
    pub async fn promote_settled_turn(&self, turn_id: &str, status: &str) -> Result<bool> {
        let result =
            sqlx::query("UPDATE turns SET status = ? WHERE id = ? AND status = 'settling'")
                .bind(status)
                .bind(turn_id)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Record a terminal turn state at a stated time, in one write.
    ///
    /// Same first-writer-wins guard as its siblings, so a terminal state stays final.
    ///
    /// # Why this settles in one write and a live turn does not
    ///
    /// A live turn settles in two — [`Self::begin_settling`], then
    /// [`Self::promote_settled_turn`] — because the server has work to do between knowing
    /// the outcome and having written the record, and a client must not see a terminal turn
    /// in between.
    ///
    /// There is no such gap here. This is `crate::db::ingest` reading a vendor's own store
    /// for a turn that ended before Sylph looked, so nothing is left open to sweep and no
    /// client is streaming it. Routing it through the two-phase pair would add a state the
    /// turn is never really in.
    ///
    /// # Why the timestamp is a parameter
    ///
    /// A live turn ends *now*, so [`Self::begin_settling`] reading the clock is right. An
    /// **adopted** turn ended whenever the vendor recorded it, possibly months ago, and
    /// stamping it `now()` would report every adopted turn as having finished at the moment
    /// somebody clicked adopt — giving a whole transcript one timestamp, a duration of zero,
    /// and a position at the top of every recency-ordered listing.
    pub async fn finish_turn_at(
        &self,
        turn_id: &str,
        status: &str,
        error_code: Option<&str>,
        error_message: Option<&str>,
        completed_at_ms: i64,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = ?, completed_at_ms = ?,
                error_code = ?, error_message = ?
            WHERE id = ?
              AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
            "#,
        )
        .bind(status)
        .bind(completed_at_ms)
        .bind(error_code)
        .bind(error_message)
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Close an adopted turn, recording both of its stamps from the vendor's own record.
    ///
    /// # Why this is not [`Self::finish_turn_at`] plus [`Self::mark_turn_running`]
    ///
    /// `mark_turn_running` requires a `queued` or `starting` status and stamps
    /// `started_at_ms` with `now()`. An adopted turn is neither — `crate::db::ingest`
    /// creates it `running`, because there is no queue and nothing to start — and `now()`
    /// is the wrong value twice over: the turn began whenever the vendor recorded its
    /// first event, and stamping it now gives every adopted turn in a months-old
    /// transcript the same start.
    ///
    /// So both stamps are known at close time and neither is the clock's. One statement
    /// rather than two, so a reader never observes a turn with an end and no beginning —
    /// which is the state a duration is computed from, and computing one from a null start
    /// is how `AGENTS.md`'s publishing-gap defect presented.
    ///
    /// Same first-writer-wins guard as its siblings, so a terminal state stays final.
    pub async fn close_adopted_turn(
        &self,
        turn_id: &str,
        status: &str,
        started_at_ms: i64,
        completed_at_ms: i64,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = ?, started_at_ms = ?, completed_at_ms = ?
            WHERE id = ?
              AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
            "#,
        )
        .bind(status)
        .bind(started_at_ms)
        .bind(completed_at_ms)
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Fail every turn left mid-flight by a previous process.
    ///
    /// Child processes do not survive a backend restart, so any turn still
    /// marked live in the journal is unrecoverable and must be reported as
    /// interrupted rather than appearing to hang forever.
    ///
    /// # `settling` is in scope, and what that costs
    ///
    /// A crash between [`Self::begin_settling`] and [`Self::promote_settled_turn`] leaves a
    /// turn whose outcome is known and whose record is unfinished. Left out of this sweep it
    /// would stay `settling` forever, which is the spinning card this function exists to
    /// prevent — so it is swept, and the sweep's own account replaces the outcome the claim
    /// recorded. That is a real loss of precision: a turn that had completed is reported
    /// `interrupted`.
    ///
    /// It is accepted rather than engineered around. Recovering the outcome would mean
    /// persisting it in a column read only by this function, in a window a few milliseconds
    /// wide that only a crash can land in — and `interrupted` with this message is a *true*
    /// statement about what happened, not a placeholder standing in for one.
    ///
    /// What is **not** accepted is losing the turn's end time. `completed_at_ms` is
    /// `COALESCE`d, so a `settling` turn keeps the stamp its claim recorded and only a turn
    /// that never ended is stamped with the restart. Overwriting it would give a turn that
    /// finished before the crash a duration measured to whenever the backend came back —
    /// hours, for an overnight restart — which is `AGENTS.md`'s publishing-gap defect with a
    /// new cause.
    ///
    /// The trajectories such a turn left open are closed by
    /// [`Self::close_stale_trajectories`], which the caller runs next.
    pub async fn interrupt_stale_turns(&self) -> Result<u64> {
        let mut tx = self.pool.begin().await?;

        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'interrupted',
                completed_at_ms = COALESCE(completed_at_ms, ?),
                error_code = 'TURN_INTERRUPTED',
                error_message = 'Backend restarted while this turn was running'
            WHERE status IN ('queued', 'starting', 'running', 'stopping', 'settling')
            "#,
        )
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;

        // Sessions cannot be running if no turn is.
        sqlx::query("UPDATE sessions SET status = 'idle' WHERE status = 'running'")
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(result.rows_affected())
    }

    /// Close every trajectory left open by a turn that died without settling.
    ///
    /// The normal path is `TurnSupervisor::settle`, which emits these closes before
    /// scheduling consolidation. A crash runs neither: `interrupt_stale_turns` marks
    /// the turn interrupted, which makes it *consolidatable*, and consolidation then
    /// folds a `subagent` `started` that has no text and no close into no block at
    /// all — so the trajectory disappears from history while its children survive,
    /// and they reattach to the root on the next read. The delegation is gone and
    /// its work is misattributed, both permanently.
    ///
    /// Written straight to the journal because there is no hub and no client at
    /// startup. Consolidation reads the journal, so a row is all that is needed for
    /// history to come out right; anyone who later streams the session reads these
    /// from history like any other event.
    ///
    /// Returns how many were closed.
    pub async fn close_stale_trajectories(&self) -> Result<u64> {
        let turns = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT turn_id FROM events
              WHERE turn_id IS NOT NULL AND (kind = 'subagent' OR span IS NOT NULL)",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)?;

        let mut closed = 0u64;
        for turn_id in turns {
            // Backgrounded work included, unlike the turn-settle sweep. That exception
            // holds a delegation open because the agent will report back; here the
            // agent is gone, so nothing will, and leaving it open trades one wrong
            // record for a card that spins forever.
            let open = self
                .open_trajectories_including_backgrounded(&turn_id)
                .await?;
            if open.is_empty() {
                continue;
            }

            let Some((session_id, next_seq)) = self.turn_tail(&turn_id).await? else {
                continue;
            };

            for (offset, (stream_id, span, name)) in open.into_iter().enumerate() {
                let event = Event {
                    // Assigned by the insert; `append_event` does not read it.
                    id: None,
                    seq: next_seq + offset as i32,
                    session_id: session_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    kind: "subagent".to_string(),
                    phase: Some("completed".to_string()),
                    stream_id: Some(stream_id),
                    span,
                    interior_state: None,
                    name,
                    text: String::new(),
                    // The same shape `settle` emits, so a client cannot tell which
                    // path closed a trajectory — and `abandoned` stays distinct from
                    // `failed`, because nobody reported an outcome here either.
                    data_json: r#"{"closedBy":"server","reason":"backend_restarted"}"#.to_string(),
                    is_error: true,
                    stability: "stable".to_string(),
                    source: "server".to_string(),
                    arrival_at_ms: now_ms(),
                    recorded_at_ms: None,
                    created_at_ms: now_ms(),
                };
                self.append_event(&event).await?;
                closed += 1;
            }
        }

        Ok(closed)
    }

    /// A turn's session, and the next free sequence number *in that session*.
    ///
    /// Session-scoped and not turn-scoped, because `events` is
    /// `UNIQUE(session_id, seq)`: a later turn on the same session already holds
    /// higher numbers, so continuing from this turn's own maximum would collide with
    /// it and the insert would fail. Appending at the session tail puts the close
    /// after the delegation it closes, which is what a reader needs.
    async fn turn_tail(&self, turn_id: &str) -> Result<Option<(SessionId, i32)>> {
        let Some(turn) = self.get_turn(turn_id).await? else {
            return Ok(None);
        };
        let session_id = turn.session_id;

        let max_seq = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(seq) FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(Some((session_id, max_seq.unwrap_or(0) + 1)))
    }

    pub async fn count_turns_by_status(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<(String, i64)>> {
        // One prepared statement for both the whole-journal and per-session cases:
        // every predicate is a no-op when its bound value is NULL.
        sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT status, COUNT(*) FROM turns
            WHERE (?1 IS NULL OR session_env = ?1)
              AND (?2 IS NULL OR session_agent = ?2)
              AND (?3 IS NULL OR session_native = ?3)
            GROUP BY status
            "#,
        )
        .bind(session_id.map(|id| id.environment.as_str()))
        .bind(session_id.map(|id| id.agent.as_str()))
        .bind(session_id.map(|id| id.native_id.as_str()))
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn count_sessions_by_status(&self) -> Result<Vec<(String, i64)>> {
        sqlx::query_as::<_, (String, i64)>("SELECT status, COUNT(*) FROM sessions GROUP BY status")
            .fetch_all(&self.pool)
            .await
            .map_err(Error::Database)
    }

    // Event journal

    /// Highest sequence number recorded for a session, or -1 when empty.
    ///
    /// Used to seed the in-memory sequence counter after a restart so new
    /// events never collide with journaled ones.
    pub async fn max_event_seq(&self, session_id: &SessionId) -> Result<i32> {
        let seq: Option<i32> = sqlx::query_scalar(
            "SELECT MAX(seq) FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(seq.unwrap_or(-1))
    }

    /// Append one event to the journal, in its own transaction.
    ///
    /// Both stable and ephemeral events belong here. Ephemeral ones are what
    /// consolidation folds into a block's offset and timing arrays, so dropping
    /// them would leave the fold nothing to fold and make `docs/storage.md`'s
    /// lossless-replay guarantee unreachable. They are short-lived: consolidation
    /// replaces them with the folded form and deletes the rows.
    ///
    /// Prefer [`Self::append_events`] for a run of ephemeral events. One
    /// transaction per delta is thousands of fsyncs per turn, and fsync count is
    /// what actually costs on the hot path.
    pub async fn append_event(&self, event: &Event) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO events (
                session_env, session_agent, session_native, turn_id, seq, kind,
                phase, stream_id, span, interior_state, name, text, data_json, is_error,
                stability, source, arrival_at_ms, recorded_at_ms, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
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
        .bind(&event.source)
        .bind(event.arrival_at_ms)
        .bind(event.recorded_at_ms)
        .bind(event.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append many events in one transaction.
    ///
    /// This is the single largest lever on the hot path (`docs/storage.md`): a 2 KB
    /// assistant message arrives as ~400 deltas, and committing each one separately
    /// makes ~400 fsyncs out of what can be one. Batching converts thousands of tiny
    /// transactions per turn into tens.
    ///
    /// All-or-nothing per batch, which is the honest granularity: a partially
    /// applied window would leave gaps in the middle of a block's delta run, and a
    /// fold over those would reconstruct a stream that never happened. Losing a
    /// whole trailing window instead reconstructs a turn that was visibly cut off,
    /// which is a state the replayer already handles.
    pub async fn append_events(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        for event in events {
            sqlx::query(
                r#"
                INSERT INTO events (
                    session_env, session_agent, session_native, turn_id, seq, kind,
                    phase, stream_id, span, interior_state, name, text, data_json, is_error,
                    stability, source, arrival_at_ms, recorded_at_ms, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
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
            .bind(&event.source)
            .bind(event.arrival_at_ms)
            .bind(event.recorded_at_ms)
            .bind(event.created_at_ms)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Highest applied migration version, or 0 on an unmigrated journal.
    ///
    /// Reported by `/health` so a client can detect that it is talking to a
    /// backend whose journal shape it does not understand.
    ///
    /// # Why this reads `_sqlx_migrations` rather than a stored key
    ///
    /// **A number the migration runner already maintains must not have a second
    /// hand-written copy.** A hand-bumped `app_meta.schema_version` drifts the first time a
    /// migration forgets to bump it, and it drifts *silently* because nothing compares the
    /// two — so the one question this field exists to answer, whether a client understands
    /// this journal's shape, is answered wrongly with nothing reporting it.
    pub async fn schema_version(&self) -> Result<i64> {
        let value: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success")
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        Ok(value.unwrap_or(0))
    }

    /// Journaled events for one turn after `after_seq`, in sequence order.
    /// A turn's *stable* journal rows: what a transcript renders.
    ///
    /// Deltas are journaled too — consolidation needs them to fold — but they are
    /// excluded here, because a `completed` event already carries its block's whole
    /// text. Including both would render every token twice.
    /// Delegated trajectories this turn opened and never closed, oldest first.
    ///
    /// One left open means the adapter stopped reporting it — its link died, its
    /// vendor went quiet, the turn was interrupted mid-delegation — and the server
    /// closes it on the turn's behalf.
    ///
    /// **What counts as a trajectory here mirrors the protocol's definition, not
    /// the `subagent` kind.** This query used to match `phase = 'started'` on a
    /// `subagent` row, and both halves of that were too narrow. A trajectory opened
    /// by a `delta` — which is every `claude` workflow delegate, since its first
    /// notification is progress rather than a start — was invisible to this sweep,
    /// as was a promoted `tool_call` (`docs/session-protocol.md` § Promotion). The
    /// consequence is the exact spinner this function exists to prevent, reached
    /// through a door it did not check: measured on a real session, two workflow
    /// cards read *running* permanently, across reloads, because nothing ever
    /// closed them and nothing ever would.
    ///
    /// So the set is every id that is **declared** (any `subagent` event names it as
    /// its `stream_id`) or **referenced** (any event names it as its `span`), minus
    /// every id already closed. A promoted trajectory's close is its own
    /// `tool_result`, because a `tool_call`'s `completed` phase reports that its
    /// input finished streaming and says nothing about the delegated work.
    ///
    /// The alternative is a spinner that never resolves, and worse, a subtree the
    /// UI must report as *running* forever because nothing ever said otherwise.
    /// That is the same reasoning that makes a pending request `abandoned` rather
    /// than left pending: a recorded failure explains a gap, and silence does not.
    ///
    /// **Backgrounded work is excluded, because for it the reasoning inverts.** A
    /// delegation the agent says runs in the background is *expected* to outlive its
    /// turn and to report its own outcome afterwards, on an agent-initiated turn. It
    /// is not a gap that silence has to explain; it is work still running. Closing it
    /// as abandoned writes a contradiction its own later completion cannot undo —
    /// both records then exist, on different turns, and the earlier card still says
    /// the work never finished when it demonstrably did. Silence is the honest answer
    /// here, and it is bounded three ways: the agent reports back and
    /// `crate::supervisor::reconcile` records that on the turn that opened the
    /// trajectory; a crash is caught by
    /// [`Self::open_trajectories_including_backgrounded`] on the next start, which
    /// deliberately does *not* honour the flag; and a trajectory that reached history
    /// still open is closed there by
    /// `crate::storage::HistoryStore::close_running_trajectories`.
    ///
    /// Leaving it open is only safe because consolidation *preserves* an open
    /// trajectory rather than folding it into nothing. It did not, and the two changes
    /// together are what make the exception honest: on its own, skipping the sweep
    /// deleted the delegation from the dispatching turn instead of misdescribing it.
    ///
    /// **A turn may only close a trajectory it actually holds.** A trajectory id is
    /// session-scoped, not turn-scoped, so a *late* event can name one that an earlier
    /// turn opened — which is exactly what backgrounded work does when it reports its
    /// outcome on an agent-initiated turn. A bare `span` reference used to promote that
    /// id into a trajectory *on the receiving turn*, where nothing had opened it and
    /// nothing ever would, so the sweep closed it as abandoned. Measured: a `Workflow`
    /// host that had already completed cleanly on the dispatching turn acquired a
    /// second, fabricated `abandoned` close on the agent's turn — a record of a failure
    /// that never happened, on a turn the host never ran in, escalating a card that had
    /// done nothing wrong.
    ///
    /// So a candidate survives only if some row *on this turn* carries it as its
    /// `stream_id`. That is precisely what opening one means, by either route: a
    /// declaring `subagent` row, or a `tool_call` this turn's own children name as
    /// their span (§ Promotion).
    ///
    /// The narrower rule — "skip it only when another turn on this session declared it"
    /// — was tried first and cannot work, which is worth recording because it is the
    /// more obvious design. The evidence it needs does not survive: consolidation moves
    /// the earlier turn's rows into `history.db` and deletes them here, so by the time
    /// a late outcome arrives the journal no longer holds anything saying which turn
    /// owned the host, and a second DB cannot be reached from this query. The rule above
    /// needs no such evidence — it asks only about the turn being swept.
    ///
    /// It also stops the sweep inventing a close for a `span` that names nothing
    /// anywhere. That is the right answer for the same reason: a client already renders
    /// an unknown `span` at the root (§ Delegated trajectories), whereas a server-authored
    /// close for an id nothing opened conjures a node whose only phase is its own ending.
    ///
    /// Returns `(stream_id, span, name)` — the parent and name are needed to emit a
    /// closing event that lands in the right place in the tree.
    pub async fn open_trajectories(
        &self,
        turn_id: &str,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        self.open_trajectories_inner(turn_id, true).await
    }

    /// Every open trajectory, *including* work declared backgrounded.
    ///
    /// The exclusion above rests on one premise: the agent is still alive and will
    /// report back. At startup that premise is false — no link survives a restart, so a
    /// backgrounded task inside a dead process can no more report than an abandoned one
    /// can. Honouring the flag there would leave the work open forever, which is the
    /// permanent spinner the sweep exists to prevent, reached by way of the exception
    /// that exists to prevent a wrong record.
    ///
    /// So the flag is respected exactly while it means something, and the caller says
    /// which situation it is in rather than this query guessing.
    pub async fn open_trajectories_including_backgrounded(
        &self,
        turn_id: &str,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        self.open_trajectories_inner(turn_id, false).await
    }

    async fn open_trajectories_inner(
        &self,
        turn_id: &str,
        respect_background_flag: bool,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            // `open.id` is the trajectory; `seq` orders the sweep by when each was
            // first seen; `span` and `name` are carried so the closing event lands
            // in the right place in the tree, taken from the row that knows them —
            // a declaring `subagent` row when there is one, since a promoted host's
            // own `span` is its grandparent and its `name` is the tool's.
            "WITH open AS (
                 SELECT stream_id AS id,
                        MIN(seq) AS seq,
                        -- MAX ignores NULLs, so a phase that carried the value
                        -- wins over one that did not, whatever order they arrived.
                        MAX(CASE WHEN kind = 'subagent' THEN span END) AS declared_span,
                        MAX(CASE WHEN kind = 'subagent' THEN name END) AS declared_name,
                        MAX(span) AS any_span,
                        MAX(name) AS any_name
                   FROM events
                  WHERE turn_id = ?1 AND kind = 'subagent' AND stream_id IS NOT NULL
                  GROUP BY stream_id
                 UNION ALL
                 SELECT span AS id, MIN(seq) AS seq,
                        NULL, NULL, NULL, NULL
                   FROM events
                  WHERE turn_id = ?1 AND span IS NOT NULL
                  GROUP BY span
             )
             SELECT open.id,
                    MAX(COALESCE(open.declared_span, open.any_span)),
                    MAX(COALESCE(open.declared_name, open.any_name))
               FROM open
              WHERE NOT EXISTS (
                    SELECT 1 FROM events done
                     WHERE done.turn_id = ?1
                       AND done.stream_id = open.id
                       AND done.phase = 'completed'
                       AND done.kind IN ('subagent', 'tool_result')
              )
              -- Backgrounded work is still running, not abandoned. Matched on the
              -- adapter's own flag, on *any* phase of the trajectory: the vendor
              -- reports it once, at the start, so reading only the latest row would
              -- un-background the very work the flag protects.
              --
              -- Skipped entirely when the caller says the agent is gone, because then
              -- nothing can report and silence explains nothing.
              AND (NOT ?2 OR NOT EXISTS (
                    SELECT 1 FROM events bg
                     WHERE bg.turn_id = ?1
                       AND bg.stream_id = open.id
                       AND bg.kind = 'subagent'
                       AND json_extract(bg.data_json, '$.is_backgrounded') = 1
              ))
              -- This turn must actually hold the trajectory, not merely mention it.
              -- A bare `span` with no row of its own here is a reference *outward*:
              -- most often to a trajectory an earlier turn opened, which is what a
              -- late outcome carries.
              AND EXISTS (
                    SELECT 1 FROM events mine
                     WHERE mine.turn_id = ?1
                       AND mine.stream_id = open.id
              )
              GROUP BY open.id
              ORDER BY MIN(open.seq)",
        )
        .bind(turn_id)
        .bind(respect_background_flag)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(rows)
    }

    pub async fn turn_events_after(&self, turn_id: &str, after_seq: i32) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events
              WHERE turn_id = ? AND seq > ? AND stability = 'stable'
              ORDER BY seq",
        )
        .bind(turn_id)
        .bind(after_seq)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Every journaled row for one session, ephemeral included.
    ///
    /// **Deliberately not filtered by stability**, which is what separates it from
    /// [`Self::turn_events_after`]. That one answers "what should a reader see", and
    /// ephemeral rows are excluded because a `delta` is superseded by the block it belongs
    /// to. This one answers "what has this session already been told", which a re-read needs
    /// (`crate::db::ingest::NativeIngest::reread`): an ephemeral row a previous pass
    /// journaled is still a row, and omitting it made a re-read re-add every `turn`/`started`
    /// on every click — one duplicate per pass, in the tier a reader's transcript is
    /// assembled from.
    ///
    /// One query for the session rather than one per turn, because a long session has many
    /// turns and the per-turn shape costs a round trip each.
    pub async fn session_events(&self, session_id: &SessionId) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
              ORDER BY seq",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Record a request an agent raised, so it survives every client leaving.
    pub async fn create_pending_request(&self, request: &PendingRequest) -> Result<PendingRequest> {
        sqlx::query_as::<_, PendingRequest>(
            r#"
            INSERT INTO pending_requests (
                id, session_env, session_agent, session_native, turn_id,
                adapter_request_id, kind, tool, tool_use_id, title, detail_json,
                options_json, form_json, default_option, risk, allow_persistent,
                expires_at_ms, status, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)
            RETURNING *
            "#,
        )
        .bind(&request.id)
        .bind(request.session_id.environment.as_str())
        .bind(request.session_id.agent.as_str())
        .bind(request.session_id.native_id.as_str())
        .bind(&request.turn_id)
        .bind(&request.adapter_request_id)
        .bind(&request.kind)
        .bind(&request.tool)
        .bind(&request.tool_use_id)
        .bind(&request.title)
        .bind(&request.detail_json)
        .bind(&request.options_json)
        .bind(&request.form_json)
        .bind(&request.default_option)
        .bind(&request.risk)
        .bind(request.allow_persistent)
        .bind(request.expires_at_ms)
        .bind(request.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// What a session is currently waiting on, oldest first.
    pub async fn open_requests(&self, session_id: &SessionId) -> Result<Vec<PendingRequest>> {
        sqlx::query_as::<_, PendingRequest>(
            "SELECT * FROM pending_requests
              WHERE session_env = ? AND session_agent = ? AND session_native = ? AND status = 'pending'
              ORDER BY created_at_ms",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_request(&self, id: &str) -> Result<Option<PendingRequest>> {
        sqlx::query_as::<_, PendingRequest>("SELECT * FROM pending_requests WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    /// Claim a request for resolution. `None` when it was already answered.
    ///
    /// The `status = 'pending'` predicate is the whole concurrency control: two
    /// clients answering at once both run this UPDATE, and SQLite serializes
    /// them, so exactly one sees a row. Reading the status first and then writing
    /// would leave a window where both observe `pending` and both forward an
    /// answer to the adapter — which for an approval means the second overwrites
    /// the first's decision.
    pub async fn resolve_request(
        &self,
        id: &str,
        option: &str,
        input_json: Option<&str>,
        resolved_by: Option<&str>,
    ) -> Result<Option<PendingRequest>> {
        sqlx::query_as::<_, PendingRequest>(
            r#"
            UPDATE pending_requests
               SET status = 'resolved',
                   chosen_option = ?,
                   resolved_input_json = ?,
                   resolved_by = ?,
                   resolved_at_ms = ?
             WHERE id = ? AND status = 'pending'
            RETURNING *
            "#,
        )
        .bind(option)
        .bind(input_json)
        .bind(resolved_by)
        .bind(now_ms())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Record a trajectory's real outcome on the turn that owns it.
    ///
    /// The journal-tier half of the operation `crate::supervisor::reconcile` documents
    /// — read there for why a late outcome amends an earlier record instead of being
    /// moved onto it, and which records are provisional enough to amend.
    ///
    /// Selected by `(session_id, stream_id)` and *not* by turn, which is the whole
    /// point: a trajectory id is session-scoped, so the turn holding the record is not
    /// the turn the outcome arrived on. `turn_id <> ?` excludes the arriving turn so
    /// the late close cannot amend itself.
    pub async fn reconcile_trajectory(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        except_turn_id: &str,
        outcome: &crate::supervisor::TrajectoryOutcome<'_>,
    ) -> Result<bool> {
        let Some(row) = sqlx::query_as::<_, Event>(
            "SELECT * FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
                AND stream_id = ? AND turn_id <> ?
                AND kind = 'subagent'
              ORDER BY seq DESC LIMIT 1",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .bind(stream_id)
        .bind(except_turn_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)?
        else {
            return Ok(false);
        };

        let mut data: Option<serde_json::Value> = data_from_column(&row.data_json);
        if !outcome.supersedes(row.phase.as_deref(), data.as_ref()) {
            return Ok(false);
        }

        let mut phase = row.phase.clone();
        let mut text = row.text.clone();
        let mut is_error = row.is_error;
        let mut source = row.source.clone();
        outcome.apply(&mut phase, &mut text, &mut is_error, &mut data, &mut source);

        let result = sqlx::query(
            "UPDATE events
                SET phase = ?, text = ?, is_error = ?, source = ?, data_json = ?
              WHERE session_env = ? AND session_agent = ? AND session_native = ? AND seq = ?",
        )
        .bind(&phase)
        .bind(&text)
        .bind(is_error)
        .bind(&source)
        .bind(
            data.as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "{}".to_string()),
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .bind(row.seq)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(result.rows_affected() > 0)
    }

    /// Mark every open request on a session abandoned, returning how many.
    ///
    /// Called when a link dies with requests outstanding. They can never be
    /// answered — the adapter waiting on them is gone — so leaving them `pending`
    /// would show a session forever waiting on a question nobody can act on. The
    /// timeline needs to say the turn stopped because its agent died, rather than
    /// leaving an unexplained gap.
    /// Returns the rows it abandoned, not merely how many.
    ///
    /// The caller has to *announce* each one — a client showing an approval panel has
    /// no other way to learn the question died with its agent — and a count cannot be
    /// announced. Returning the rows is what lets one publisher serve every departure
    /// from `pending`; callers wanting the old number take `.len()`.
    pub async fn abandon_open_requests(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<PendingRequest>> {
        sqlx::query_as::<_, PendingRequest>(
            "UPDATE pending_requests
                SET status = 'abandoned', resolved_at_ms = ?
              WHERE session_env = ? AND session_agent = ? AND session_native = ? AND status = 'pending'
            RETURNING *",
        )
        .bind(now_ms())
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// A session's status, without loading the whole row.
    pub async fn session_status(&self, id: &SessionId) -> Option<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM sessions
              WHERE environment = ? AND agent = ? AND native_id = ?",
        )
        .bind(id.environment.as_str())
        .bind(id.agent.as_str())
        .bind(id.native_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
    }

    /// The turn currently executing on a session, if any.
    ///
    /// A link's events have to be attributed to a turn, and the link does not
    /// know turn ids — they are the server's concept. Ordered descending so a
    /// journal that somehow holds two running turns attributes to the newer,
    /// which is the one an event just arrived for.
    pub async fn running_turn_id(&self, session_id: &SessionId) -> Result<Option<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM turns
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
                AND status IN ('queued', 'starting', 'running', 'stopping', 'settling')
              ORDER BY created_at_ms DESC LIMIT 1",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn active_turn_count(&self) -> Result<usize> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM turns WHERE status IN ('queued','starting','running','stopping','settling')",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count as usize)
    }

    /// A session's journal rows after `after_seq`, across every turn.
    ///
    /// Distinct from `turn_events_after`: a replay is session-scoped, and the
    /// journal holds only the tail that consolidation has not moved yet.
    pub async fn session_events_after(
        &self,
        session_id: &SessionId,
        after_seq: i32,
    ) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events
              WHERE session_env = ? AND session_agent = ? AND session_native = ?
                AND seq > ? AND stability = 'stable'
              ORDER BY seq",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .bind(after_seq)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Drop a turn's journal rows once history holds them.
    ///
    /// Called only by consolidation, and only after the history insert has
    /// committed. `docs/storage.md` states the rule: history is durable before the
    /// journal forgets, never the reverse.
    pub async fn delete_turn_events(&self, turn_id: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM events WHERE turn_id = ?")
            .bind(turn_id)
            .execute(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(result.rows_affected())
    }

    /// Every journal row for a turn, deltas included.
    ///
    /// Only consolidation wants this: the deltas are what it folds. A reader that
    /// used it for a transcript would show each token twice.
    pub async fn turn_events_for_consolidation(&self, turn_id: &str) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>("SELECT * FROM events WHERE turn_id = ? ORDER BY seq")
            .bind(turn_id)
            .fetch_all(&self.pool)
            .await
            .map_err(Error::Database)
    }

    /// Turns whose events are still in the journal, oldest first.
    ///
    /// The consolidation work list. Only terminal turns qualify: a running turn's
    /// stream is still being appended to.
    pub async fn consolidatable_turns(&self, limit: i64) -> Result<Vec<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT t.id FROM turns t
               JOIN events e ON e.turn_id = t.id
              WHERE t.status IN ('completed', 'failed', 'cancelled', 'interrupted')
              ORDER BY t.completed_at_ms
              LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Abandon every open request across all sessions, for startup recovery.
    ///
    /// No link survives a restart, so anything still `pending` in the journal is
    /// waiting on a process that no longer exists.
    pub async fn abandon_all_open_requests(&self) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE pending_requests
                SET status = 'abandoned', resolved_at_ms = ?
              WHERE status = 'pending'",
        )
        .bind(now_ms())
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session every fixture below hangs off.
    fn session_id() -> SessionId {
        crate::fixtures::test_session_id("web-1")
    }

    /// A session and turn to hang events on. Both are foreign keys, so an event
    /// cannot be journaled without them.
    async fn seeded(dir: &std::path::Path) -> JournalStore {
        let store = JournalStore::new(dir.join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        store
            .create_session(&Session {
                id: session_id(),
                workdir: Some("/tmp/project".to_string()),
                title: None,
                inject_identity: true,
                // A live link holds the process's own stdio, so a session these
                // fixtures journal events onto is stream fidelity.
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .expect("session");
        store
            .create_turn(&crate::models::Turn {
                id: "turn-1".to_string(),
                session_id: session_id(),
                status: "running".to_string(),
                origin: "user".to_string(),
                origin_subject: None,
                request_text: "go".to_string(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                close_reason: None,
                created_at_ms: 1,
                started_at_ms: Some(1),
                completed_at_ms: None,
                stopped_at_ms: None,
            })
            .await
            .expect("turn");
        store
    }

    fn span_event(seq: i32, kind: &str, phase: &str, stream: &str, span: Option<&str>) -> Event {
        Event {
            id: None,
            session_id: session_id(),
            turn_id: Some("turn-1".to_string()),
            seq,
            kind: kind.to_string(),
            phase: Some(phase.to_string()),
            stream_id: Some(stream.to_string()),
            span: span.map(str::to_string),
            interior_state: None,
            name: None,
            text: String::new(),
            data_json: "{}".to_string(),
            is_error: false,
            stability: "stable".to_string(),
            source: "agent".to_string(),
            arrival_at_ms: seq as i64,
            recorded_at_ms: Some(seq as i64),
            created_at_ms: seq as i64,
        }
    }

    #[tokio::test]
    async fn an_events_trajectory_survives_the_journal() {
        // The point of the column: without it a reload reparents every delegated
        // event onto the root, which is the flattening the tree exists to prevent.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "subagent", "started", "s1", None))
            .await
            .expect("open trajectory");
        store
            .append_event(&span_event(2, "thinking", "completed", "b1", Some("s1")))
            .await
            .expect("child event");

        let events = store
            .turn_events_after("turn-1", 0)
            .await
            .expect("read back");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].span, None, "a root trajectory names no parent");
        assert_eq!(events[1].span.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn a_trajectory_left_open_is_reported_so_the_server_can_close_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        // s1 closes; s2 is nested inside it and never does. A UI given only this
        // stream must report s2 as running forever.
        store
            .append_event(&span_event(1, "subagent", "started", "s1", None))
            .await
            .expect("s1 open");
        store
            .append_event(&span_event(2, "subagent", "started", "s2", Some("s1")))
            .await
            .expect("s2 open");
        store
            .append_event(&span_event(3, "subagent", "completed", "s1", None))
            .await
            .expect("s1 close");

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert_eq!(open.len(), 1, "only the unclosed trajectory");
        assert_eq!(open[0].0, "s2");
        assert_eq!(
            open[0].1.as_deref(),
            Some("s1"),
            "its parent travels with it, so the closing event lands in the tree"
        );
    }

    #[tokio::test]
    async fn a_delta_does_not_count_as_closing_a_trajectory() {
        // `delta` is progress, not an outcome. Treating it as a close would leave
        // a failed delegation looking like a finished one.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "subagent", "started", "s1", None))
            .await
            .expect("open");
        store
            .append_event(&span_event(2, "subagent", "delta", "s1", None))
            .await
            .expect("progress");

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert_eq!(open.len(), 1);
    }

    /// A trajectory whose first event was a `delta` is still swept.
    ///
    /// Every `claude` workflow delegate is this shape: its first notification is
    /// progress, not a start. Matching `phase = 'started'` skipped them, so the
    /// turn-settle sweep never closed them and their cards read *running*
    /// permanently, across reloads — which is the bug this whole query exists to
    /// prevent, reached through a door it did not check.
    #[tokio::test]
    async fn a_trajectory_opened_by_a_delta_is_still_closed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "tool_call", "completed", "host-1", None))
            .await
            .expect("host");
        store
            .append_event(&span_event(
                2,
                "subagent",
                "delta",
                "task-a",
                Some("host-1"),
            ))
            .await
            .expect("progress");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        // Both, and that is the point: the delegate is declared, and naming a host
        // this turn also emitted promotes that host too. Sweeping one and not the
        // other would leave whichever was missed spinning.
        assert!(ids.contains(&"task-a"), "the delegate is open: {ids:?}");
        assert!(ids.contains(&"host-1"), "so is its host: {ids:?}");

        let delegate = open
            .iter()
            .find(|row| row.0 == "task-a")
            .expect("the delegate");
        assert_eq!(
            delegate.1.as_deref(),
            Some("host-1"),
            "its host travels with it, so the close lands inside the container"
        );
    }

    /// A `span` naming nothing this turn emitted is not this turn's to close.
    ///
    /// This fixture used to omit the host's `tool_call` and still assert the host was
    /// swept, on the reasoning that a missed host spins forever. That reasoning does
    /// not hold when the host has no row: there is no card to spin, and the server's
    /// close would *invent* one whose only phase is its own ending.
    ///
    /// What forced the change is that this shape is indistinguishable, in the journal,
    /// from a delegation reporting its outcome on a later turn — consolidation has by
    /// then deleted the rows that would say which turn owned the host. So the choice is
    /// between a phantom container for a malformed `span`, and a fabricated `abandoned`
    /// record on every backgrounded delegation that ever reports back. The second is
    /// measured and routine; the first is neither.
    #[tokio::test]
    async fn a_span_this_turn_never_emitted_is_not_swept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(
                1,
                "subagent",
                "completed",
                "task-a",
                Some("elsewhere"),
            ))
            .await
            .expect("a late outcome, naming a host from another turn");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert!(
            !ids.contains(&"elsewhere"),
            "the foreign host is left alone: {ids:?}"
        );
    }

    /// A `tool_call` another event names as its `span` is a trajectory.
    ///
    /// Promotion (`docs/session-protocol.md` § Promotion). Delegation is only
    /// knowable retroactively, so a host that never declared itself must still be
    /// swept — otherwise an interrupted turn leaves its container spinning while
    /// every child inside it is resolved.
    #[tokio::test]
    async fn a_promoted_tool_call_is_swept_when_nothing_closed_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "tool_call", "completed", "host-1", None))
            .await
            .expect("host");
        store
            .append_event(&span_event(
                2,
                "thinking",
                "completed",
                "b1",
                Some("host-1"),
            ))
            .await
            .expect("child");

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert_eq!(
            open.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
            vec!["host-1"],
            "the host is open; its child is not a trajectory"
        );
    }

    /// A promoted host's `tool_result` closes it, so the sweep leaves it alone.
    ///
    /// A `tool_call`'s own `completed` phase reports that its arguments finished
    /// streaming and says nothing about the delegated work, so the result is the
    /// only close it has.
    #[tokio::test]
    async fn a_tool_result_closes_the_trajectory_its_call_hosted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "tool_call", "completed", "host-1", None))
            .await
            .expect("host");
        store
            .append_event(&span_event(
                2,
                "thinking",
                "completed",
                "b1",
                Some("host-1"),
            ))
            .await
            .expect("child");
        store
            .append_event(&span_event(3, "tool_result", "completed", "host-1", None))
            .await
            .expect("result");

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert!(open.is_empty(), "a resolved host is not open: {open:?}");
    }

    /// Backgrounded work is still running, not abandoned.
    ///
    /// The sweep exists so silence never has to explain a gap. For work the agent said
    /// runs in the background the reasoning inverts: it is *expected* to outlive its
    /// turn and to report its own outcome on an agent-initiated turn afterwards, so a
    /// close written now is a contradiction that later completion cannot undo — both
    /// records would exist, on different turns, and the earlier card would say the
    /// work never finished when it demonstrably did.
    #[tokio::test]
    async fn a_backgrounded_delegate_is_left_open_rather_than_abandoned() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut started = span_event(1, "subagent", "started", "bg-task", Some("host-1"));
        started.data_json = r#"{"is_backgrounded":true,"task_type":"local_agent"}"#.to_string();
        store.append_event(&started).await.expect("open");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert!(
            !ids.contains(&"bg-task"),
            "backgrounded work is not swept: {ids:?}"
        );
    }

    /// The flag is found on whichever phase carried it, not on the latest one.
    ///
    /// `claude` reports `is_backgrounded` on `task_started` and on no later
    /// notification, so its adapter re-attaches it — but an adapter nobody has written
    /// yet need not, and a `delta` arriving after the start must not un-background the
    /// trajectory. Reading only the newest row would close exactly the work the flag
    /// exists to protect.
    #[tokio::test]
    async fn the_background_flag_survives_a_later_phase_that_omits_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut started = span_event(1, "subagent", "started", "bg-task", Some("host-1"));
        started.data_json = r#"{"is_backgrounded":true}"#.to_string();
        store.append_event(&started).await.expect("open");
        // Progress, with no flag — the shape the vendor actually sends.
        store
            .append_event(&span_event(
                2,
                "subagent",
                "delta",
                "bg-task",
                Some("host-1"),
            ))
            .await
            .expect("progress");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert!(
            !ids.contains(&"bg-task"),
            "still backgrounded despite the later phase: {ids:?}"
        );
    }

    /// A restart closes backgrounded work too, because nothing can report it any more.
    ///
    /// The exception the turn-settle sweep makes rests on the agent being alive to
    /// report back. No link survives a restart, so honouring the flag at startup would
    /// leave the delegation open forever — the permanent spinner the sweep exists to
    /// prevent, reached by way of the exception that exists to prevent a wrong record.
    #[tokio::test]
    async fn a_restart_closes_backgrounded_work_the_settle_sweep_would_skip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut started = span_event(1, "subagent", "started", "bg-task", None);
        started.data_json = r#"{"is_backgrounded":true}"#.to_string();
        store.append_event(&started).await.expect("open");

        // The settle sweep leaves it alone; the startup sweep must not.
        assert!(
            store
                .open_trajectories("turn-1")
                .await
                .expect("settle sweep")
                .is_empty(),
            "while the agent lives, this is work in progress"
        );

        let closed = store
            .close_stale_trajectories()
            .await
            .expect("startup sweep");
        assert_eq!(closed, 1, "once the agent is gone, nothing will report it");
    }

    /// The same delegate, without the flag, is still swept.
    ///
    /// The exclusion has to be narrow: an ordinary delegation left open is the spinner
    /// this whole query exists to prevent, so a predicate that skipped everything would
    /// trade one silent failure for another.
    #[tokio::test]
    async fn a_delegate_with_no_background_flag_is_still_swept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(
                1,
                "subagent",
                "started",
                "fg-task",
                Some("host-1"),
            ))
            .await
            .expect("open");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert!(
            ids.contains(&"fg-task"),
            "an ordinary delegate is swept: {ids:?}"
        );
    }

    /// Many delegates under one host are many trajectories, not one.
    ///
    /// `claude`'s `Workflow` fans out under a single `tool_use_id`. Keyed by the
    /// host, all of them collapse into one row where the last name wins — measured
    /// on a real session as a two-agent workflow rendering as a single card.
    #[tokio::test]
    async fn every_delegate_under_one_host_is_swept_separately() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "tool_call", "completed", "host-1", None))
            .await
            .expect("host");
        for (seq, task) in [(2, "task-a"), (3, "task-b")] {
            store
                .append_event(&span_event(
                    seq,
                    "subagent",
                    "started",
                    task,
                    Some("host-1"),
                ))
                .await
                .expect("delegate");
        }
        store
            .append_event(&span_event(
                4,
                "subagent",
                "completed",
                "task-a",
                Some("host-1"),
            ))
            .await
            .expect("close one");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(
            ids,
            vec!["host-1", "task-b"],
            "the closed delegate is gone; its sibling and their host are not"
        );
    }

    /// A crash skips `settle`, so startup has to close what it left open.
    ///
    /// Consolidation folds a trajectory with no close into no block at all, so the
    /// delegation vanishes from history while its children survive and reattach to
    /// the root: the work is misattributed permanently.
    #[tokio::test]
    async fn startup_closes_a_trajectory_a_crash_left_open() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .append_event(&span_event(1, "subagent", "started", "task-a", None))
            .await
            .expect("open");
        store
            .append_event(&span_event(
                2,
                "thinking",
                "completed",
                "b1",
                Some("task-a"),
            ))
            .await
            .expect("child");

        let closed = store.close_stale_trajectories().await.expect("sweep");
        assert_eq!(closed, 1);

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert!(open.is_empty(), "nothing is left open: {open:?}");

        let events = store
            .turn_events_for_consolidation("turn-1")
            .await
            .expect("read back");
        let close = events
            .iter()
            .find(|e| e.kind == "subagent" && e.phase.as_deref() == Some("completed"))
            .expect("a closing event was journaled");
        assert!(
            close.data_json.contains("\"closedBy\":\"server\""),
            "attributed to the server so the UI reports it as abandoned, not failed"
        );
        assert_eq!(close.stream_id.as_deref(), Some("task-a"));

        // Idempotent: a second startup finds nothing to do.
        assert_eq!(store.close_stale_trajectories().await.expect("again"), 0);
    }

    /// A late outcome corrects the sweep's provisional close on the earlier turn.
    ///
    /// The contradiction this removes: both records existed, on different turns, and
    /// the earlier card said the work never finished when it demonstrably had.
    #[tokio::test]
    async fn a_late_outcome_corrects_the_turn_that_opened_the_trajectory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut swept = span_event(1, "subagent", "completed", "bg-task", Some("bg-host"));
        swept.data_json = r#"{"closedBy":"server","reason":"abandoned"}"#.to_string();
        swept.is_error = true;
        swept.source = "server".to_string();
        store.append_event(&swept).await.expect("the sweep's close");

        let amended = store
            .reconcile_trajectory(
                &session_id(),
                "bg-task",
                "turn-2",
                &crate::supervisor::TrajectoryOutcome {
                    text: "worker finished",
                    is_error: false,
                    reported_on_turn_id: "turn-2",
                },
            )
            .await
            .expect("reconcile");
        assert!(amended, "the provisional close was corrected");

        let row = store
            .turn_events_after("turn-1", -1)
            .await
            .expect("read back")
            .into_iter()
            .find(|e| e.stream_id.as_deref() == Some("bg-task"))
            .expect("the trajectory is still on the turn that opened it");

        assert_eq!(row.text, "worker finished");
        assert!(!row.is_error, "a reported outcome is not an error");
        // `closedBy` is the *only* thing a client reads to decide `abandoned`, so
        // leaving it would keep rendering "never finished" over work that finished.
        assert!(
            !row.data_json.contains("closedBy"),
            "the server no longer claims this close: {}",
            row.data_json
        );
        assert!(
            row.data_json.contains("reportedOnTurnId"),
            "and the correction says where the outcome came from: {}",
            row.data_json
        );
    }

    /// An outcome the agent already stated is never rewritten by a later mention.
    ///
    /// Only two records are provisional — the sweep's own close, and a trajectory left
    /// open. A delegate that closed cleanly on its own turn and is described again later
    /// is being summarized, not re-decided; letting the last word win would let a
    /// summary quoting an earlier failure invert the outcome it quotes.
    #[tokio::test]
    async fn a_settled_outcome_is_not_rewritten_by_a_later_mention() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut settled = span_event(1, "subagent", "completed", "task-a", None);
        settled.text = "the delegate failed".to_string();
        settled.is_error = true;
        store.append_event(&settled).await.expect("its own close");

        let amended = store
            .reconcile_trajectory(
                &session_id(),
                "task-a",
                "turn-2",
                &crate::supervisor::TrajectoryOutcome {
                    text: "all good",
                    is_error: false,
                    reported_on_turn_id: "turn-2",
                },
            )
            .await
            .expect("reconcile");

        assert!(!amended, "an agent-stated outcome stands");
        let row = store
            .turn_events_after("turn-1", -1)
            .await
            .expect("read back")
            .into_iter()
            .find(|e| e.stream_id.as_deref() == Some("task-a"))
            .expect("the row");
        assert_eq!(row.text, "the delegate failed");
        assert!(row.is_error);
    }

    /// A trajectory still open on the earlier turn is closed by its real outcome.
    ///
    /// This is the shape a *cooperating* adapter produces: the sweep skipped it because
    /// it was flagged backgrounded, so there is no provisional close to correct — only
    /// an open trajectory waiting for the report this delivers.
    #[tokio::test]
    async fn a_late_outcome_closes_a_trajectory_the_sweep_left_open() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        let mut open = span_event(1, "subagent", "started", "bg-task", Some("bg-host"));
        open.data_json = r#"{"is_backgrounded":true}"#.to_string();
        open.name = Some("Background: worker".to_string());
        store.append_event(&open).await.expect("left open");

        assert!(store
            .reconcile_trajectory(
                &session_id(),
                "bg-task",
                "turn-2",
                &crate::supervisor::TrajectoryOutcome {
                    text: "worker finished",
                    is_error: false,
                    reported_on_turn_id: "turn-2",
                },
            )
            .await
            .expect("reconcile"));

        let row = store
            .turn_events_after("turn-1", -1)
            .await
            .expect("read back")
            .into_iter()
            .find(|e| e.stream_id.as_deref() == Some("bg-task"))
            .expect("the row");
        assert_eq!(row.phase.as_deref(), Some("completed"));
        assert_eq!(row.text, "worker finished");
        assert_eq!(
            row.name.as_deref(),
            Some("Background: worker"),
            "the name given at open survives the correction"
        );
        assert!(
            row.data_json.contains("is_backgrounded"),
            "and so does the flag that explains why it outlived its turn: {}",
            row.data_json
        );
    }

    /// A settling turn is not terminal, and its outcome is not published until it is.
    ///
    /// The property every client depends on, stated where it is enforced. A reader between
    /// the two writes must be able to tell "the outcome is known" from "the record is
    /// finished", which is exactly what one column reporting both could not do.
    #[tokio::test]
    async fn a_claimed_turn_is_not_terminal_until_it_is_promoted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        assert!(
            store
                .begin_settling("turn-1", None, Some(""))
                .await
                .expect("claim"),
            "the first caller claims the turn"
        );

        let claimed = store.get_turn("turn-1").await.expect("read").expect("turn");
        assert_eq!(claimed.status, "settling");
        assert!(
            !claimed.is_terminal(),
            "a client polling mid-sweep must read this as in flight, or it acts on a \
             record the sweep has not finished writing"
        );
        assert!(
            claimed.completed_at_ms.is_some(),
            "the turn ended when its outcome arrived, so the stamp is the claim's — \
             charging the sweep's duration to the turn would report time nobody spent"
        );

        assert!(
            store
                .promote_settled_turn("turn-1", "completed")
                .await
                .expect("promote"),
            "the promotion follows its own claim"
        );

        let settled = store.get_turn("turn-1").await.expect("read").expect("turn");
        assert_eq!(settled.status, "completed");
        assert!(settled.is_terminal());
        assert_eq!(
            settled.completed_at_ms, claimed.completed_at_ms,
            "promotion publishes the outcome and changes nothing else"
        );
    }

    /// The claim is the first-writer-wins guard, so the sweep runs once.
    ///
    /// Two settlements race by design — a stop records `cancelled` and the agent's own
    /// `turn` event arrives a moment later — and before two-phase settling the terminal
    /// status was what refused the second one. It has to be the claim now: a second caller
    /// getting through would sweep the same turn again and emit a duplicate close for every
    /// trajectory it left open.
    #[tokio::test]
    async fn only_one_caller_can_claim_a_turn_for_settling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        assert!(store
            .begin_settling(
                "turn-1",
                Some("TURN_INTERRUPTED"),
                Some("Interrupted by user")
            )
            .await
            .expect("first claim"));
        assert!(
            !store
                .begin_settling("turn-1", None, Some(""))
                .await
                .expect("second claim"),
            "a turn already settling is refused, or its trajectories are closed twice"
        );

        store
            .promote_settled_turn("turn-1", "cancelled")
            .await
            .expect("promote");

        assert!(
            !store
                .begin_settling("turn-1", None, Some(""))
                .await
                .expect("post-terminal claim"),
            "and a settled turn stays settled: the agent's late `completed` must not \
             overwrite the stop the user asked for"
        );

        let turn = store.get_turn("turn-1").await.expect("read").expect("turn");
        assert_eq!(turn.status, "cancelled");
        assert_eq!(turn.error_code.as_deref(), Some("TURN_INTERRUPTED"));
    }

    /// A promotion with no claim behind it does nothing.
    ///
    /// What keeps the two calls a pair rather than two independent writers of `status`. It
    /// is also the case a restart produces: `interrupt_stale_turns` settles a turn whose
    /// sweep was still in flight, and the in-flight sweep's promotion must not resurrect it
    /// with an outcome the restart already overrode.
    #[tokio::test]
    async fn a_promotion_without_a_claim_changes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        assert!(
            !store
                .promote_settled_turn("turn-1", "completed")
                .await
                .expect("unclaimed promotion"),
            "a running turn cannot be promoted straight to terminal"
        );
        assert_eq!(
            store
                .get_turn("turn-1")
                .await
                .expect("read")
                .expect("turn")
                .status,
            "running"
        );

        store
            .begin_settling("turn-1", None, Some(""))
            .await
            .expect("claim");
        store.interrupt_stale_turns().await.expect("restart sweep");

        assert!(
            !store
                .promote_settled_turn("turn-1", "completed")
                .await
                .expect("promotion after a restart claimed it"),
            "the restart's account stands"
        );
        assert_eq!(
            store
                .get_turn("turn-1")
                .await
                .expect("read")
                .expect("turn")
                .status,
            "interrupted"
        );
    }

    /// A restart sweeps a settling turn, keeping the end time its claim recorded.
    ///
    /// Left out of the restart sweep it would stay `settling` forever, which is the
    /// spinning card that sweep exists to prevent. Included naively it would be stamped
    /// with the restart, giving a turn that finished before the crash a duration measured
    /// to whenever the backend came back.
    #[tokio::test]
    async fn a_restart_settles_a_claimed_turn_without_moving_its_end() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = seeded(dir.path()).await;

        store
            .begin_settling("turn-1", None, Some(""))
            .await
            .expect("claim");
        let ended = store
            .get_turn("turn-1")
            .await
            .expect("read")
            .expect("turn")
            .completed_at_ms
            .expect("the claim stamped it");

        assert_eq!(
            store.interrupt_stale_turns().await.expect("restart sweep"),
            1,
            "a turn left settling by a crash is unrecoverable and must be reported"
        );

        let turn = store.get_turn("turn-1").await.expect("read").expect("turn");
        assert_eq!(turn.status, "interrupted");
        assert!(
            turn.is_terminal(),
            "nothing may be left reading as in flight"
        );
        assert_eq!(
            turn.completed_at_ms,
            Some(ended),
            "the turn ended when its outcome arrived, not when the backend came back"
        );
    }

    /// Seed `natives` sessions that all share one `updated_at_ms`.
    ///
    /// The tie is written, not raced for. Creating sessions quickly enough to land
    /// in one millisecond would pass or fail on machine speed, which is a test that
    /// stops testing without saying so.
    async fn tied_at(dir: &std::path::Path, stamp: i64, natives: &[&str]) -> JournalStore {
        let store = JournalStore::new(dir.join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        for native in natives {
            store
                .create_session(&Session {
                    id: crate::fixtures::test_session_id(native),
                    workdir: Some("/tmp/project".to_string()),
                    title: None,
                    inject_identity: true,
                    fidelity: sylph_protocol::Fidelity::Messages,
                    flags_json: "{}".to_string(),
                    status: "idle".to_string(),
                    created_at_ms: stamp,
                    updated_at_ms: stamp,
                })
                .await
                .expect("session");
        }
        store
    }

    #[tokio::test]
    async fn sessions_sharing_one_timestamp_list_in_the_same_order_every_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Inserted ascending, so rowid order is the reverse of the answer. Without a
        // tiebreaker SQLite is free to return either, and the one it returns for a
        // three-row table is not the one it returns under a different plan.
        let store = tied_at(dir.path(), 1_700_000_000_000, &["a", "b", "c"]).await;

        let natives = |rows: &[Session]| -> Vec<String> {
            rows.iter()
                .map(|s| s.id.native_id.as_str().to_string())
                .collect()
        };
        let filter = SessionFilter::default();

        let first = store.list_sessions(&filter).await.expect("list");
        let second = store.list_sessions(&filter).await.expect("list again");

        assert_eq!(
            natives(&first),
            vec!["c", "b", "a"],
            "a tie breaks on the identity triple descending"
        );
        assert_eq!(
            natives(&first),
            natives(&second),
            "one filter, two answers: the listing has no total order"
        );
    }

    #[tokio::test]
    async fn capability_filter_is_applied_before_session_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        for (environment, installation) in [("env7fk2", "install-a"), ("env2abc", "install-b")] {
            sqlx::query(
                "INSERT INTO env_installations (environment_id, installation_id, name, origin, token_hash)
                 VALUES (?, ?, 'test', 'development', 'hash')",
            )
            .bind(environment)
            .bind(installation)
            .execute(store.raw_pool())
            .await
            .expect("environment");
        }
        sqlx::query(
            "INSERT INTO subjects (id, name, token_hash, is_admin, created_at_ms)
             VALUES ('subject-a', 'Subject A', 'hash-a', 0, 1)",
        )
        .execute(store.raw_pool())
        .await
        .expect("subject");
        let inaccessible = Session {
            id: SessionId::new(
                sylph_protocol::EnvironmentId::parse("env2abc").unwrap(),
                sylph_protocol::AgentName::parse("claude").unwrap(),
                sylph_protocol::NativeId::new("newest-hidden"),
            ),
            workdir: Some("/tmp/project".into()),
            title: None,
            inject_identity: true,
            fidelity: sylph_protocol::Fidelity::Messages,
            flags_json: "{}".into(),
            status: "idle".into(),
            created_at_ms: 2,
            updated_at_ms: 2,
        };
        let visible = Session {
            id: SessionId::new(
                sylph_protocol::EnvironmentId::parse("env7fk2").unwrap(),
                sylph_protocol::AgentName::parse("claude").unwrap(),
                sylph_protocol::NativeId::new("older-visible"),
            ),
            updated_at_ms: 1,
            ..inaccessible.clone()
        };
        store.create_session(&inaccessible).await.expect("hidden");
        store.create_session(&visible).await.expect("visible");
        sqlx::query(
            "INSERT INTO env_grants (subject_id, environment_id, capability, issued_at_ms)
             VALUES ('subject-a', 'env7fk2', 'observe', 1)",
        )
        .execute(store.raw_pool())
        .await
        .expect("grant");

        let rows = store
            .list_sessions(&SessionFilter {
                limit: 1,
                capability_subject: Some("subject-a".into()),
                capability: Some("observe".into()),
                ..Default::default()
            })
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id.native_id.as_str(), "older-visible");
    }

    #[tokio::test]
    async fn a_cursor_landing_inside_a_tie_still_reaches_every_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stamp = 1_700_000_000_000;
        let store = tied_at(dir.path(), stamp, &["a", "b", "c", "d"]).await;

        // A page that ends mid-tie. `updated_before_ms` is inclusive, so the next
        // page re-reads the whole tied group — that overlap is only survivable
        // because both pages agree on the order, which is what makes a client's
        // dedupe by id correct rather than lossy.
        let page = |limit: i64, before: Option<i64>| SessionFilter {
            limit,
            updated_before_ms: before,
            ..SessionFilter::default()
        };
        let first = store.list_sessions(&page(2, None)).await.expect("page 1");
        assert_eq!(first.len(), 2, "the limit bounds the page");

        let cursor = first.last().expect("a row").updated_at_ms;
        let second = store
            .list_sessions(&page(4, Some(cursor)))
            .await
            .expect("page 2");

        let mut seen: Vec<String> = first
            .iter()
            .chain(second.iter())
            .map(|s| s.id.native_id.as_str().to_string())
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen,
            vec!["a", "b", "c", "d"],
            "walking the cursor across a tie must not drop a session"
        );
        assert_eq!(
            second
                .iter()
                .map(|s| s.id.native_id.as_str())
                .collect::<Vec<_>>(),
            vec!["d", "c", "b", "a"],
            "the second page repeats the first in the same order it was first served"
        );
    }
}
