//! Coordinate generation publication without holding the evidence/control writer.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    evidence::catalogue::{
        BuildTicket, CatalogueError, EvidenceCatalogue, PublicationGuard, PublishedGeneration,
        ReadyManifest,
    },
    generation::{
        FenceLease, GenerationError, GenerationManifest, GenerationStore, GenerationValidation,
        QueryLimits, Snapshot, WriterMode,
    },
};

#[async_trait]
pub trait ProjectionCatchUp: Send + Sync {
    /// The worker persists only complete validated units. Cancellation leaves its
    /// last committed generation checkpoint resumable from durable admission.
    async fn catch_up(
        &self,
        generation: &GenerationStore,
        writer_token: u64,
        through_cut: u64,
    ) -> anyhow::Result<()>;
    /// Full background validation includes retained payload/manifest bytes and
    /// source/receipt consistency, beyond the generation database's SQL integrity.
    async fn validate_generation(
        &self,
        generation: &GenerationStore,
        snapshot: &Snapshot,
    ) -> anyhow::Result<()>;
}

#[derive(Debug, Clone)]
pub struct PublicationLimits {
    pub barrier_budget: Duration,
    pub max_tail_revisions: usize,
    pub queries: QueryLimits,
}

impl Default for PublicationLimits {
    fn default() -> Self {
        Self {
            barrier_budget: Duration::from_secs(2),
            max_tail_revisions: 1024,
            queries: QueryLimits::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublicationError {
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error(transparent)]
    Generation(#[from] GenerationError),
    #[error("projection worker failed: {0:#}")]
    Worker(anyhow::Error),
    #[error("publication barrier budget expired; active projection was resumed")]
    BarrierExpired,
    #[error("publication inputs do not match the durable active/building generations")]
    GenerationMismatch,
    #[error("projection worker did not reach the requested evidence cut")]
    IncompleteCatchUp,
    #[error("publication recovery failed: {0}")]
    Recovery(String),
    #[cfg(test)]
    #[error("injected publication crash")]
    InjectedCrash,
}

pub type Result<T> = std::result::Result<T, PublicationError>;

#[derive(Clone)]
pub struct PublicationCoordinator {
    catalogue: EvidenceCatalogue,
    directory: PathBuf,
    limits: PublicationLimits,
    barrier: Arc<Mutex<()>>,
    #[cfg(test)]
    crash: Option<PublicationCrash>,
}

impl PublicationCoordinator {
    pub fn new(
        catalogue: EvidenceCatalogue,
        directory: impl Into<PathBuf>,
        limits: PublicationLimits,
    ) -> Result<Self> {
        if limits.barrier_budget.is_zero() || limits.max_tail_revisions == 0 {
            return Err(PublicationError::GenerationMismatch);
        }
        Ok(Self {
            catalogue,
            directory: directory.into(),
            limits,
            barrier: Arc::new(Mutex::new(())),
            #[cfg(test)]
            crash: None,
        })
    }

    pub fn catalogue(&self) -> &EvidenceCatalogue {
        &self.catalogue
    }

    pub async fn begin_build(
        &self,
        manifest: GenerationManifest,
        now_ms: i64,
    ) -> Result<(BuildTicket, GenerationStore)> {
        let ticket = self.catalogue.begin_build(manifest, now_ms).await?;
        let created = GenerationStore::create(
            self.directory.join(&ticket.relative_file),
            ticket.manifest.clone(),
            self.limits.queries.clone(),
        )
        .await;
        let store = match created {
            Ok(store) => store,
            Err(error) => {
                self.catalogue
                    .cancel_build(&ticket, "generation file creation failed")
                    .await?;
                return Err(error.into());
            }
        };
        let validation = store.validate_for_publication().await?;
        self.catalogue
            .register_file(&ticket, &validation.database_identity)
            .await?;
        Ok((ticket, store))
    }

    pub async fn publish(
        &self,
        ticket: &BuildTicket,
        private: &GenerationStore,
        old: Option<&GenerationStore>,
        worker: &impl ProjectionCatchUp,
    ) -> Result<PublishedGeneration> {
        let guard = self.catalogue.acquire_publication(ticket).await?;
        let result = self
            .publish_guarded(ticket, private, old, worker, &guard, None)
            .await;
        self.catalogue.release_publication(&guard).await?;
        result
    }

    /// Rebuild an unavailable published file only after exclusive startup recovery
    /// proves the prior process is gone. Current admitted evidence bounds every cut
    /// the lost writer could have committed; directory discovery cannot supply it.
    pub async fn recover_unavailable(
        &self,
        ticket: &BuildTicket,
        private: &GenerationStore,
        worker: &impl ProjectionCatchUp,
    ) -> Result<PublishedGeneration> {
        let published = self
            .catalogue
            .active_generation()
            .await?
            .ok_or(PublicationError::GenerationMismatch)?;
        if published.pointer != ticket.expected_pointer {
            return Err(CatalogueError::PublicationConflict.into());
        }
        let recovery_cut = self
            .catalogue
            .evidence()
            .evidence_cut()
            .await
            .map_err(|error| PublicationError::Worker(error.into()))?;
        if recovery_cut < published.ready.snapshot.evidence_cut {
            return Err(PublicationError::GenerationMismatch);
        }
        let guard = self.catalogue.acquire_publication(ticket).await?;
        let result = self
            .publish_guarded(ticket, private, None, worker, &guard, Some(recovery_cut))
            .await;
        self.catalogue.release_publication(&guard).await?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn publish_guarded(
        &self,
        ticket: &BuildTicket,
        private: &GenerationStore,
        old: Option<&GenerationStore>,
        worker: &impl ProjectionCatchUp,
        guard: &PublicationGuard,
        recovery_cut: Option<u64>,
    ) -> Result<PublishedGeneration> {
        if private.manifest() != &ticket.manifest
            || !path_matches(private.path(), &self.directory.join(&ticket.relative_file))
        {
            return Err(PublicationError::GenerationMismatch);
        }
        match (&ticket.expected_pointer.generation, old) {
            (None, None) if recovery_cut.is_none() => {}
            (Some(_), None) if recovery_cut.is_some() => {}
            (Some(expected), Some(old)) if &old.manifest().generation == expected => {}
            _ => return Err(PublicationError::GenerationMismatch),
        }
        // Expensive replay/full validation runs while old projection and evidence
        // admission continue. Only the final catch-up tail enters the barrier.
        let private_token = ensure_running(private).await?;
        let background_cut = self
            .catalogue
            .evidence()
            .evidence_cut()
            .await
            .map_err(|e| PublicationError::Worker(anyhow::anyhow!(e)))?;
        worker
            .catch_up(private, private_token, background_cut)
            .await
            .map_err(PublicationError::Worker)?;
        require_cut(private, background_cut).await?;
        // Semantic validation covers this exact baseline. A concurrent same-cut
        // continuation remains in the tail instead of silently joining the baseline.
        let baseline = private.validate_for_publication().await?;
        worker
            .validate_generation(private, &baseline.checkpoint)
            .await
            .map_err(PublicationError::Worker)?;
        let _barrier = self.barrier.lock().await;
        if self.catalogue.pointer().await? != ticket.expected_pointer {
            return Err(CatalogueError::PublicationConflict.into());
        }
        let old_token = if let Some(old) = old {
            Some(ensure_running(old).await?)
        } else {
            None
        };
        let deadline = tokio::time::Instant::now() + self.limits.barrier_budget;
        let attempt = tokio::time::timeout_at(
            deadline,
            self.publish_barrier(
                ticket,
                private,
                old,
                old_token,
                private_token,
                &baseline,
                worker,
                guard,
                recovery_cut,
            ),
        )
        .await;
        match attempt {
            Ok(Ok(published)) => Ok(published),
            other => {
                // A timeout can arrive after SQL committed its pointer but before
                // the future returned. The catalogue, never the returned error,
                // decides which generation is authoritative during recovery.
                if let Some(published) = self.catalogue.active_generation().await? {
                    if published.ready.manifest.generation == ticket.manifest.generation {
                        resume_observed(private).await?;
                        return Ok(published);
                    }
                }
                if self.catalogue.pointer().await? == ticket.expected_pointer {
                    if let (Some(old), Some(token)) = (old, old_token) {
                        resume_if_owned(old, token).await?;
                    }
                }
                resume_if_owned(private, private_token).await?;
                match other {
                    Err(_) => Err(PublicationError::BarrierExpired),
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(_)) => unreachable!(),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn publish_barrier(
        &self,
        ticket: &BuildTicket,
        private: &GenerationStore,
        old: Option<&GenerationStore>,
        old_token: Option<u64>,
        private_token: u64,
        baseline: &GenerationValidation,
        worker: &impl ProjectionCatchUp,
        guard: &PublicationGuard,
        recovery_cut: Option<u64>,
    ) -> Result<PublishedGeneration> {
        let old_fence = match (old, old_token) {
            (Some(old), Some(token)) => Some(old.fence(token).await?),
            _ => None,
        };
        let old_cut = old_fence
            .as_ref()
            .map_or(recovery_cut.unwrap_or(0), |fence| {
                fence.checkpoint.evidence_cut
            });
        let publication_cut = self
            .catalogue
            .evidence()
            .evidence_cut()
            .await
            .map_err(|e| PublicationError::Worker(anyhow::anyhow!(e)))?;
        if publication_cut < old_cut {
            return Err(PublicationError::GenerationMismatch);
        }
        worker
            .catch_up(private, private_token, publication_cut)
            .await
            .map_err(PublicationError::Worker)?;
        require_cut(private, publication_cut).await?;
        let private_fence = private.fence(private_token).await?;
        let validated = private
            .validate_tail(baseline, &private_fence, self.limits.max_tail_revisions)
            .await?;
        let identity = private.seal(&private_fence, &validated).await?;
        self.catalogue
            .record_ready(
                ticket,
                &ReadyManifest {
                    manifest: ticket.manifest.clone(),
                    relative_file: ticket.relative_file.clone(),
                    database_identity: identity,
                    snapshot: private_fence.checkpoint.clone(),
                    fence_token: private_fence.token,
                },
            )
            .await?;
        #[cfg(test)]
        self.crash_at(PublicationCrash::AfterReady)?;
        let published = self.catalogue.publish(ticket, guard, old_cut).await?;
        #[cfg(test)]
        self.crash_at(PublicationCrash::AfterPointer)?;
        private.resume(&private_fence).await?;
        Ok(published)
    }

    /// Recovery trusts the evidence catalogue. Retained ready directories never
    /// win by mtime, and a missing/corrupt referenced file is an explicit failure.
    pub async fn recover_active(
        &self,
        worker: &impl ProjectionCatchUp,
    ) -> Result<Option<GenerationStore>> {
        self.catalogue.recover_publication_guard().await?;
        let Some(published) = self.catalogue.active_generation().await? else {
            return Ok(None);
        };
        let store = GenerationStore::open(
            self.directory.join(&published.ready.relative_file),
            published.ready.manifest.clone(),
            self.limits.queries.clone(),
        )
        .await?;
        let validation = store.validate_for_publication().await?;
        if validation.database_identity != published.ready.database_identity {
            return Err(PublicationError::Recovery(
                "published generation identity failed validation".into(),
            ));
        }
        store
            .validate_published_snapshot(&published.ready.snapshot)
            .await
            .map_err(|error| {
                PublicationError::Recovery(format!(
                    "published generation checkpoint failed validation: {error}"
                ))
            })?;
        worker
            .validate_generation(&store, &validation.checkpoint)
            .await
            .map_err(PublicationError::Worker)?;
        resume_observed(&store).await?;
        Ok(Some(store))
    }

    #[cfg(test)]
    fn crash_at(&self, point: PublicationCrash) -> Result<()> {
        if self.crash != Some(point) {
            return Ok(());
        }
        #[cfg(unix)]
        if std::env::var_os("SYLPH_PUBLICATION_CRASH_PROCESS").is_some() {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // SAFETY: the fixture explicitly requests killing only this test child.
            unsafe { kill(std::process::id() as i32, 9) };
            unreachable!("SIGKILL must terminate the publication probe");
        }
        Err(PublicationError::InjectedCrash)
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationCrash {
    AfterReady,
    AfterPointer,
}

async fn ensure_running(store: &GenerationStore) -> Result<u64> {
    let checkpoint = store.checkpoint().await?;
    if checkpoint.writer_mode == WriterMode::Running {
        return Ok(checkpoint.fence_token);
    }
    Ok(store
        .resume(&FenceLease {
            token: checkpoint.fence_token,
            checkpoint: checkpoint.snapshot,
        })
        .await?)
}
async fn resume_observed(store: &GenerationStore) -> Result<()> {
    ensure_running(store).await?;
    Ok(())
}
async fn resume_if_owned(store: &GenerationStore, previous_token: u64) -> Result<()> {
    let checkpoint = store.checkpoint().await?;
    if checkpoint.writer_mode != WriterMode::Running && checkpoint.fence_token == previous_token + 1
    {
        store
            .resume(&FenceLease {
                token: checkpoint.fence_token,
                checkpoint: checkpoint.snapshot,
            })
            .await?;
    }
    Ok(())
}
async fn require_cut(store: &GenerationStore, cut: u64) -> Result<()> {
    if store.checkpoint().await?.snapshot.evidence_cut != cut {
        Err(PublicationError::IncompleteCatchUp)
    } else {
        Ok(())
    }
}
fn path_matches(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(test)]
mod tests;
