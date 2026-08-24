use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use crate::error::{Error, Result};
use crate::models::{Event, Incarnation, PendingRequest, Session, Turn};
use crate::tracedb::summarize_line;

#[derive(Clone)]
pub struct LiveStore {
    pool: SqlitePool,
}

/// Narrowing criteria for a session listing. `None` means "do not narrow on
/// this field", which is distinct from matching an empty value.
#[derive(Debug, Clone)]
pub struct SessionFilter {
    pub limit: i64,
    pub agent: Option<String>,
    pub origin: Option<String>,
    /// Case-insensitive substring of the working directory, so a user can type
    /// a project name without reproducing an absolute path.
    pub workdir: Option<String>,
    pub updated_after_ms: Option<i64>,
    pub updated_before_ms: Option<i64>,
}

impl Default for SessionFilter {
    fn default() -> Self {
        Self {
            limit: 50,
            agent: None,
            origin: None,
            workdir: None,
            updated_after_ms: None,
            updated_before_ms: None,
        }
    }
}

impl LiveStore {
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
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| Error::Internal(format!("Migration failed: {}", e)))?;

        Ok(Self { pool })
    }

    // Session operations
    pub async fn create_session(&self, session: &Session) -> Result<Session> {
        sqlx::query_as::<_, Session>(
            r#"
            INSERT INTO sessions (
                id, origin, identity_mode, lifecycle_mode, agent,
                backend_session_id, trace_session_id, workdir, title,
                flags_json, status, created_at_ms, updated_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&session.id)
        .bind(&session.origin)
        .bind(&session.identity_mode)
        .bind(&session.lifecycle_mode)
        .bind(&session.agent)
        .bind(&session.backend_session_id)
        .bind(&session.trace_session_id)
        .bind(&session.workdir)
        .bind(&session.title)
        .bind(&session.flags_json)
        .bind(&session.status)
        .bind(session.created_at_ms)
        .bind(session.updated_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let mut session = sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)?;
        if let Some(session) = session.as_mut() {
            self.hydrate_session_title(session).await?;
        }
        Ok(session)
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
              AND (?2 IS NULL OR origin = ?2)
              AND (?3 IS NULL OR instr(lower(workdir), lower(?3)) > 0)
              AND (?4 IS NULL OR updated_at_ms >= ?4)
              AND (?5 IS NULL OR updated_at_ms <= ?5)
            ORDER BY updated_at_ms DESC
            LIMIT ?6
            "#,
        )
        .bind(filter.agent.as_deref())
        .bind(filter.origin.as_deref())
        .bind(filter.workdir.as_deref())
        .bind(filter.updated_after_ms)
        .bind(filter.updated_before_ms)
        .bind(filter.limit)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)?;
        for session in &mut sessions {
            self.hydrate_session_title(session).await?;
        }
        Ok(sessions)
    }

    /// Fill titles for older live rows created before title derivation existed.
    /// The first user event is the user's actual opening ask, unlike the
    /// runner's injected envelope, and therefore matches TraceDB's label rule.
    async fn hydrate_session_title(&self, session: &mut Session) -> Result<()> {
        if session.title.as_deref().is_some_and(|title| !title.trim().is_empty()) {
            return Ok(());
        }
        let Some(text) = sqlx::query_scalar::<_, String>(
            "SELECT text FROM events WHERE session_id = ? AND kind = 'user' ORDER BY seq LIMIT 1",
        )
        .bind(&session.id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)? else {
            return Ok(());
        };
        let title = summarize_line(&text);
        if title.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE sessions SET title = ? WHERE id = ? AND (title IS NULL OR trim(title) = '')",
        )
        .bind(&title)
        .bind(&session.id)
        .execute(&self.pool)
        .await?;
        session.title = Some(title);
        Ok(())
    }

    /// Associate an archived TraceDB id with a live session. Normal live turns
    /// only know the runner's native id, so the fallback matches the stable
    /// `<agent>:<native-id>` identity that TraceDB uses once ingestion occurs.
    pub async fn find_session_by_archive_id(&self, archive_id: &str) -> Result<Option<Session>> {
        if let Some(session) = self.find_session_by_trace_id(archive_id).await? {
            return Ok(Some(session));
        }
        // A slash denotes a TraceDB subagent identity. Its root native id is
        // shared with the parent, so matching only the prefix would incorrectly
        // hide delegated work as if it were the live parent row.
        if archive_id.contains('/') {
            return Ok(None);
        }
        let Some((agent, native)) = archive_id.split_once(':') else {
            return Ok(None);
        };
        sqlx::query_as::<_, Session>(
            "SELECT s.* FROM sessions s
             WHERE s.agent = ?
               AND (s.backend_session_id = ? OR EXISTS (
                 SELECT 1 FROM backend_incarnations i
                 WHERE i.session_id = s.id AND i.backend_session_id = ?
               ))
             ORDER BY s.updated_at_ms DESC LIMIT 1",
        )
        .bind(agent)
        .bind(native)
        .bind(native)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Remove a provisional Sylph session that never received a turn.
    pub async fn delete_empty_sylph_session(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ? AND origin = 'sylph' AND NOT EXISTS (SELECT 1 FROM turns WHERE turns.session_id = sessions.id)")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_session_status(&self, id: &str, status: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET status = ?, updated_at_ms = ? WHERE id = ?")
            .bind(status)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Turn operations
    pub async fn create_turn(&self, turn: &Turn) -> Result<Turn> {
        sqlx::query_as::<_, Turn>(
            r#"
            INSERT INTO turns (
                id, session_id, incarnation_id, status, request_text,
                command_invocation_id, idempotency_key, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&turn.id)
        .bind(&turn.session_id)
        .bind(&turn.incarnation_id)
        .bind(&turn.status)
        .bind(&turn.request_text)
        .bind(&turn.command_invocation_id)
        .bind(&turn.idempotency_key)
        .bind(turn.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_turn(&self, id: &str) -> Result<Option<Turn>> {
        sqlx::query_as::<_, Turn>("SELECT * FROM turns WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    pub async fn list_turns(&self, session_id: &str, limit: i64) -> Result<Vec<Turn>> {
        sqlx::query_as::<_, Turn>(
            "SELECT * FROM turns WHERE session_id = ? ORDER BY created_at_ms DESC LIMIT ?",
        )
        .bind(session_id)
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
                session_id, turn_id, seq, kind, phase, stream_id, name,
                text, data_json, is_error, stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&event.session_id)
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.name)
        .bind(&event.text)
        .bind(&event.data_json)
        .bind(event.is_error)
        .bind(&event.stability)
        .bind(&event.source)
        .bind(event.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_events(
        &self,
        session_id: &str,
        turn_id: Option<&str>,
        after_seq: Option<i32>,
    ) -> Result<Vec<Event>> {
        let query = match (turn_id, after_seq) {
            (Some(tid), Some(seq)) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events WHERE session_id = ? AND turn_id = ? AND seq > ? ORDER BY seq",
                )
                .bind(session_id)
                .bind(tid)
                .bind(seq)
            }
            (Some(tid), None) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events WHERE session_id = ? AND turn_id = ? ORDER BY seq",
                )
                .bind(session_id)
                .bind(tid)
            }
            (None, Some(seq)) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events WHERE session_id = ? AND seq > ? ORDER BY seq",
                )
                .bind(session_id)
                .bind(seq)
            }
            (None, None) => {
                sqlx::query_as::<_, Event>(
                    "SELECT * FROM events WHERE session_id = ? ORDER BY seq",
                )
                .bind(session_id)
            }
        };

        query.fetch_all(&self.pool).await.map_err(Error::Database)
    }

    /// Persist agent-native session id on both the session and its incarnation.
    pub async fn set_backend_session_id(
        &self,
        session_id: &str,
        incarnation_id: Option<&str>,
        backend_session_id: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        // `updated_at_ms` is deliberately not touched. It orders sessions by *user
        // activity* — it is what "pick up where you left off" and the sidebar sort
        // mean — and learning where an agent keeps its own session is bookkeeping, not
        // activity. Bumping it made merely *opening* an old session reorder it above a
        // newer one, because establishing a link persists this id.
        sqlx::query("UPDATE sessions SET backend_session_id = ? WHERE id = ?")
            .bind(backend_session_id)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

        if let Some(incarnation_id) = incarnation_id {
            sqlx::query("UPDATE backend_incarnations SET backend_session_id = ? WHERE id = ?")
                .bind(backend_session_id)
                .bind(incarnation_id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn set_trace_session_id(&self, session_id: &str, trace_id: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET trace_session_id = ?, updated_at_ms = ? WHERE id = ?")
            .bind(trace_id)
            .bind(now_ms())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_session_by_trace_id(&self, trace_id: &str) -> Result<Option<Session>> {
        sqlx::query_as::<_, Session>(
            "SELECT s.* FROM sessions s
             WHERE s.trace_session_id = ? OR EXISTS (
               SELECT 1 FROM backend_incarnations i
               WHERE i.session_id = s.id
                 AND (i.source_trace_session_id = ? OR i.resulting_trace_session_id = ?)
             )
             ORDER BY s.updated_at_ms DESC LIMIT 1",
        )
        .bind(trace_id)
        .bind(trace_id)
        .bind(trace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn touch_session(&self, session_id: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET updated_at_ms = ? WHERE id = ?")
            .bind(now_ms())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_session_title(&self, session_id: &str, title: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET title = ?, updated_at_ms = ? WHERE id = ?")
            .bind(title)
            .bind(now_ms())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set the opening title once, without changing activity ordering. This is
    /// used when a turn starts so live and archived rows acquire the same label
    /// before TraceDB ingestion catches up.
    pub async fn set_session_title_if_empty(&self, session_id: &str, title: &str) -> Result<()> {
        let title = summarize_line(title);
        if title.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE sessions SET title = ? WHERE id = ? AND (title IS NULL OR trim(title) = '')",
        )
        .bind(&title)
        .bind(session_id)
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
    pub async fn set_session_flags(&self, session_id: &str, flags_json: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET flags_json = ?, updated_at_ms = ? WHERE id = ?")
            .bind(flags_json)
            .bind(now_ms())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Incarnation operations

    pub async fn create_incarnation(&self, incarnation: &Incarnation) -> Result<Incarnation> {
        sqlx::query_as::<_, Incarnation>(
            r#"
            INSERT INTO backend_incarnations (
                id, session_id, agent, backend_session_id, started_at_ms,
                source_trace_session_id, status
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&incarnation.id)
        .bind(&incarnation.session_id)
        .bind(&incarnation.agent)
        .bind(&incarnation.backend_session_id)
        .bind(incarnation.started_at_ms)
        .bind(&incarnation.source_trace_session_id)
        .bind(&incarnation.status)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// The session's current incarnation, or None if it has never run a turn.
    pub async fn active_incarnation(&self, session_id: &str) -> Result<Option<Incarnation>> {
        sqlx::query_as::<_, Incarnation>(
            r#"
            SELECT * FROM backend_incarnations
            WHERE session_id = ? AND status = 'active'
            ORDER BY started_at_ms DESC
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn list_incarnations(&self, session_id: &str) -> Result<Vec<Incarnation>> {
        sqlx::query_as::<_, Incarnation>(
            "SELECT * FROM backend_incarnations WHERE session_id = ? ORDER BY started_at_ms DESC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// End the active incarnation so the next turn starts a fresh agent context.
    pub async fn end_incarnation(&self, incarnation_id: &str, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE backend_incarnations
            SET status = 'ended', ended_at_ms = ?, reset_reason = ?
            WHERE id = ?
            "#,
        )
        .bind(now_ms())
        .bind(reason)
        .bind(incarnation_id)
        .execute(&self.pool)
        .await?;
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
        session_id: &str,
        key: &str,
    ) -> Result<Option<Turn>> {
        sqlx::query_as::<_, Turn>(
            "SELECT * FROM turns WHERE session_id = ? AND idempotency_key = ?",
        )
        .bind(session_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// Claim a queued turn for execution, returning false if it is no longer
    /// queued. The compare-and-set keeps two supervisors from starting the same
    /// turn twice.
    pub async fn mark_turn_running(&self, turn_id: &str, incarnation_id: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'running', started_at_ms = ?, incarnation_id = ?
            WHERE id = ? AND status IN ('queued', 'starting')
            "#,
        )
        .bind(now_ms())
        .bind(incarnation_id)
        .bind(turn_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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

    /// Record a terminal turn state. `status` is one of `completed`, `failed`,
    /// `cancelled`, or `interrupted`.
    /// Record a turn's terminal state. First writer wins.
    ///
    /// The `status NOT IN (...)` guard is what makes a terminal state final. Two
    /// settlements race by design: `stop` interrupts the agent and records
    /// `cancelled`, and the agent then emits its own `turn` event, which arrives a
    /// moment later and would otherwise overwrite that with `completed`. The user
    /// asked for a stop and got one, so reporting the turn as having completed
    /// normally is a lie about what happened.
    ///
    /// Returns whether this call was the one that settled it, so a caller can tell
    /// "I settled this" from "someone already had".
    pub async fn finish_turn(
        &self,
        turn_id: &str,
        status: &str,
        exit_code: Option<i32>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = ?, completed_at_ms = ?, runner_exit_code = ?,
                error_code = ?, error_message = ?
            WHERE id = ?
              AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
            "#,
        )
        .bind(status)
        .bind(now_ms())
        .bind(exit_code)
        .bind(error_code)
        .bind(error_message)
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
    pub async fn interrupt_stale_turns(&self) -> Result<u64> {
        let mut tx = self.pool.begin().await?;

        let result = sqlx::query(
            r#"
            UPDATE turns
            SET status = 'interrupted', completed_at_ms = ?,
                error_code = 'TURN_INTERRUPTED',
                error_message = 'Backend restarted while this turn was running'
            WHERE status IN ('queued', 'starting', 'running', 'stopping')
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

    pub async fn count_turns_by_status(&self, session_id: Option<&str>) -> Result<Vec<(String, i64)>> {
        sqlx::query_as::<_, (String, i64)>(
            r#"
            SELECT status, COUNT(*) FROM turns
            WHERE (?1 IS NULL OR session_id = ?1)
            GROUP BY status
            "#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn count_sessions_by_status(&self) -> Result<Vec<(String, i64)>> {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT status, COUNT(*) FROM sessions GROUP BY status",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }

    // Event journal

    /// Highest sequence number recorded for a session, or -1 when empty.
    ///
    /// Used to seed the in-memory sequence counter after a restart so new
    /// events never collide with journaled ones.
    pub async fn max_event_seq(&self, session_id: &str) -> Result<i32> {
        let seq: Option<i32> =
            sqlx::query_scalar("SELECT MAX(seq) FROM events WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(seq.unwrap_or(-1))
    }

    /// Append a stable event to the journal.
    ///
    /// Ephemeral events are deliberately not written here — they live only in
    /// the hub's bounded projection.
    pub async fn append_event(&self, event: &Event) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO events (
                session_id, turn_id, seq, kind, phase, stream_id, name,
                text, data_json, is_error, stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&event.session_id)
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.name)
        .bind(&event.text)
        .bind(&event.data_json)
        .bind(event.is_error)
        .bind(&event.stability)
        .bind(&event.source)
        .bind(event.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Schema version recorded in `app_meta`, or 0 when unset.
    ///
    /// Reported by `/health` so a client can detect that it is talking to a
    /// backend whose journal shape it does not understand.
    pub async fn schema_version(&self) -> Result<i64> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM app_meta WHERE key = 'schema_version'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    /// Journaled events for one turn after `after_seq`, in sequence order.
    /// A turn's *stable* journal rows: what a transcript renders.
    ///
    /// Deltas are journaled too — consolidation needs them to fold — but they are
    /// excluded here, because a `completed` event already carries its block's whole
    /// text. Including both would render every token twice.
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

    /// Record a request an agent raised, so it survives every client leaving.
    pub async fn create_pending_request(&self, request: &PendingRequest) -> Result<PendingRequest> {
        sqlx::query_as::<_, PendingRequest>(
            r#"
            INSERT INTO pending_requests (
                id, session_id, turn_id, adapter_request_id, kind, tool,
                tool_use_id, title, detail_json, options_json, default_option,
                risk, allow_persistent, expires_at_ms, status, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)
            RETURNING *
            "#,
        )
        .bind(&request.id)
        .bind(&request.session_id)
        .bind(&request.turn_id)
        .bind(&request.adapter_request_id)
        .bind(&request.kind)
        .bind(&request.tool)
        .bind(&request.tool_use_id)
        .bind(&request.title)
        .bind(&request.detail_json)
        .bind(&request.options_json)
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
    pub async fn open_requests(&self, session_id: &str) -> Result<Vec<PendingRequest>> {
        sqlx::query_as::<_, PendingRequest>(
            "SELECT * FROM pending_requests
             WHERE session_id = ? AND status = 'pending'
             ORDER BY created_at_ms",
        )
        .bind(session_id)
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

    /// Mark every open request on a session abandoned, returning how many.
    ///
    /// Called when a link dies with requests outstanding. They can never be
    /// answered — the adapter waiting on them is gone — so leaving them `pending`
    /// would show a session forever waiting on a question nobody can act on. The
    /// timeline needs to say the turn stopped because its agent died, rather than
    /// leaving an unexplained gap.
    pub async fn abandon_open_requests(&self, session_id: &str) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE pending_requests
                SET status = 'abandoned', resolved_at_ms = ?
              WHERE session_id = ? AND status = 'pending'",
        )
        .bind(now_ms())
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(Error::Database)?;
        Ok(result.rows_affected())
    }

    /// A session's status, without loading the whole row.
    pub async fn session_status(&self, session_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id)
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
    pub async fn running_turn_id(&self, session_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM turns
              WHERE session_id = ? AND status IN ('running', 'stopping')
              ORDER BY created_at_ms DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    /// A session's journal rows after `after_seq`, across every turn.
    ///
    /// Distinct from `turn_events_after`: a replay is session-scoped, and the
    /// journal holds only the tail that consolidation has not moved yet.
    pub async fn session_events_after(&self, session_id: &str, after_seq: i32) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events
              WHERE session_id = ? AND seq > ? AND stability = 'stable'
              ORDER BY seq",
        )
        .bind(session_id)
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
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE turn_id = ? ORDER BY seq",
        )
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

/// Current wall-clock time in milliseconds, the unit used by every `*_at_ms`
/// column in the schema.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_identity_matches_native_root_but_not_subagent_prefix() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LiveStore::new(dir.path().join("live.db").to_str().unwrap())
            .await
            .expect("store");
        store
            .create_session(&Session {
                id: "web-1".to_string(),
                origin: "sylph".to_string(),
                identity_mode: "sylph".to_string(),
                lifecycle_mode: "sylph".to_string(),
                agent: "claude".to_string(),
                backend_session_id: Some("native-1".to_string()),
                trace_session_id: None,
                workdir: "/tmp/project".to_string(),
                title: None,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .expect("session");
        store
            .create_incarnation(&Incarnation {
                id: "inc-1".to_string(),
                session_id: "web-1".to_string(),
                agent: "claude".to_string(),
                backend_session_id: Some("native-old".to_string()),
                started_at_ms: 1,
                ended_at_ms: Some(2),
                reset_reason: Some("test".to_string()),
                source_trace_session_id: None,
                resulting_trace_session_id: None,
                status: "ended".to_string(),
            })
            .await
            .expect("incarnation");

        assert_eq!(
            store
                .find_session_by_archive_id("claude:native-1")
                .await
                .expect("lookup")
                .expect("root match")
                .id,
            "web-1"
        );
        assert_eq!(
            store
                .find_session_by_archive_id("claude:native-old")
                .await
                .expect("lookup")
                .expect("historical match")
                .id,
            "web-1"
        );
        assert!(store
            .find_session_by_archive_id("claude:native-1/agent-worker")
            .await
            .expect("lookup")
            .is_none());
    }
}
