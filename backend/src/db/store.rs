use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use crate::db::labels::summarize_line;
use crate::error::{Error, Result};
use crate::events::data_from_column;
use crate::models::{Event, PendingRequest, Session, Turn};
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

    /// Record a session an agent has already assigned an id.
    ///
    /// **The caller must have started the agent first.** A row exists only once a
    /// session has an identity (`docs/architecture.md` § Identity), so this is the
    /// second half of `POST /sessions` rather than the whole of it — see
    /// [`crate::links::LinkManager::open_session`], which owns that ordering.
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
                inject_identity, fidelity, flags_json, status, parent_session,
                created_at_ms, updated_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .bind(&session.parent_session)
        .bind(session.created_at_ms)
        .bind(session.updated_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
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
    /// Asked per row, that is one indexed lookup each — correct, and **measured at 4.26 s**
    /// for `codex`'s 1258 rollouts against 232 ms for `pi`'s 531, which is the round-trip
    /// count rather than the query. It also inverts the requirement the adapter contract
    /// puts on `enumerate`: the listing pass is deliberately one short header read per
    /// file (164 ms measured) precisely so a whole-store listing is affordable, and paying
    /// a database round trip per row hands that back one layer up.
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
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT native_id FROM sessions WHERE environment = ? AND agent = ?",
        )
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
            ORDER BY updated_at_ms DESC
            LIMIT ?6
            "#,
        )
        .bind(filter.agent.as_deref())
        .bind(filter.environment.as_deref())
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
                request_text, command_invocation_id, idempotency_key, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&turn.id)
        .bind(turn.session_id.environment.as_str())
        .bind(turn.session_id.agent.as_str())
        .bind(turn.session_id.native_id.as_str())
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
                phase, stream_id, span, name, text, data_json, is_error,
                stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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

    /// Set the opening title once, without changing activity ordering. This is
    /// used when a turn starts so live and archived rows acquire the same label
    /// before TraceDB ingestion catches up.
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
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool> {
        self.finish_turn_at(turn_id, status, error_code, error_message, now_ms())
            .await
    }

    /// Record a terminal turn state at a stated time.
    ///
    /// Same first-writer-wins guard as [`Self::finish_turn`], which delegates here so
    /// there is one implementation of the guard rather than two that can drift.
    ///
    /// # Why the timestamp is a parameter
    ///
    /// A live turn ends *now*, so [`Self::finish_turn`] reading the clock is right. An
    /// **adopted** turn ended whenever the vendor recorded it, possibly months ago
    /// (`crate::db::ingest`), and stamping it `now()` would report every adopted turn as
    /// having finished at the moment somebody clicked adopt — giving a whole transcript
    /// one timestamp, a duration of zero, and a position at the top of every
    /// recency-ordered listing.
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
                phase, stream_id, span, name, text, data_json, is_error,
                stability, source, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                    session_env, session_agent, session_native, turn_id, seq, kind,
                    phase, stream_id, span, name, text, data_json, is_error,
                    stability, source, created_at_ms
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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

    /// Highest applied migration version, or 0 on an unmigrated journal.
    ///
    /// Reported by `/health` so a client can detect that it is talking to a
    /// backend whose journal shape it does not understand.
    ///
    /// # Why this reads `_sqlx_migrations` rather than `app_meta`
    ///
    /// It used to read an `app_meta.schema_version` key that migrations bumped by
    /// hand. `001` wrote `1`, `003` wrote `3`, and `004` through `007` forgot — so a
    /// fully-migrated journal published `3` while seven migrations had run, and the
    /// one question the field exists to answer was answered wrongly. A number the
    /// migration runner already maintains must not have a second hand-written copy:
    /// the copy is what drifts, and it drifts silently because nothing compares them.
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
              WHERE session_env = ? AND session_agent = ? AND session_native = ? AND status IN ('running', 'stopping')
              ORDER BY created_at_ms DESC LIMIT 1",
        )
        .bind(session_id.environment.as_str())
        .bind(session_id.agent.as_str())
        .bind(session_id.native_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
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
                workdir: "/tmp/project".to_string(),
                title: None,
                inject_identity: true,
                // A live link holds the process's own stdio, so a session these
                // fixtures journal events onto is stream fidelity.
                fidelity: sylph_protocol::Fidelity::Stream,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                parent_session: None,
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
                request_text: "go".to_string(),
                command_invocation_id: None,
                idempotency_key: None,
                error_code: None,
                error_message: None,
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
            name: None,
            text: String::new(),
            data_json: "{}".to_string(),
            is_error: false,
            stability: "stable".to_string(),
            source: "agent".to_string(),
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
            .append_event(&span_event(2, "subagent", "delta", "task-a", Some("host-1")))
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
            .append_event(&span_event(1, "subagent", "completed", "task-a", Some("elsewhere")))
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
            .append_event(&span_event(2, "subagent", "delta", "bg-task", Some("host-1")))
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

        let closed = store.close_stale_trajectories().await.expect("startup sweep");
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
            .append_event(&span_event(1, "subagent", "started", "fg-task", Some("host-1")))
            .await
            .expect("open");

        let open = store.open_trajectories("turn-1").await.expect("query");
        let ids: Vec<&str> = open.iter().map(|row| row.0.as_str()).collect();
        assert!(ids.contains(&"fg-task"), "an ordinary delegate is swept: {ids:?}");
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
}
