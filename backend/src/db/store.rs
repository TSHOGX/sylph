use sqlx::{sqlite::SqlitePool, sqlite::SqlitePoolOptions};
use std::path::Path;
use crate::error::{Error, Result};
use crate::models::{Session, Turn, Event};

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

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&format!("sqlite:{}?mode=rwc", path))
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
            .bind(chrono::Utc::now().timestamp_millis())
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
}


