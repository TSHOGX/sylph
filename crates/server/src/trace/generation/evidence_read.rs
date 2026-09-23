//! Snapshot-bound claim membership and bounded metadata authorization inputs.
use super::*;
use sylph_protocol::trace::ObservationRef;

impl GenerationStore {
    pub async fn claim_support(
        &self,
        snapshot: &Snapshot,
        subject: &AnyObjectRef,
        claim: &ClaimRef,
        limit: usize,
    ) -> Result<Vec<ObservationRef>> {
        if limit == 0 || limit > self.limits.max_candidates {
            return Err(GenerationError::Limit("claim support work"));
        }
        let (mut conn, _) = self.read_connection(snapshot).await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM claim_subjects WHERE subject_ref=? AND claim_ref=? AND first_revision<=?)")
            .bind(object_key(subject)?).bind(claim.encode()).bind(signed(snapshot.view_revision)?).fetch_one(&mut *conn).await.map_err(map_query_error)?;
        if !exists {
            return Err(GenerationError::NotFound);
        }
        let rows: Vec<String> = sqlx::query_scalar("SELECT observation_ref FROM claim_evidence WHERE claim_ref=? ORDER BY observation_ref LIMIT ?")
            .bind(claim.encode()).bind((limit + 1) as i64).fetch_all(&mut *conn).await.map_err(map_query_error)?;
        if rows.is_empty() {
            return Err(GenerationError::NotFound);
        }
        if rows.len() > limit {
            return Err(GenerationError::Limit("claim support work"));
        }
        rows.into_iter()
            .map(|value| {
                ObservationRef::decode(&value)
                    .map_err(|error| GenerationError::Invalid(error.to_string()))
            })
            .collect()
    }
}
