use super::*;
use crate::trace::evidence::catalogue::ReprojectionState;
use crate::trace::generation::WriterMode;

async fn run(service: Arc<TraceService>) {
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(service.run_reprojection())
            .unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_reprojection_admission_coalesces_and_completion_keeps_old_views() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(TraceService::open(dir.path()).await.unwrap());
    super::super::tests::admit_native(&service).await;
    service.project_pending().await.unwrap();
    let view = service.lease_guard("reader", None).await.unwrap();
    view.retain();
    let old = service.catalogue.pointer().await.unwrap();
    let (left, right) = tokio::join!(
        service.start_reprojection("admin-a"),
        service.start_reprojection("admin-b")
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left.job_id, right.job_id);
    assert_eq!(left.state, ReprojectionState::Queued);
    assert_eq!(service.catalogue.pointer().await.unwrap(), old);
    run(service.clone()).await;
    let done = service
        .catalogue
        .reprojection_job(&left.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, ReprojectionState::Completed);
    assert_eq!(done.snapshot.as_ref().unwrap().generation, done.generation);
    assert_eq!(
        service
            .catalogue
            .pointer()
            .await
            .unwrap()
            .generation
            .as_ref(),
        Some(&done.generation)
    );
    assert_eq!(
        service
            .catalogue
            .resolve_lease("reader", &view.token, crate::models::now_ms())
            .await
            .unwrap()
            .snapshot,
        view.snapshot
    );
    assert_eq!(
        service
            .cancel_reprojection(&done.job_id)
            .await
            .unwrap()
            .unwrap(),
        done
    );
    assert_eq!(
        service.catalogue.reprojection_jobs().await.unwrap(),
        vec![done.clone()]
    );
    super::super::tests::close(Arc::try_unwrap(service).ok().unwrap()).await;
    let reopened = TraceService::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened
            .catalogue
            .reprojection_job(&done.job_id)
            .await
            .unwrap()
            .unwrap(),
        done
    );
    assert_eq!(
        reopened
            .cancel_reprojection(&done.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Completed
    );
}

#[tokio::test]
async fn cancellation_during_private_replay_preserves_active_reads_and_releases_publication() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(TraceService::open(dir.path()).await.unwrap());
    let old = service.active().await.unwrap();
    let pointer = service.catalogue.pointer().await.unwrap();
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    *service.reprojection_pause.lock().await = Some((entered.clone(), resume.clone()));
    let job = service.start_reprojection("admin").await.unwrap();
    let mut task = tokio::spawn(run(service.clone()));
    entered.notified().await;
    assert_eq!(
        service
            .catalogue
            .reprojection_job(&job.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Running
    );
    // Admission and the active projection are independent of the private replay.
    super::super::tests::admit_native(&service).await;
    service.project_pending().await.unwrap();
    assert!(old.checkpoint().await.unwrap().snapshot.evidence_cut > 0);
    let view = service.lease_guard("reader", None).await.unwrap();
    assert_eq!(
        view.snapshot.generation,
        pointer.generation.clone().unwrap()
    );
    assert_eq!(
        service
            .cancel_reprojection(&job.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Cancelling
    );
    let cancelled_promptly =
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut task).await;
    let prompt = cancelled_promptly.is_ok();
    if !prompt {
        resume.notify_one();
        task.await.unwrap();
    } else {
        cancelled_promptly.unwrap().unwrap();
    }
    assert!(
        prompt,
        "cancellation must release paused private replay without its peer answering"
    );
    assert_eq!(
        service
            .catalogue
            .reprojection_job(&job.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Cancelled
    );
    assert_eq!(service.catalogue.pointer().await.unwrap(), pointer);
    assert_eq!(
        old.checkpoint().await.unwrap().writer_mode,
        WriterMode::Running
    );
    assert!(service.catalogue.pending_builds().await.unwrap().is_empty());
    let successor = service.start_reprojection("admin").await.unwrap();
    run(service.clone()).await;
    assert_eq!(
        service
            .catalogue
            .reprojection_job(&successor.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Completed
    );
}

#[tokio::test]
async fn queued_cancellation_and_restart_keep_terminal_job_status() {
    let dir = tempfile::tempdir().unwrap();
    let service = TraceService::open(dir.path()).await.unwrap();
    let cancelled = service.start_reprojection("admin").await.unwrap();
    assert_eq!(
        service
            .cancel_reprojection(&cancelled.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Cancelled
    );
    let interrupted = service.start_reprojection("admin").await.unwrap();
    service
        .catalogue
        .claim_reprojection(crate::models::now_ms())
        .await
        .unwrap();
    super::super::tests::close(service).await;
    let reopened = TraceService::open(dir.path()).await.unwrap();
    let failed = reopened
        .catalogue
        .reprojection_job(&interrupted.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, ReprojectionState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("server_restarted"));
    assert_eq!(
        reopened
            .catalogue
            .reprojection_job(&cancelled.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Cancelled
    );
    let cancelling = reopened.start_reprojection("admin").await.unwrap();
    reopened
        .catalogue
        .claim_reprojection(crate::models::now_ms())
        .await
        .unwrap();
    reopened
        .cancel_reprojection(&cancelling.job_id)
        .await
        .unwrap();
    super::super::tests::close(reopened).await;
    let reopened = TraceService::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened
            .catalogue
            .reprojection_job(&cancelling.job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ReprojectionState::Cancelled
    );
}

#[tokio::test]
async fn recent_job_listing_is_bounded_while_each_terminal_job_remains_addressable() {
    let dir = tempfile::tempdir().unwrap();
    let service = TraceService::open(dir.path()).await.unwrap();
    let mut first = None;
    for ordinal in 0..40 {
        let job = service
            .catalogue
            .admit_reprojection("admin", ordinal)
            .await
            .unwrap();
        first.get_or_insert(job.job_id.clone());
        service
            .catalogue
            .cancel_reprojection(&job.job_id, ordinal)
            .await
            .unwrap();
    }
    let jobs = service.catalogue.reprojection_jobs().await.unwrap();
    assert_eq!(jobs.len(), 32);
    assert_eq!(jobs[0].created_at_ms, 39);
    assert_eq!(jobs.last().unwrap().created_at_ms, 8);
    assert!(service
        .catalogue
        .reprojection_job(&first.unwrap())
        .await
        .unwrap()
        .is_some());
}
