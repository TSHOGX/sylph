//! Shared product trace runtime: durable stores, projection, publication and read leases.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use sylph_protocol::trace::*;
use sylph_trace::mapping::{self, SemanticMapper};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, RwLock, Semaphore};

use super::{
    cas::BlobStore,
    evidence::catalogue::{CatalogueLimits, EvidenceCatalogue, ViewLease},
    evidence::{AdmissionLimits, EvidenceStore},
    gc::{GcLimits, TraceGarbageCollector},
    generation::{GenerationManifest, GenerationStore, QueryLimits},
    publication::{PublicationCoordinator, PublicationLimits},
    search::{SearchError, SearchLimits},
    upload::{CaptureUploads, UploadLimits},
    worker::{ProjectionWorker, WorkerLimits, WorkerProgress},
};
mod reprojection;
mod search;
#[cfg(test)]
type ReprojectionPause = Arc<Mutex<Option<(Arc<Notify>, Arc<Notify>)>>>;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct TraceService {
    root: PathBuf,
    _archive_lock: Arc<std::fs::File>,
    evidence: EvidenceStore,
    cursors: Arc<super::cursor::CursorCodec>,
    catalogue: EvidenceCatalogue,
    uploads: CaptureUploads,
    worker: ProjectionWorker,
    publication: PublicationCoordinator,
    generations: Arc<RwLock<BTreeMap<String, Arc<GenerationStore>>>>,
    wake: Arc<Notify>,
    projection: Arc<Mutex<()>>,
    progress: Arc<RwLock<ServiceProgress>>,
    limits: ReadLimits,
    queries: QueryLimits,
    changes: tokio::sync::watch::Sender<u64>,
    searches: Arc<RwLock<BTreeMap<String, Arc<super::search::SearchStore>>>>,
    search_wake: Arc<Notify>,
    search_gate: Arc<Mutex<()>>,
    search_limits: SearchLimits,
    search_queries: Arc<Semaphore>,
    transfers: Arc<Semaphore>,
    evidence_exports: Arc<Semaphore>,
    evidence_playbacks: Arc<Semaphore>,
    pub(crate) evidence_playback_limits: super::evidence_export::Limits,
    pub(crate) evidence_export_limits: super::evidence_export::Limits,
    transfer_limits: super::transfer::TransferLimits,
    pub(crate) structures: super::structure::Jobs,
    #[cfg(test)]
    pub(crate) transfer_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) transfer_backpressure: Arc<Notify>,
    #[cfg(test)]
    pub(crate) evidence_read_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) directory_read_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) result_catalogue_read_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) structure_scan_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) evidence_export_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) evidence_export_prepare_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) evidence_export_cleanup_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) evidence_export_payload_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) evidence_export_backpressure: Arc<Notify>,
    #[cfg(test)]
    pub(crate) structure_page_pause: ReprojectionPause,
    recovery: Arc<RwLock<Option<String>>>,
    gc: TraceGarbageCollector,
    reprojection_wake: Arc<Notify>,
    lease_cleanup_runtime: tokio::runtime::Handle,
    #[cfg(test)]
    reprojection_pause: ReprojectionPause,
    #[cfg(test)]
    pub(crate) bootstrap_response_pause: ReprojectionPause,
}

/// A newly reserved view belongs to its request until the successful response
/// retains it. Failed/cancelled requests release their reservation automatically;
/// borrowing an existing client view never transfers ownership to the request.
pub struct LeaseGuard {
    view: ViewLease,
    release: Option<LeaseRelease>,
}
impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LeaseGuard")
            .field("snapshot", &self.view.snapshot)
            .finish()
    }
}
impl std::ops::Deref for LeaseGuard {
    type Target = ViewLease;
    fn deref(&self) -> &Self::Target {
        &self.view
    }
}
impl LeaseGuard {
    pub fn retain(&self) {
        if let Some(release) = &self.release {
            release.armed.store(false, Ordering::Relaxed);
        }
    }
}
struct LeaseRelease {
    catalogue: EvidenceCatalogue,
    runtime: tokio::runtime::Handle,
    subject: String,
    token: String,
    armed: AtomicBool,
}
impl Drop for LeaseRelease {
    fn drop(&mut self) {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let catalogue = self.catalogue.clone();
        let subject = self.subject.clone();
        let token = self.token.clone();
        // Search owns a temporary current-thread runtime on a blocking worker.
        // Spawning there would lose this task when that request runtime drops:
        // Tokio shutdown does not drain spawned async work. The application's
        // runtime, captured when the trace service opens, owns lease cleanup.
        self.runtime.spawn(async move {
            if let Err(error) = catalogue.expire_lease(&subject, &token).await {
                tracing::debug!(%error,"failed request view release could not finish");
            }
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TraceServiceError {
    #[error("Trace projection is recovering from retained evidence: {0}")]
    Recovering(String),
}

#[derive(Debug, Clone, Default)]
pub struct ServiceProgress {
    pub worker: WorkerProgress,
    pub fault: Option<String>,
}

impl TraceService {
    pub async fn open(root: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(root).await?;
        // Recovery resets process-owned publication/GC guards. An advisory file
        // lock, retained for the entire service lifetime, proves no other product
        // writer can still commit into the old generation during that reset.
        let archive_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("trace.lock"))?;
        archive_lock.try_lock().map_err(|error| {
            anyhow::anyhow!("Trace archive is already open by another server: {error}")
        })?;
        let admission = AdmissionLimits::default();
        let queries = QueryLimits::default();
        let evidence = EvidenceStore::open(
            root.join("evidence.db"),
            BlobStore::new(root.join("blobs")),
            admission.clone(),
        )
        .await?;
        let cursors = Arc::new(super::cursor::CursorCodec::new(
            &evidence.cursor_key().await?,
            queries.max_key_bytes,
        )?);
        let catalogue = EvidenceCatalogue::new(evidence.clone(), CatalogueLimits::default())?;
        while catalogue.retire_exports(true, 32).await? > 0 {}
        let uploads = CaptureUploads::new(catalogue.clone(), UploadLimits::default())?;
        let worker =
            ProjectionWorker::new(evidence.clone(), catalogue.clone(), WorkerLimits::default())?;
        let publication = PublicationCoordinator::new(
            catalogue.clone(),
            root.to_path_buf(),
            PublicationLimits {
                queries: queries.clone(),
                ..PublicationLimits::default()
            },
        )?;
        let limits = ReadLimits {
            max_items: queries.max_items,
            max_summary_bytes: queries.max_summary_bytes,
            max_preview_bytes: queries.max_preview_bytes,
            max_payload_bytes: admission.max_chunk_bytes,
            max_change_items: queries.max_batch_changes,
            max_mounted_rows: 200,
            max_metadata_cache_bytes: 16 * 1024 * 1024,
            max_payload_cache_bytes: 32 * 1024 * 1024,
            max_hidden_locations: 10,
            max_visible_interests: 16,
        };
        let (changes, _) = tokio::sync::watch::channel(0);
        let search_limits = SearchLimits::default();
        let gc = TraceGarbageCollector::new(catalogue.clone(), GcLimits::default())?;
        let transfer_limits = super::transfer::TransferLimits::default();
        let evidence_export_limits = super::evidence_export::Limits::default();
        let evidence_playback_limits = super::evidence_export::Limits::playback();
        let service = Self {
            root: root.into(),
            _archive_lock: Arc::new(archive_lock),
            evidence,
            cursors,
            catalogue,
            uploads,
            worker,
            publication,
            generations: Arc::new(RwLock::new(BTreeMap::new())),
            wake: Arc::new(Notify::new()),
            projection: Arc::new(Mutex::new(())),
            progress: Arc::new(RwLock::new(ServiceProgress::default())),
            limits,
            queries,
            changes,
            searches: Arc::new(RwLock::new(BTreeMap::new())),
            search_wake: Arc::new(Notify::new()),
            search_gate: Arc::new(Mutex::new(())),
            transfers: Arc::new(Semaphore::new(transfer_limits.max_jobs)),
            evidence_exports: Arc::new(Semaphore::new(evidence_export_limits.max_jobs)),
            evidence_export_limits,
            evidence_playbacks: Arc::new(Semaphore::new(evidence_playback_limits.max_jobs)),
            evidence_playback_limits,
            transfer_limits,
            structures: super::structure::Jobs::open(root).await?,
            #[cfg(test)]
            transfer_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            transfer_backpressure: Arc::new(Notify::new()),
            #[cfg(test)]
            evidence_read_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            directory_read_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            result_catalogue_read_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            structure_scan_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            evidence_export_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            evidence_export_prepare_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            evidence_export_cleanup_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            evidence_export_payload_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            evidence_export_backpressure: Arc::new(Notify::new()),
            #[cfg(test)]
            structure_page_pause: Arc::new(Mutex::new(None)),
            search_queries: Arc::new(Semaphore::new(search_limits.max_concurrent_queries)),
            search_limits,
            recovery: Arc::new(RwLock::new(None)),
            gc,
            reprojection_wake: Arc::new(Notify::new()),
            lease_cleanup_runtime: tokio::runtime::Handle::current(),
            #[cfg(test)]
            reprojection_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            bootstrap_response_pause: Arc::new(Mutex::new(None)),
        };
        service
            .catalogue
            .recover_reprojection_jobs(crate::models::now_ms())
            .await?;
        service.uploads.recover_interrupted_writes().await?;
        service.gc.recover_interrupted_pass().await?;
        // Abandoned private work is disposable, never an alternative active
        // generation. Cancel it before a new rebuild can consume the job bound.
        for ticket in service.catalogue.pending_builds().await? {
            service
                .catalogue
                .cancel_build(&ticket, "startup discarded unpublished work")
                .await?;
        }
        // Projection files are disposable and may have an older row/security
        // contract. Reject the manifest before opening those files for reads.
        let contract_changed =
            service
                .catalogue
                .active_generation()
                .await?
                .is_some_and(|published| {
                    published.ready.manifest
                        != Self::manifest(published.ready.manifest.generation.clone())
                });
        let recovered = if contract_changed {
            Err(super::publication::PublicationError::Recovery(
                "Published projection contract differs from this server".into(),
            ))
        } else {
            service.publication.recover_active(&service.worker).await
        };
        match recovered {
            Ok(Some(active)) => {
                service
                    .generations
                    .write()
                    .await
                    .insert(active.manifest().generation.clone(), Arc::new(active));
            }
            Ok(None) => {
                service.rebuild().await?;
            }
            Err(error @ super::publication::PublicationError::Catalogue(_)) => {
                return Err(error.into());
            }
            Err(error) => {
                let message = format!("{error:#}");
                if let Some(generation) = service.catalogue.pointer().await?.generation {
                    service
                        .catalogue
                        .cancel_generation_views(&generation)
                        .await?;
                }
                tracing::error!(%error, "published trace unavailable; rebuilding from retained evidence");
                *service.recovery.write().await = Some(message.clone());
                service.progress.write().await.fault = Some(message);
                service.notify_admission();
            }
        }
        Ok(service)
    }

    pub fn evidence(&self) -> &EvidenceStore {
        &self.evidence
    }

    pub(crate) fn cursors(&self) -> &super::cursor::CursorCodec {
        &self.cursors
    }
    pub fn catalogue(&self) -> &EvidenceCatalogue {
        &self.catalogue
    }
    pub fn uploads(&self) -> &CaptureUploads {
        &self.uploads
    }
    pub fn worker(&self) -> &ProjectionWorker {
        &self.worker
    }
    pub fn limits(&self) -> &ReadLimits {
        &self.limits
    }
    pub fn notify_access_changed(&self) {
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub fn notify_admission(&self) {
        self.wake.notify_one();
    }
    pub async fn progress(&self) -> ServiceProgress {
        self.progress.read().await.clone()
    }
    pub fn try_search_permit(&self) -> Result<OwnedSemaphorePermit, SearchError> {
        self.search_queries
            .clone()
            .try_acquire_owned()
            .map_err(|_| SearchError::Limit("concurrent search queries"))
    }

    pub async fn require_available(&self) -> anyhow::Result<()> {
        if let Some(reason) = self.recovery.read().await.as_ref() {
            return Err(TraceServiceError::Recovering(reason.clone()).into());
        }
        Ok(())
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn spawn(self: &Arc<Self>) {
        self.spawn_reprojection();
        let weak = Arc::downgrade(self);
        let wake = self.wake.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=wake.notified()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(1))=>{}}
                let Some(service) = weak.upgrade() else {
                    break;
                };
                let projected = service.clone();
                let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(projected.project_pending())
                })
                .await
                .unwrap_or_else(|error| Err(error.into()));
                if let Err(error) = result {
                    let message = format!("{error:#}");
                    let mut progress = service.progress.write().await;
                    if progress.fault.as_ref() == Some(&message) {
                        tracing::debug!(error=%error,"trace projection remains stalled");
                    } else {
                        tracing::error!(error=%error,"trace projection stalled; evidence remains retained");
                    }
                    progress.fault = Some(message);
                }
            }
        });
        let weak = Arc::downgrade(self);
        let wake = self.search_wake.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {_=wake.notified()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(5))=>{}}
                let Some(service) = weak.upgrade() else {
                    break;
                };
                // One serialized search pass owns tokenization and source scanning on
                // a blocking worker; it never occupies an async request executor.
                match tokio::task::spawn_blocking(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(service.index_pending())
                })
                .await
                {
                    Ok(Ok(work)) => tracing::debug!(?work, "trace search checkpoint reconciled"),
                    Ok(Err(error)) => tracing::debug!(%error,"trace search remains indexing"),
                    Err(error) => tracing::error!(%error,"trace search worker failed"),
                }
            }
        });
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut last_fault = None;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        let now = crate::models::now_ms();
                        service.retire_searches(now).await?;
                        Ok(service.gc.run_once(now).await?)
                    })
                })
                .await
                .unwrap_or_else(|error| Err(error.into()));
                match result {
                    Ok(progress) => {
                        tracing::debug!(?progress, "trace collection pass completed");
                        last_fault = None;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if last_fault.as_ref() == Some(&message) {
                            tracing::debug!(%error, "trace collection remains stalled");
                        } else {
                            tracing::error!(%error, "trace collection stalled");
                        }
                        last_fault = Some(message);
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub(crate) async fn hold_fixture_projection(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.projection.clone().lock_owned().await
    }

    pub async fn project_pending(&self) -> anyhow::Result<()> {
        let _gate = self.projection.lock().await;
        if self.recovery.read().await.is_some() {
            self.rebuild_locked(true).await?;
            *self.recovery.write().await = None;
            self.progress.write().await.fault = None;
        }
        if !self.catalogue.active_visibility_is_current().await? {
            // Deletion changes membership without advancing admission. Replaying a
            // private generation removes suppressed objects and search membership
            // atomically while old leased snapshots keep their visibility checks.
            self.rebuild_locked(false).await?;
        }
        let generation = self.active().await?;
        let checkpoint = generation.checkpoint().await?;
        let cut = self.evidence.evidence_cut().await?;
        if cut > checkpoint.snapshot.evidence_cut
            || self.worker.has_continuations(&generation).await?
        {
            let worker = self
                .worker
                .catch_up(&generation, checkpoint.fence_token, cut)
                .await?;
            *self.progress.write().await = ServiceProgress {
                worker,
                fault: None,
            };
            self.changes
                .send_modify(|revision| *revision = revision.wrapping_add(1));
            self.search_wake.notify_one();
        }
        Ok(())
    }

    pub async fn active(&self) -> anyhow::Result<Arc<GenerationStore>> {
        self.require_available().await?;
        let pointer = self.catalogue.pointer().await?;
        let generation = pointer
            .generation
            .ok_or_else(|| anyhow::anyhow!("No trace generation is published"))?;
        self.generation(&generation).await
    }

    pub async fn generation(&self, generation: &str) -> anyhow::Result<Arc<GenerationStore>> {
        self.require_available().await?;
        if let Some(store) = self.generations.read().await.get(generation) {
            self.require_generation_contract(store.manifest()).await?;
            return Ok(store.clone());
        }
        let mut generations = self.generations.write().await;
        if let Some(store) = generations.get(generation) {
            self.require_generation_contract(store.manifest()).await?;
            return Ok(store.clone());
        }
        let ready = self.catalogue.ready_generation(generation).await?;
        self.require_generation_contract(&ready.manifest).await?;
        let store = Arc::new(
            GenerationStore::open(
                self.root.join(&ready.relative_file),
                ready.manifest,
                self.queries.clone(),
            )
            .await?,
        );
        generations.insert(generation.into(), store.clone());
        Ok(store)
    }

    async fn require_generation_contract(
        &self,
        manifest: &GenerationManifest,
    ) -> anyhow::Result<()> {
        if manifest != &Self::manifest(manifest.generation.clone()) {
            // Retained views can outlive an active contract change. Reject them
            // before opening an older row layout, including already cached stores.
            self.catalogue
                .cancel_generation_views(&manifest.generation)
                .await?;
            return Err(super::evidence::catalogue::CatalogueError::SnapshotExpired.into());
        }
        Ok(())
    }

    pub async fn lease_guard(
        &self,
        subject: &str,
        token: Option<&str>,
    ) -> anyhow::Result<LeaseGuard> {
        self.require_available().await?;
        let now = crate::models::now_ms();
        if let Some(token) = token {
            let view = self.catalogue.resolve_lease(subject, token, now).await?;
            self.generation(&view.snapshot.generation).await?;
            return Ok(LeaseGuard {
                view,
                release: None,
            });
        }
        let pending = self.catalogue.begin_lease(subject, now, 120_000).await?;
        let release = self.pending_release(subject, &pending.lease_id);
        let generation = self.generation(&pending.generation).await?;
        let checkpoint = generation.checkpoint().await?;
        let view = self
            .catalogue
            .finish_lease(&pending, &checkpoint.snapshot, now)
            .await?;
        Ok(LeaseGuard {
            view,
            release: Some(release),
        })
    }

    pub async fn lease_at_snapshot(
        &self,
        subject: &str,
        snapshot: &super::generation::Snapshot,
    ) -> anyhow::Result<LeaseGuard> {
        self.require_available().await?;
        self.generation(&snapshot.generation).await?;
        let now = crate::models::now_ms();
        let pending = self.catalogue.begin_lease(subject, now, 120_000).await?;
        let release = self.pending_release(subject, &pending.lease_id);
        let view = self.catalogue.finish_lease(&pending, snapshot, now).await?;
        Ok(LeaseGuard {
            view,
            release: Some(release),
        })
    }

    pub fn transfer_limits(&self) -> &super::transfer::TransferLimits {
        &self.transfer_limits
    }
    pub fn transfer_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.transfers.clone().try_acquire_owned().ok()
    }
    pub(crate) fn evidence_export_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.evidence_exports.clone().try_acquire_owned().ok()
    }
    pub(crate) fn evidence_playback_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.evidence_playbacks.clone().try_acquire_owned().ok()
    }
    pub async fn transfer_lease(
        &self,
        subject: &str,
        token: &str,
        lifetime_ms: i64,
    ) -> anyhow::Result<LeaseGuard> {
        self.require_available().await?;
        let view = self
            .catalogue
            .fork_lease(subject, token, crate::models::now_ms(), lifetime_ms)
            .await?;
        let release = self.pending_release(subject, &view.token);
        self.generation(&view.snapshot.generation).await?;
        Ok(LeaseGuard {
            view,
            release: Some(release),
        })
    }

    pub(crate) async fn evidence_export_lease(
        &self,
        subject: &str,
        token: &str,
        lifetime_ms: i64,
    ) -> anyhow::Result<LeaseGuard> {
        self.require_available().await?;
        // The export authorizes its environment, not whichever other metadata
        // windows the initiating reader happened to bind to the same view.
        let view = self
            .catalogue
            .fork_window_lease(subject, token, crate::models::now_ms(), lifetime_ms)
            .await?;
        let release = self.pending_release(subject, &view.token);
        self.generation(&view.snapshot.generation).await?;
        Ok(LeaseGuard {
            view,
            release: Some(release),
        })
    }

    pub async fn window_lease(&self, subject: &str, token: &str) -> anyhow::Result<LeaseGuard> {
        self.require_available().await?;
        let view = self
            .catalogue
            .fork_window_lease(subject, token, crate::models::now_ms(), 120_000)
            .await?;
        let release = self.pending_release(subject, &view.token);
        self.generation(&view.snapshot.generation).await?;
        Ok(LeaseGuard {
            view,
            release: Some(release),
        })
    }

    pub async fn release_lease(&self, subject: &str, token: &str) -> anyhow::Result<()> {
        self.catalogue.expire_lease(subject, token).await?;
        Ok(())
    }

    fn pending_release(&self, subject: &str, token: &str) -> LeaseRelease {
        LeaseRelease {
            catalogue: self.catalogue.clone(),
            runtime: self.lease_cleanup_runtime.clone(),
            subject: subject.into(),
            token: token.into(),
            armed: AtomicBool::new(true),
        }
    }

    pub async fn rebuild(&self) -> anyhow::Result<String> {
        let _gate = self.projection.lock().await;
        self.require_available().await?;
        self.rebuild_locked(false).await
    }

    async fn rebuild_locked(&self, recovery: bool) -> anyhow::Result<String> {
        let generation = format!("trace-{}", uuid::Uuid::new_v4());
        let (ticket, private) = self
            .publication
            .begin_build(Self::manifest(generation.clone()), crate::models::now_ms())
            .await?;
        let result = if recovery {
            self.publication
                .recover_unavailable(&ticket, &private, &self.worker)
                .await
        } else {
            let old = if self.catalogue.pointer().await?.generation.is_some() {
                Some(self.active().await?)
            } else {
                None
            };
            self.publication
                .publish(&ticket, &private, old.as_deref(), &self.worker)
                .await
        };
        if let Err(error) = result {
            self.catalogue
                .cancel_build(&ticket, &error.to_string())
                .await?;
            return Err(error.into());
        }
        self.generations
            .write()
            .await
            .insert(generation.clone(), Arc::new(private));
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.notify_admission();
        self.search_wake.notify_one();
        Ok(generation)
    }

    fn manifest(generation: String) -> GenerationManifest {
        GenerationManifest {
            generation: generation.clone(),
            semantic_schema: "trace-22".into(),
            mapper_builds: [
                ("server", mapping::server::ServerMapper.build()),
                ("codex", mapping::codex::CodexMapper.build()),
                ("claude", mapping::claude::ClaudeMapper.build()),
                ("pi", mapping::pi::PiMapper.build()),
                ("inventory", mapping::inventory::InventoryMapper.build()),
                ("attachment", mapping::attachment::AttachmentMapper.build()),
                (
                    "observation",
                    mapping::observation::ObservationMapper.build(),
                ),
            ]
            .into_iter()
            .map(|(vendor, build)| (vendor.into(), build.into()))
            .collect(),
            reducer_build: "trace-reducer-2".into(),
            capture_inventory_build: "source-inventory-3".into(),
            encoding_build: "typed-blake3-1".into(),
        }
    }
}
