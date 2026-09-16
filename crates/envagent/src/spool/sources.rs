//! A missing file does not erase the locator a previous pass actually observed.
use super::{capture::CaptureAuthorization, Spool, SpoolError};
use sqlx::Row;
use sylph_protocol::{trace::IngestionEpoch, NativeSource, SessionId, SourceStateEvent};
impl Spool {
    pub async fn native_sources(
        &self,
        session: &SessionId,
        epoch: &IngestionEpoch,
    ) -> Result<Vec<NativeSource>, SpoolError> {
        let values:Vec<String>=sqlx::query_scalar("SELECT source_json FROM native_source_inventory WHERE session_key=? AND ingestion_epoch=? ORDER BY locator").bind(session.encode()).bind(epoch.as_str()).fetch_all(&self.readers).await?;
        values
            .into_iter()
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(anyhow::Error::from)
                    .map_err(SpoolError::from)
            })
            .collect()
    }
    pub async fn remember_native_sources(
        &self,
        session: &SessionId,
        epoch: &IngestionEpoch,
        sources: &[NativeSource],
    ) -> Result<(), SpoolError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for source in sources {
            sqlx::query("INSERT INTO native_source_inventory(session_key,ingestion_epoch,locator,source_json) VALUES(?,?,?,?) ON CONFLICT(session_key,ingestion_epoch,locator) DO UPDATE SET source_json=excluded.source_json WHERE source_json!=excluded.source_json")
                .bind(session.encode()).bind(epoch.as_str()).bind(&source.locator).bind(serde_json::to_string(source).map_err(anyhow::Error::from)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn source_state_matches(
        &self,
        session: &SessionId,
        state: &SourceStateEvent,
        epoch: &IngestionEpoch,
    ) -> Result<bool, SpoolError> {
        let previous:Option<String>=sqlx::query_scalar("SELECT state_json FROM captured_source_states WHERE session_key=? AND source_key=? AND ingestion_epoch=?")
            .bind(session.encode()).bind(state.source.encode()).bind(epoch.as_str()).fetch_optional(&self.readers).await?;
        Ok(
            previous.as_deref()
                == Some(&serde_json::to_string(state).map_err(anyhow::Error::from)?),
        )
    }
    pub async fn remember_source_state(
        &self,
        session: &SessionId,
        state: &SourceStateEvent,
        authorization: &CaptureAuthorization,
    ) -> Result<(), SpoolError> {
        sqlx::query("INSERT INTO captured_source_states(session_key,source_key,ingestion_epoch,state_json,authorization_json) VALUES(?,?,?,?,?) ON CONFLICT(session_key,source_key,ingestion_epoch) DO UPDATE SET state_json=excluded.state_json,authorization_json=excluded.authorization_json")
            .bind(session.encode()).bind(state.source.encode()).bind(authorization.epoch.as_str()).bind(serde_json::to_string(state).map_err(anyhow::Error::from)?).bind(serde_json::to_string(authorization).map_err(anyhow::Error::from)?).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn source_states(
        &self,
        session: &SessionId,
    ) -> Result<Vec<(SourceStateEvent, CaptureAuthorization)>, SpoolError> {
        let rows = sqlx::query(
            "SELECT state_json,authorization_json FROM captured_source_states WHERE session_key=?",
        )
        .bind(session.encode())
        .fetch_all(&self.readers)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    serde_json::from_str(&row.try_get::<String, _>("state_json")?)
                        .map_err(anyhow::Error::from)?,
                    serde_json::from_str(&row.try_get::<String, _>("authorization_json")?)
                        .map_err(anyhow::Error::from)?,
                ))
            })
            .collect()
    }
}
