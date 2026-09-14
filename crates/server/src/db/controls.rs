use super::JournalStore;
use crate::{
    error::{Error, Result},
    models::ControlRequest,
};

impl JournalStore {
    pub async fn control_request(&self, id: &str) -> Result<Option<ControlRequest>> {
        sqlx::query_as("SELECT * FROM control_requests WHERE request_id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Error::Database)
    }

    pub async fn amend_control_request(
        &self,
        id: &str,
        revision: i64,
        text: Option<&str>,
        state: &str,
    ) -> Result<Option<ControlRequest>> {
        sqlx::query_as("UPDATE control_requests SET text = COALESCE(?, text), state = ?, revision = revision + 1 WHERE request_id = ? AND revision = ? AND state = 'queued' RETURNING *")
            .bind(text).bind(state).bind(id).bind(revision).fetch_optional(&self.pool).await.map_err(Error::Database)
    }

    pub async fn queued_controls(&self) -> Result<Vec<ControlRequest>> {
        // One head per idle session prevents a busy session from monopolizing a pass.
        sqlx::query_as("SELECT c.* FROM control_requests c WHERE c.state = 'queued'
            AND NOT EXISTS (SELECT 1 FROM turns t WHERE t.session_env = c.session_env AND t.session_agent = c.session_agent AND t.session_native = c.session_native AND t.status IN ('queued','starting','running','stopping','settling'))
            AND NOT EXISTS (SELECT 1 FROM control_requests p WHERE p.session_env = c.session_env AND p.session_agent = c.session_agent AND p.session_native = c.session_native AND p.state = 'queued' AND p.server_seq < c.server_seq)
            ORDER BY c.created_at_ms LIMIT 32")
            .fetch_all(&self.pool).await.map_err(Error::Database)
    }

    pub async fn set_control_target(&self, id: &str, turn_id: &str) -> Result<()> {
        sqlx::query("UPDATE control_requests SET target_turn_id = ? WHERE request_id = ?")
            .bind(turn_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(Error::Database)?;
        Ok(())
    }

    pub async fn recover_control_requests(&self) -> Result<()> {
        sqlx::query("UPDATE control_requests SET state = CASE WHEN state='dispatched' THEN 'unconfirmed' ELSE 'failed' END, error_message = 'Server restarted before delivery could be confirmed; inspect the transcript before submitting again.', failed_at_ms = ?, revision = revision + 1 WHERE state IN ('accepted', 'dispatched')")
            .bind(crate::models::now_ms()).execute(&self.pool).await.map_err(Error::Database)?;
        Ok(())
    }
}
