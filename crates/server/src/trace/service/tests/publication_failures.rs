use super::*;
use crate::trace::{
    generation::{GenerationStore, Snapshot, WriterMode},
    publication::PublicationCrash,
    reading_store::read_scope,
};

async fn history(generation: &GenerationStore, snapshot: &Snapshot) -> Vec<serde_json::Value> {
    let owner = AgentRef::Session(SessionId::decode("env7fk2:codex:response-user").unwrap());
    let page = generation
        .read_page(
            snapshot,
            &read_scope(&owner, None, "history"),
            None,
            None,
            None,
            16,
            false,
        )
        .await
        .unwrap();
    assert!(!page.has_after && !page.has_before && !page.work_exhausted);
    page.rows.into_iter().map(|row| row.body).collect()
}

async fn exercise(point: PublicationCrash) {
    let directory = tempfile::tempdir().unwrap();
    let mut service = TraceService::open(directory.path()).await.unwrap();
    admit_native(&service).await;
    service.project_pending().await.unwrap();
    let original = service.catalogue.pointer().await.unwrap();
    let old = service.active().await.unwrap();
    let view = service.lease_guard("reader", None).await.unwrap();
    let snapshot = view.snapshot.clone();
    let before = history(&old, &snapshot).await;
    assert!(serde_json::to_string(&before)
        .unwrap()
        .contains("The transcript is available."));
    let token = view.token.clone();
    view.retain();
    drop(view);

    let append = b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"id\":\"later-answer\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Admitted before publication failure\"}]}}\n";
    admit_native_from(
        &service,
        append,
        NATIVE.split_inclusive(|byte| *byte == b'\n').count() as u64,
        NATIVE.len() as u64,
    )
    .await;
    let admitted_cut = service.evidence.evidence_cut().await.unwrap();
    assert!(admitted_cut > snapshot.evidence_cut);
    service.publication.inject_crash(Some(point));
    let result = service.rebuild().await;
    let active = service.catalogue.pointer().await.unwrap();
    if point == PublicationCrash::AfterPointer {
        assert!(
            result.is_ok(),
            "{point:?}: committed pointer must win over the interrupted return: {result:?}"
        );
        assert_ne!(active.generation, original.generation);
        let current = service.active().await.unwrap();
        assert_eq!(
            current.checkpoint().await.unwrap().snapshot.evidence_cut,
            admitted_cut
        );
        assert!(serde_json::to_string(
            &history(&current, &current.checkpoint().await.unwrap().snapshot).await
        )
        .unwrap()
        .contains("Admitted before publication failure"));
    } else {
        assert!(
            result.is_err(),
            "{point:?}: the selected failure must be reached"
        );
        assert_eq!(
            active, original,
            "{point:?}: no private generation may become visible"
        );
        assert_eq!(
            old.checkpoint().await.unwrap().snapshot.evidence_cut,
            snapshot.evidence_cut
        );
        assert_eq!(
            old.checkpoint().await.unwrap().writer_mode,
            WriterMode::Running,
            "{point:?}: a failed rebuild must resume the old writer"
        );
    }
    assert!(
        service.catalogue.pending_builds().await.unwrap().is_empty(),
        "{point:?}: a failed private build must release its job capacity"
    );
    assert_eq!(
        history(&old, &snapshot).await,
        before,
        "{point:?}: the pinned reader must preserve ordering and exact content"
    );
    let held = service.lease_guard("reader", Some(&token)).await.unwrap();
    assert_eq!(held.snapshot, snapshot);
    held.retain();
    drop(held);
    assert_eq!(service.evidence.evidence_cut().await.unwrap(), admitted_cut);

    service.publication.inject_crash(None);
    service.project_pending().await.unwrap();
    service.rebuild().await.unwrap();
    let current = service.active().await.unwrap();
    let current_snapshot = current.checkpoint().await.unwrap().snapshot;
    assert_eq!(current_snapshot.evidence_cut, admitted_cut);
    assert!(
        serde_json::to_string(&history(&current, &current_snapshot).await)
            .unwrap()
            .contains("Admitted before publication failure")
    );
    assert_eq!(history(&old, &snapshot).await, before);
    // Recovery must consult the committed catalogue, including the successful
    // pointer commit whose caller was interrupted before the candidate resumed.
    let published = service.catalogue.pointer().await.unwrap();
    close(service).await;
    let restarted = TraceService::open(directory.path()).await.unwrap();
    assert_eq!(restarted.catalogue.pointer().await.unwrap(), published);
    let current = restarted.active().await.unwrap();
    assert_eq!(
        history(&current, &current_snapshot).await,
        history(&current, &current.checkpoint().await.unwrap().snapshot).await
    );
    let held = restarted.lease_guard("reader", Some(&token)).await.unwrap();
    assert_eq!(held.snapshot, snapshot);
    let retained = restarted.generation(&snapshot.generation).await.unwrap();
    assert_eq!(history(&retained, &snapshot).await, before);
    drop(held);
    close(restarted).await;
}

#[tokio::test]
async fn failed_private_generation_initialization_releases_capacity_and_preserves_the_reader() {
    for point in [
        PublicationCrash::BeforeCreate,
        PublicationCrash::BeforeInitialValidation,
        PublicationCrash::BeforeFileRegistration,
    ] {
        exercise(point).await;
    }
}

#[tokio::test]
async fn every_rebuild_publication_failure_preserves_a_complete_readable_generation() {
    for point in [
        PublicationCrash::BeforeReplay,
        PublicationCrash::BeforeReplayCut,
        PublicationCrash::BeforeBaselineValidation,
        PublicationCrash::BeforeSemanticValidation,
        PublicationCrash::BeforeOldFence,
        PublicationCrash::BeforeTailReplay,
        PublicationCrash::BeforeTailCut,
        PublicationCrash::BeforePrivateFence,
        PublicationCrash::BeforeTailValidation,
        PublicationCrash::BeforeSeal,
        PublicationCrash::BeforeReady,
        PublicationCrash::AfterReady,
        PublicationCrash::AfterPointer,
    ] {
        exercise(point).await;
    }
}
