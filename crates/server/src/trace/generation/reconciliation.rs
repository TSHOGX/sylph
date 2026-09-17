use super::*;

#[cfg(test)]
pub(super) struct Pause {
    after_chunks: usize,
    entered: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

impl GenerationStore {
    // SQL transactions stay bounded, while an advisory lock reserves the complete
    // interpretation across them. Unlike a durable owner flag, cancellation and
    // process death release this reservation without a timeout or wall-clock lease.
    pub(super) async fn lock_complete_writer(&self) -> Result<std::fs::File> {
        let mut lock_path = self.path.canonicalize()?.into_os_string();
        lock_path.push(".writer-lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(GenerationError::Fenced);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    pub(crate) fn reconciliation_batches(
        &self,
        batch: &ProjectionBatch,
    ) -> Result<Vec<ProjectionBatch>> {
        self.validate_batch_contents(batch)?;
        let empty = ProjectionBatch {
            through_cut: batch.through_cut,
            continuation: batch.continuation,
            ..ProjectionBatch::default()
        };
        let base_bytes = serde_json::to_vec(&empty)?.len();
        let mut current = empty.clone();
        let mut current_bytes = base_bytes;
        let mut current_items = 0;
        let mut batches = Vec::new();
        let mut push = |piece: ProjectionBatch| -> Result<()> {
            self.validate_batch(&piece)?;
            // A singleton's serialized increment, plus one separator, bounds its
            // contribution to every array or map in the combined batch.
            let increment = serde_json::to_vec(&piece)?.len() - base_bytes + 1;
            if current_items != 0
                && (current_items == self.limits.max_batch_changes
                    || current_bytes + increment > self.limits.max_batch_bytes)
            {
                self.validate_batch(&current)?;
                batches.push(std::mem::replace(&mut current, empty.clone()));
                current_items = 0;
                current_bytes = base_bytes;
            }
            current.claims.extend(piece.claims);
            current.objects.extend(piece.objects);
            current.memberships.extend(piece.memberships);
            current.payload_anchors.extend(piece.payload_anchors);
            current.mapper_states.extend(piece.mapper_states);
            current
                .processed_occurrences
                .extend(piece.processed_occurrences);
            current
                .payload_derivations
                .extend(piece.payload_derivations);
            current.read_rows.extend(piece.read_rows);
            current_items += 1;
            current_bytes += increment;
            Ok(())
        };
        macro_rules! pieces {
            ($field:ident) => {
                for item in &batch.$field {
                    let mut piece = empty.clone();
                    piece.$field.push(item.clone());
                    push(piece)?;
                }
            };
        }
        // References are installed before memberships and authorization anchors.
        pieces!(claims);
        pieces!(objects);
        pieces!(memberships);
        pieces!(payload_anchors);
        pieces!(payload_derivations);
        pieces!(read_rows);
        pieces!(processed_occurrences);
        for (key, value) in &batch.mapper_states {
            let mut piece = empty.clone();
            piece.mapper_states.insert(key.clone(), value.clone());
            push(piece)?;
        }
        if current_items != 0 || batches.is_empty() {
            self.validate_batch(&current)?;
            batches.push(current);
        }
        Ok(batches)
    }

    pub(crate) async fn apply_reconciled(
        &self,
        expected_fence: u64,
        expected_snapshot: &Snapshot,
        batch: &ProjectionBatch,
    ) -> Result<Checkpoint> {
        let batches = self.reconciliation_batches(batch)?;
        let _writer = self.lock_complete_writer().await?;
        self.resume_reconciliation_locked(expected_fence).await?;
        let prior = self.checkpoint().await?;
        self.check_reconciliation_writer(&prior, expected_fence)?;
        if &prior.snapshot != expected_snapshot {
            return Err(GenerationError::StaleSnapshot {
                expected: expected_snapshot.clone(),
                actual: prior.snapshot,
            });
        }
        if batch.through_cut < expected_snapshot.evidence_cut
            || (batch.through_cut == expected_snapshot.evidence_cut
                && (!batch.continuation || !batch.processed_occurrences.is_empty()))
        {
            return Err(GenerationError::Invalid(
                "a same-cut interpretation continuation cannot claim new admitted occurrences"
                    .into(),
            ));
        }
        let revision = expected_snapshot
            .view_revision
            .checked_add(1)
            .ok_or_else(|| GenerationError::Invalid("revision overflow".into()))?;
        let mut changed = BTreeSet::new();
        let mut removed = BTreeSet::new();
        for update in &batch.objects {
            match update {
                ObjectUpdate::Put { .. } => changed.insert(update.reference()),
                ObjectUpdate::Withdraw { .. } => removed.insert(update.reference()),
            };
        }
        let changes = ChangeBatch {
            snapshot: Snapshot {
                generation: self.manifest.generation.clone(),
                evidence_cut: batch.through_cut,
                view_revision: revision,
            },
            changed: changed.into_iter().collect(),
            removed: removed.into_iter().collect(),
            moved: batch
                .memberships
                .iter()
                .map(|change| change.object_ref.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        };
        let mut change_json = serde_json::to_string(&changes)?;
        let change_bytes = change_json.len();
        if change_bytes > self.limits.max_batch_bytes {
            // The existing change-backlog contract requests a fresh view when its
            // byte budget is exceeded. Do not store an unbounded notification body.
            change_json.clear();
        }
        let digest = blake3::hash(&serde_json::to_vec(batch)?)
            .to_hex()
            .to_string();
        sqlx::query("INSERT INTO projection_reconciliation(singleton,previous_revision,previous_cut,target_revision,target_cut,fence_token,unit_digest,chunks,change_json,change_bytes) VALUES(1,?,?,?,?,?,?,?,?,?)")
            .bind(signed(expected_snapshot.view_revision)?).bind(signed(expected_snapshot.evidence_cut)?)
            .bind(signed(revision)?).bind(signed(batch.through_cut)?).bind(signed(expected_fence)?)
            .bind(digest).bind(batches.len() as i64).bind(change_json).bind(change_bytes as i64)
            .execute(&self.writer).await?;
        for (ordinal, chunk) in batches.iter().enumerate() {
            sqlx::query("INSERT INTO reconciliation_chunks VALUES(?,?)")
                .bind(ordinal as i64)
                .bind(serde_json::to_string(chunk)?)
                .execute(&self.writer)
                .await?;
        }
        let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO projection_revisions VALUES(?,?)")
            .bind(signed(revision)?)
            .bind(signed(batch.through_cut)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE projection_reconciliation SET ready=1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.resume_reconciliation_locked(expected_fence).await?;
        self.checkpoint().await
    }

    pub(crate) async fn resume_reconciliation(&self, expected_fence: u64) -> Result<()> {
        let _writer = self.lock_complete_writer().await?;
        self.resume_reconciliation_locked(expected_fence).await
    }

    fn check_reconciliation_writer(
        &self,
        checkpoint: &Checkpoint,
        expected_fence: u64,
    ) -> Result<()> {
        if checkpoint.fence_token != expected_fence || checkpoint.writer_mode != WriterMode::Running
        {
            return Err(GenerationError::Fenced);
        }
        Ok(())
    }

    pub(super) async fn resume_reconciliation_locked(&self, expected_fence: u64) -> Result<()> {
        loop {
            let Some(plan) = sqlx::query("SELECT previous_revision,previous_cut,target_revision,target_cut,fence_token,chunks,next_chunk,ready FROM projection_reconciliation WHERE singleton=1")
                .fetch_optional(&self.readers).await? else { return Ok(()) };
            let prior = self.checkpoint().await?;
            self.check_reconciliation_writer(&prior, expected_fence)?;
            if unsigned(plan.try_get("fence_token")?)? != expected_fence
                || unsigned(plan.try_get("previous_revision")?)? != prior.snapshot.view_revision
                || unsigned(plan.try_get("previous_cut")?)? != prior.snapshot.evidence_cut
            {
                return Err(GenerationError::Invalid(
                    "reconciliation no longer matches its reserved snapshot".into(),
                ));
            }
            if plan.try_get::<i64, _>("ready")? == 0 {
                // A cancelled preparation has changed no semantic table. Discard
                // its durable plan in bounded transactions, then recompute normally.
                let removed = sqlx::query("DELETE FROM reconciliation_chunks WHERE ordinal IN (SELECT ordinal FROM reconciliation_chunks ORDER BY ordinal LIMIT ?)")
                    .bind(self.limits.max_batch_changes as i64).execute(&self.writer).await?;
                if removed.rows_affected() != 0 {
                    tokio::task::yield_now().await;
                    continue;
                }
                sqlx::query("DELETE FROM projection_reconciliation WHERE singleton=1")
                    .execute(&self.writer)
                    .await?;
                return Ok(());
            }
            let next: i64 = plan.try_get("next_chunk")?;
            let revision = unsigned(plan.try_get("target_revision")?)?;
            if next < plan.try_get::<i64, _>("chunks")? {
                let json: String = sqlx::query_scalar(
                    "SELECT batch_json FROM reconciliation_chunks WHERE ordinal=?",
                )
                .bind(next)
                .fetch_one(&self.readers)
                .await?;
                let chunk: ProjectionBatch = serde_json::from_str(&json)?;
                self.validate_batch(&chunk)?;
                if chunk.through_cut != unsigned(plan.try_get("target_cut")?)? {
                    return Err(GenerationError::Invalid(
                        "reconciliation chunk names another evidence cut".into(),
                    ));
                }
                let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
                self.write_batch_rows(&mut tx, &chunk, revision).await?;
                sqlx::query("DELETE FROM reconciliation_chunks WHERE ordinal=?")
                    .bind(next)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE projection_reconciliation SET next_chunk=next_chunk+1 WHERE singleton=1")
                    .execute(&mut *tx).await?;
                tx.commit().await?;
                #[cfg(test)]
                {
                    let pause = {
                        let mut pause = self.reconciliation_pause.lock().unwrap();
                        if pause.as_ref().is_some_and(|pause| {
                            pause.after_chunks == next as usize + 1
                                || (pause.after_chunks == 0
                                    && next + 1 == plan.get::<i64, _>("chunks"))
                        }) {
                            pause.take()
                        } else {
                            None
                        }
                    };
                    if let Some(pause) = pause {
                        pause.entered.notify_one();
                        pause.resume.notified().await;
                    }
                }
                tokio::task::yield_now().await;
                continue;
            }
            let final_plan = sqlx::query("SELECT unit_digest,change_json,change_bytes FROM projection_reconciliation WHERE singleton=1")
                .fetch_one(&self.readers).await?;
            let mut tx = self.writer.begin_with("BEGIN IMMEDIATE").await?;
            self.publish_batch(
                &mut tx,
                &prior,
                unsigned(plan.try_get("target_cut")?)?,
                revision,
                &final_plan.try_get::<String, _>("unit_digest")?,
                &final_plan.try_get::<String, _>("change_json")?,
                usize::try_from(unsigned(final_plan.try_get("change_bytes")?)?)
                    .map_err(|_| GenerationError::Limit("change backlog bytes"))?,
            )
            .await?;
            sqlx::query("DELETE FROM projection_reconciliation WHERE singleton=1")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(());
        }
    }

    #[cfg(test)]
    pub(crate) fn pause_reconciliation_after(
        &self,
        after_chunks: usize,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        *self.reconciliation_pause.lock().unwrap() = Some(Pause {
            after_chunks,
            entered: entered.clone(),
            resume: resume.clone(),
        });
        (entered, resume)
    }
}
