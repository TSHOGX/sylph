//! Bounded collection of blobs owned exclusively by a unified trace archive.

use super::evidence::catalogue::{CatalogueError, EvidenceCatalogue};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct GcLimits {
    pub max_candidates: usize,
    pub max_epoch_checks: usize,
    pub max_root_releases: usize,
}
impl Default for GcLimits {
    fn default() -> Self {
        Self {
            max_candidates: 128,
            max_epoch_checks: 128,
            max_root_releases: 1024,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcProgress {
    pub inspected: usize,
    pub collected: usize,
    pub collected_bytes: u64,
    pub released_roots: usize,
    pub retired_exports: usize,
    pub pinned: usize,
}

#[derive(Clone)]
pub struct TraceGarbageCollector {
    catalogue: EvidenceCatalogue,
    limits: GcLimits,
    cursor: Arc<Mutex<Option<String>>>,
    #[cfg(test)]
    crash_after_unlink: bool,
    #[cfg(test)]
    before_unlink: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}
impl TraceGarbageCollector {
    /// The caller supplies a catalogue whose BlobStore root belongs exclusively
    /// to this trace archive. Legacy stores must be removed before activation.
    pub fn new(catalogue: EvidenceCatalogue, limits: GcLimits) -> Result<Self, CatalogueError> {
        if [
            limits.max_candidates,
            limits.max_epoch_checks,
            limits.max_root_releases,
        ]
        .contains(&0)
        {
            return Err(CatalogueError::Invalid("GC bounds must be positive".into()));
        }
        Ok(Self {
            catalogue,
            limits,
            cursor: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            crash_after_unlink: false,
            #[cfg(test)]
            before_unlink: None,
        })
    }

    pub async fn run_once(&self, now_ms: i64) -> Result<GcProgress, CatalogueError> {
        let guard = self.catalogue.gc_acquire().await?;
        let result = self.run_guarded(now_ms).await;
        self.catalogue.gc_release(&guard).await?;
        result
    }

    /// Startup only, after the prior collector process has stopped. Existing
    /// per-digest deleting reservations remain and are retried before new work.
    pub async fn recover_interrupted_pass(&self) -> Result<(), CatalogueError> {
        self.catalogue.gc_recover().await
    }

    async fn run_guarded(&self, now_ms: i64) -> Result<GcProgress, CatalogueError> {
        let retired_exports = self
            .catalogue
            .retire_exports(false, self.limits.max_epoch_checks)
            .await?;
        let mut cursor = self.cursor.lock().await;
        let mut progress = GcProgress {
            retired_exports,
            released_roots: self
                .catalogue
                .gc_release_deleted_roots(
                    self.limits.max_epoch_checks,
                    self.limits.max_root_releases,
                )
                .await?,
            ..Default::default()
        };
        progress.released_roots += self
            .catalogue
            .gc_release_retired_pins(now_ms, self.limits.max_root_releases)
            .await?;
        let interrupted = self
            .catalogue
            .gc_interrupted(self.limits.max_candidates)
            .await?;
        for digest in &interrupted {
            progress.inspected += 1;
            self.collect(digest, &mut progress).await?;
        }
        let remaining = self.limits.max_candidates.saturating_sub(interrupted.len());
        if remaining == 0 {
            return Ok(progress);
        }
        let candidates = self
            .catalogue
            .gc_candidates(cursor.as_deref(), remaining)
            .await?;
        for digest in &candidates {
            progress.inspected += 1;
            if self.catalogue.reserve_collection(digest).await? {
                self.collect(digest, &mut progress).await?;
            } else {
                progress.pinned += 1;
            }
            *cursor = Some(digest.clone());
        }
        if candidates.len() < remaining {
            *cursor = None;
        }
        Ok(progress)
    }

    async fn collect(&self, digest: &str, progress: &mut GcProgress) -> Result<(), CatalogueError> {
        #[cfg(test)]
        if let Some((entered, resume)) = &self.before_unlink {
            entered.notify_one();
            resume.notified().await;
        }
        let bytes = self.catalogue.gc_unlink(digest).await?;
        #[cfg(test)]
        if self.crash_after_unlink {
            #[cfg(unix)]
            if std::env::var_os("SYLPH_GC_CRASH_PROCESS").is_some() {
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                unsafe { kill(std::process::id() as i32, 9) };
                unreachable!("GC probe must die");
            }
            return Err(CatalogueError::Invalid(
                "injected crash after unlink before catalogue completion".into(),
            ));
        }
        self.catalogue.gc_complete(digest).await?;
        progress.collected += 1;
        progress.collected_bytes += bytes;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
