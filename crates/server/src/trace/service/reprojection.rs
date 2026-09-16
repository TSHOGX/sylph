//! Reprojection runs independently from requests and the active projection writer.

use super::*;
use crate::trace::{evidence::catalogue::ReprojectionJob, publication::ProjectionCatchUp};

impl TraceService {
    pub async fn start_reprojection(&self, subject: &str) -> anyhow::Result<ReprojectionJob> {
        self.require_available().await?;
        let job = self
            .catalogue
            .admit_reprojection(subject, crate::models::now_ms())
            .await?;
        self.reprojection_wake.notify_one();
        Ok(job)
    }

    pub async fn cancel_reprojection(&self, id: &str) -> anyhow::Result<Option<ReprojectionJob>> {
        Ok(self
            .catalogue
            .cancel_reprojection(id, crate::models::now_ms())
            .await?)
    }

    pub(super) fn spawn_reprojection(self: &Arc<Self>) {
        let service = Arc::downgrade(self);
        let wake = self.reprojection_wake.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! { _=wake.notified()=>{}, _=tokio::time::sleep(std::time::Duration::from_secs(1))=>{} }
                let Some(service) = service.upgrade() else {
                    break;
                };
                let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(service.run_reprojection())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::error!(%error,"reprojection job worker failed"),
                    Err(error) => tracing::error!(%error,"reprojection task failed"),
                }
            }
        });
    }

    pub(super) async fn run_reprojection(&self) -> anyhow::Result<()> {
        let Some(job) = self
            .catalogue
            .claim_reprojection(crate::models::now_ms())
            .await?
        else {
            return Ok(());
        };
        let result = self.reproject(&job).await;
        if let Err(error) = result {
            tracing::warn!(job=%job.job_id,%error,"reprojection did not publish");
            self.catalogue
                .finish_reprojection_failure(&job.job_id, crate::models::now_ms())
                .await?;
        }
        Ok(())
    }

    async fn reproject(&self, job: &ReprojectionJob) -> anyhow::Result<()> {
        self.require_available().await?;
        let old = self.active().await?;
        let private_worker = ProjectionWorker::new(
            self.evidence.clone(),
            self.catalogue.clone(),
            WorkerLimits::default(),
        )?;
        let (ticket, private) = self
            .publication
            .begin_build(
                Self::manifest(job.generation.clone()),
                crate::models::now_ms(),
            )
            .await?;
        // Private reduction state dies with this job. The active worker reloads
        // the published checkpoint, so repeated cancelled jobs cannot accumulate
        // their full graphs in its generation cache.
        let replay = CancellableReplay {
            service: self,
            job,
            worker: private_worker,
        };
        // Do not cancel/drop the publication future: its error path owns guard
        // release and writer resumption. Only the replay callbacks are cancellable.
        let result = self
            .publication
            .publish(&ticket, &private, Some(&old), &replay)
            .await;
        if let Err(error) = result {
            self.catalogue
                .cancel_build(&ticket, &error.to_string())
                .await?;
            return Err(error.into());
        }
        self.generations
            .write()
            .await
            .insert(job.generation.clone(), Arc::new(private));
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.notify_admission();
        self.search_wake.notify_one();
        Ok(())
    }
}

struct CancellableReplay<'a> {
    service: &'a TraceService,
    job: &'a ReprojectionJob,
    worker: ProjectionWorker,
}
impl CancellableReplay<'_> {
    async fn cancellation(&self) -> anyhow::Error {
        loop {
            match self
                .service
                .catalogue
                .reprojection_cancelled(&self.job.generation)
                .await
            {
                Ok(true) => return anyhow::anyhow!("reprojection cancelled"),
                Err(error) => return error.into(),
                Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            }
        }
    }
}

#[async_trait::async_trait]
impl ProjectionCatchUp for CancellableReplay<'_> {
    async fn catch_up(
        &self,
        generation: &GenerationStore,
        token: u64,
        through: u64,
    ) -> anyhow::Result<()> {
        if self
            .service
            .catalogue
            .reprojection_cancelled(&self.job.generation)
            .await?
        {
            anyhow::bail!("reprojection cancelled");
        }
        #[cfg(test)]
        if let Some((entered, resume)) = self.service.reprojection_pause.lock().await.take() {
            entered.notify_one();
            tokio::select! { _=resume.notified()=>{}, error=self.cancellation()=>return Err(error) }
        }
        tokio::select! {
            result=self.worker.catch_up(generation,token,through)=>{result?;},
            error=self.cancellation()=>return Err(error),
        }
        self.service
            .catalogue
            .reprojection_progress(&self.job.job_id, through)
            .await?;
        Ok(())
    }

    async fn validate_generation(
        &self,
        generation: &GenerationStore,
        snapshot: &super::super::generation::Snapshot,
    ) -> anyhow::Result<()> {
        tokio::select! {
            result=ProjectionCatchUp::validate_generation(&self.worker,generation,snapshot)=>result,
            error=self.cancellation()=>Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
