use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use crate::error::{Error, Result};
use crate::models::{Event, Incarnation, PendingRequest, Session, Turn};
use crate::tracedb::summarize_line;

#[derive(Clone)]
pub struct JournalStore {
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

impl JournalStore {
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
                id, session_id, incarnation_id, status, origin, request_text,
                command_invocation_id, idempotency_key, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&turn.id)
        .bind(&turn.session_id)
        .bind(&turn.incarnation_id)
        .bind(&turn.status)
        .bind(&turn.origin)
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
                session_id, turn_id, seq, kind, phase, stream_id, span, name,
                text, data_json, is_error, stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&event.session_id)
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.span)
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
            let open = self.open_trajectories(&turn_id).await?;
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
                    name,
                    text: String::new(),
                    // The same shape `settle` emits, so a client cannot tell which
                    // path closed a trajectory — and `abandoned` stays distinct from
                    // `failed`, because nobody reported an outcome here either.
                    data_json: r#"{"closedBy":"server","reason":"backend_restarted"}"#
                        .to_string(),
                    is_error: true,
                    stability: "stable".to_string(),
                    source: "server".to_string(),
                    created_at_ms: now_ms(),
                    reconciled_at_ms: None,
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
    async fn turn_tail(&self, turn_id: &str) -> Result<Option<(String, i32)>> {
        let Some(session_id) =
            sqlx::query_scalar::<_, String>("SELECT session_id FROM turns WHERE id = ?")
                .bind(turn_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(Error::Database)?
        else {
            return Ok(None);
        };

        let max_seq = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(seq) FROM events WHERE session_id = ?",
        )
        .bind(&session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)?;

        Ok(Some((session_id, max_seq.unwrap_or(0) + 1)))
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
                session_id, turn_id, seq, kind, phase, stream_id, span, name,
                text, data_json, is_error, stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&event.session_id)
        .bind(&event.turn_id)
        .bind(event.seq)
        .bind(&event.kind)
        .bind(&event.phase)
        .bind(&event.stream_id)
        .bind(&event.span)
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
                    session_id, turn_id, seq, kind, phase, stream_id, span, name,
                    text, data_json, is_error, stability, source, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&event.session_id)
            .bind(&event.turn_id)
            .bind(event.seq)
            .bind(&event.kind)
            .bind(&event.phase)
            .bind(&event.stream_id)
            .bind(&event.span)
            .bind(&event.name)
            .bind(&event.text)
            .bind(&event.data_json)
            .bind(event.is_error)
            .bind(&event.stability)
            .bind(&event.source)
            .bind(event.created_at_ms)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
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
    /// Returns `(stream_id, span, name)` — the parent and name are needed to emit a
    /// closing event that lands in the right place in the tree.
    pub async fn open_trajectories(
        &self,
        turn_id: &str,
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
                  WHERE turn_id = ? AND kind = 'subagent' AND stream_id IS NOT NULL
                  GROUP BY stream_id
                 UNION ALL
                 SELECT span AS id, MIN(seq) AS seq,
                        NULL, NULL, NULL, NULL
                   FROM events
                  WHERE turn_id = ? AND span IS NOT NULL
                  GROUP BY span
             )
             SELECT open.id,
                    MAX(COALESCE(open.declared_span, open.any_span)),
                    MAX(COALESCE(open.declared_name, open.any_name))
               FROM open
              WHERE NOT EXISTS (
                    SELECT 1 FROM events done
                     WHERE done.turn_id = ?
                       AND done.stream_id = open.id
                       AND done.phase = 'completed'
                       AND done.kind IN ('subagent', 'tool_result')
              )
              GROUP BY open.id
              ORDER BY MIN(open.seq)",
        )
        .bind(turn_id)
        .bind(turn_id)
        .bind(turn_id)
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

    /// Record a request an agent raised, so it survives every client leaving.
    pub async fn create_pending_request(&self, request: &PendingRequest) -> Result<PendingRequest> {
        sqlx::query_as::<_, PendingRequest>(
            r#"
            INSERT INTO pending_requests (
                id, session_id, turn_id, adapter_request_id, kind, tool,
                tool_use_id, title, detail_json, options_json, form_json,
                default_option, risk, allow_persistent, expires_at_ms, status,
                created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)
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

    /// A session and turn to hang events on. Both are foreign keys, so an event
    /// cannot be journaled without them.
    async fn seeded(dir: &std::path::Path) -> JournalStore {
        let store = JournalStore::new(dir.join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        store
            .create_session(&Session {
                id: "web-1".to_string(),
                origin: "sylph".to_string(),
                identity_mode: "sylph".to_string(),
                lifecycle_mode: "sylph".to_string(),
                agent: "claude".to_string(),
                backend_session_id: None,
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
            .create_turn(&crate::models::Turn {
                id: "turn-1".to_string(),
                session_id: "web-1".to_string(),
                incarnation_id: None,
                status: "running".to_string(),
                origin: "user".to_string(),
                request_text: "go".to_string(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
                runner_exit_code: None,
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
            session_id: "web-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            seq,
            kind: kind.to_string(),
            phase: Some(phase.to_string()),
            stream_id: Some(stream.to_string()),
            span: span.map(str::to_string),
            name: None,
            text: String::new(),
            data_json: "{}".to_string(),
            is_error: false,
            stability: "stable".to_string(),
            source: "agent".to_string(),
            created_at_ms: seq as i64,
            reconciled_at_ms: None,
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
            .append_event(&span_event(1, "subagent", "delta", "task-a", Some("host-1")))
            .await
            .expect("progress");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        // Both, and that is the point: the delegate is declared, and naming a host
        // as its span promotes that host too. Sweeping one and not the other would
        // leave whichever was missed spinning.
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
            .append_event(&span_event(2, "thinking", "completed", "b1", Some("host-1")))
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
            .append_event(&span_event(2, "thinking", "completed", "b1", Some("host-1")))
            .await
            .expect("child");
        store
            .append_event(&span_event(3, "tool_result", "completed", "host-1", None))
            .await
            .expect("result");

        let open = store.open_trajectories("turn-1").await.expect("query");
        assert!(open.is_empty(), "a resolved host is not open: {open:?}");
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
                .append_event(&span_event(seq, "subagent", "started", task, Some("host-1")))
                .await
                .expect("delegate");
        }
        store
            .append_event(&span_event(4, "subagent", "completed", "task-a", Some("host-1")))
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
            .append_event(&span_event(2, "thinking", "completed", "b1", Some("task-a")))
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

    #[tokio::test]
    async fn archive_identity_matches_native_root_but_not_subagent_prefix() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
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
