//! Explicit, disposable JSON exploration. Retained payload bytes remain authoritative.
pub(crate) mod index;
pub(crate) mod parser;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Mutex, RwLock, OwnedSemaphorePermit, Semaphore};
use sylph_protocol::trace::IngestionEpoch;
use super::{generation::PayloadAnchor, reading_store::ReadRow, service::LeaseGuard};

pub(crate) use sylph_protocol::trace::StructureLimits as Limits;
pub(crate) fn limits() -> Limits {
    Limits {
        max_jobs: 4,
        max_bytes: 256 * 1024 * 1024,
        max_nodes: 1_000_000,
        max_depth: 128,
        max_index_bytes: 128 * 1024 * 1024,
        chunk_bytes: 64 * 1024,
        page_items: 32,
        preview_bytes: 256,
        max_query_steps: 20_000,
        lifetime_ms: 120_000,
    }
}

pub(crate) enum State {
    Building,
    Ready(Arc<index::Index>),
    Failed(String),
}
pub(crate) struct Job {
    pub id: String,
    pub subject: String,
    pub lease: LeaseGuard,
    pub row: ReadRow,
    pub anchor: PayloadAnchor,
    pub dependencies: BTreeSet<IngestionEpoch>,
    pub total: u64,
    pub expires_at_ms: i64,
    pub deadline: Instant,
    pub cancel: Arc<AtomicBool>,
    pub bytes: AtomicU64,
    pub nodes: Arc<AtomicU64>,
    pub state: RwLock<State>,
    pub _permit: OwnedSemaphorePermit,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub(crate) struct Jobs {
    pub limits: Limits,
    pub directory: std::path::PathBuf,
    jobs: Arc<Mutex<BTreeMap<String, Arc<Job>>>>,
    permits: Arc<Semaphore>,
}
impl Jobs {
    pub async fn open(root: &std::path::Path) -> anyhow::Result<Self> {
        let directory = root.join("structures");
        // The archive lock is already held. This directory contains disposable
        // indices only; restart never resumes a half-validated structural tree.
        match tokio::fs::remove_dir_all(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tokio::fs::create_dir_all(&directory).await?;
        let limits = limits();
        Ok(Self {
            permits: Arc::new(Semaphore::new(limits.max_jobs)),
            limits,
            directory,
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn reserve(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }
    pub async fn insert(&self, job: Arc<Job>) {
        self.jobs.lock().await.insert(job.id.clone(), job.clone());
        let jobs = Arc::downgrade(&self.jobs);
        let id = job.id.clone();
        let deadline = job.deadline;
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline.into()).await;
            if let Some(jobs) = jobs.upgrade() {
                if let Some(job) = jobs.lock().await.remove(&id) {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
    }
    pub async fn get(&self, id: &str, subject: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .await
            .get(id)
            .filter(|job| {
                job.subject == subject
                    && Instant::now() < job.deadline
                    && !job.cancel.load(Ordering::Relaxed)
            })
            .cloned()
    }
    pub async fn remove(&self, id: &str, subject: &str) -> bool {
        let mut jobs = self.jobs.lock().await;
        if jobs.get(id).is_some_and(|job| job.subject == subject) {
            let job = jobs.remove(id).unwrap();
            job.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn restart_discards_only_the_disposable_structural_index_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("structures/abandoned-job")).unwrap();
        std::fs::write(
            root.path().join("structures/abandoned-job/nodes.sqlite3"),
            b"unfinished index",
        )
        .unwrap();
        std::fs::write(
            root.path().join("retained-evidence"),
            b"authoritative bytes",
        )
        .unwrap();
        let jobs = Jobs::open(root.path()).await.unwrap();
        assert!(std::fs::read_dir(&jobs.directory).unwrap().next().is_none());
        assert_eq!(
            std::fs::read(root.path().join("retained-evidence")).unwrap(),
            b"authoritative bytes"
        );
    }
}
