use super::*;

pub(super) fn is_aggregate_limit(error: &GenerationError) -> bool {
    matches!(
        error,
        GenerationError::Limit("projection batch changes" | "projection batch bytes")
    )
}

pub(super) fn validate(
    generation: &GenerationStore,
    batch: &ProjectionBatch,
) -> super::super::generation::Result<()> {
    generation.reconciliation_batches(batch)?;
    Ok(())
}

pub(super) async fn apply(
    generation: &GenerationStore,
    token: u64,
    expected: &Snapshot,
    batch: &ProjectionBatch,
) -> Result<super::super::generation::Checkpoint> {
    match generation.validate_batch(batch) {
        Ok(()) => Ok(generation.apply(token, expected, batch).await?),
        Err(error) if is_aggregate_limit(&error) => {
            Ok(generation.apply_reconciled(token, expected, batch).await?)
        }
        Err(error) => Err(error.into()),
    }
}
