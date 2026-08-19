use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use crate::error::{Error, Result};
use crate::models::{Event, Incarnation, Session, Turn};

#[derive(Clone)]
pub struct LiveStore {
    pool: SqlitePool,
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
        sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    pub async fn list_sessions(&self, limit: i64) -> Result<Vec<Session>> {
        sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions ORDER BY updated_at_ms DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
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

        sqlx::query(
            "UPDATE sessions SET backend_session_id = ?, updated_at_ms = ? WHERE id = ?",
        )
        .bind(backend_session_id)
        .bind(now_ms())
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
        sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE trace_session_id = ?")
            .bind(trace_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    /// List sessions with optional agent/origin filters, newest activity first.
    pub async fn list_sessions_filtered(
        &self,
        limit: i64,
        agent: Option<&str>,
        origin: Option<&str>,
    ) -> Result<Vec<Session>> {
        // Both filters are optional, so the predicate is written to be a no-op
        // when the bound value is NULL rather than branching into four queries.
        sqlx::query_as::<_, Session>(
            r#"
            SELECT * FROM sessions
            WHERE (?1 IS NULL OR agent = ?1)
              AND (?2 IS NULL OR origin = ?2)
            ORDER BY updated_at_ms DESC
            LIMIT ?3
            "#,
        )
        .bind(agent)
        .bind(origin)
        .bind(limit)
        .fetch_all(&self.pool)
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
    pub async fn finish_turn(
        &self,
        turn_id: &str,
        status: &str,
        exit_code: Option<i32>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE turns
            SET status = ?, completed_at_ms = ?, runner_exit_code = ?,
                error_code = ?, error_message = ?
            WHERE id = ?
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
        Ok(())
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

    /// Journaled events for one turn after `after_seq`, in sequence order.
    pub async fn turn_events_after(&self, turn_id: &str, after_seq: i32) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE turn_id = ? AND seq > ? ORDER BY seq",
        )
        .bind(turn_id)
        .bind(after_seq)
        .fetch_all(&self.pool)
        .await
        .map_err(Error::Database)
    }
}

/// Current wall-clock time in milliseconds, the unit used by every `*_at_ms`
/// column in the schema.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
